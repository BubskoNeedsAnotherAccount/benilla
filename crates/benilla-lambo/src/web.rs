//! The browser build's seam. The page lets the player pick their `Data` folder and keeps the
//! files in an I/O worker (`web/io-worker.js`); this module reads them through
//! `globalThis.__benillaIo` and mounts them as the virtual `Data` folder
//! ([`benilla_formats::virtual_data`]), so the engine opens its patch chain as it does from a disk.
//!
//! The engine reads archives synchronously, and a browser's main thread cannot block on a file.
//! The worker does the read and hands the bytes over a `SharedArrayBuffer`, which `read` waits on
//! by polling (`web/io-bridge.js`). Reads go through a chunk cache so a sector-sized read does not
//! cost a round trip.

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use benilla_formats::virtual_data::{Handle, Source};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    /// How many files the player picked.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaIo"], js_name = count)]
    fn io_count() -> u32;
    /// The `i`th picked file's name.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaIo"], js_name = name)]
    fn io_name(i: u32) -> String;
    /// The `i`th picked file's size in bytes.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaIo"], js_name = size)]
    fn io_size(i: u32) -> f64;
    /// Fill `out` from the `i`th file at `offset`; the bytes read, or -1.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaIo"], js_name = read)]
    fn io_read(i: u32, offset: f64, out: &mut [u8]) -> i32;
    /// A file the page fetched ahead of the start (the car, its sounds), by path.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaIo"], js_name = asset)]
    fn io_asset(path: &str) -> Option<Vec<u8>>;
    /// Tell the engine-sound worker the game's state (`web/synth-bridge.js`).
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaSynth"], js_name = params)]
    fn synth_params(rpm: f32, throttle: f32, volume: f32, speed: f32, shifting: bool);
    /// Set the engine's master volume, 0 to 1.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaSynth"], js_name = master)]
    fn synth_master(volume: f32);
    /// Start or stop the engine sound's worker and audio.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaSynth"], js_name = enable)]
    fn synth_enable(on: bool);
}

/// Start or stop the engine sound: off, its worker is stopped and costs nothing.
pub fn set_synth_enabled(on: bool) {
    synth_enable(on);
}

/// Send the engine sound the car's state. An audio worklet plays what the worker renders from it,
/// on the audio thread, so this is all the game has to do and it need not keep pace with frames.
pub fn push_synth(p: &benilla_lambo_synth::Params) {
    synth_params(p.rpm, p.throttle, p.volume, p.speed, p.shifting);
}

/// Set the engine's master volume, 0 to 1 as an amplitude.
pub fn set_synth_master(volume: f32) {
    synth_master(volume);
}

/// The bytes of a file the page fetched ahead of the start, if it did.
pub fn asset(path: &str) -> Option<Vec<u8>> {
    io_asset(path)
}

/// One cache chunk: 128 KiB, a few sectors of a typical MPQ file.
const CHUNK: u64 = 128 * 1024;
/// Chunks kept: 64 MiB.
const CHUNKS_KEPT: usize = 512;

#[derive(Default)]
struct Cache {
    /// `(file, chunk index)` to the chunk's bytes and the tick it was last used.
    chunks: HashMap<(u32, u64), (Arc<[u8]>, u64)>,
    tick: u64,
}

/// One picked file, read through the worker.
struct PickedFile {
    index: u32,
    size: u64,
    cache: Arc<Mutex<Cache>>,
}

impl PickedFile {
    fn chunk(&self, n: u64) -> io::Result<Arc<[u8]>> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.tick += 1;
        let tick = cache.tick;
        if let Some((bytes, used)) = cache.chunks.get_mut(&(self.index, n)) {
            *used = tick;
            return Ok(bytes.clone());
        }
        let start = n * CHUNK;
        let len = CHUNK.min(self.size.saturating_sub(start)) as usize;
        let mut buf = vec![0u8; len];
        read_exact_js(self.index, start, &mut buf)?;
        let bytes: Arc<[u8]> = buf.into();
        if cache.chunks.len() >= CHUNKS_KEPT {
            let oldest = cache
                .chunks
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| *k);
            if let Some(k) = oldest {
                cache.chunks.remove(&k);
            }
        }
        cache.chunks.insert((self.index, n), (bytes.clone(), tick));
        Ok(bytes)
    }
}

fn read_exact_js(index: u32, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    let got = io_read(index, offset as f64, buf);
    if got < 0 || got as usize != buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "reading picked file {index} at {offset}: got {got} of {}",
                buf.len()
            ),
        ));
    }
    Ok(())
}

struct PickedHandle {
    file: PickedFile,
    pos: u64,
}

impl Read for PickedHandle {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.file.size {
            return Ok(0);
        }
        let want = (out.len() as u64).min(self.file.size - self.pos) as usize;
        // A read of a chunk or more skips the cache: it would only evict.
        if want as u64 >= CHUNK {
            read_exact_js(self.file.index, self.pos, &mut out[..want])?;
            self.pos += want as u64;
            return Ok(want);
        }
        let n = self.pos / CHUNK;
        let chunk = self.file.chunk(n)?;
        let at = (self.pos - n * CHUNK) as usize;
        let take = want.min(chunk.len() - at);
        out[..take].copy_from_slice(&chunk[at..at + take]);
        self.pos += take as u64;
        Ok(take)
    }
}

impl Seek for PickedHandle {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.file.size.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        self.pos = pos;
        Ok(pos)
    }
}

/// A picked file as an archive's byte source.
struct PickedSource {
    index: u32,
    size: u64,
    cache: Arc<Mutex<Cache>>,
}

impl Source for PickedSource {
    fn open(&self) -> io::Result<Box<dyn Handle>> {
        Ok(Box::new(PickedHandle {
            file: PickedFile {
                index: self.index,
                size: self.size,
                cache: self.cache.clone(),
            },
            pos: 0,
        }))
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.size)
    }
}

/// Mount the picked files as the virtual `Data` folder. Call it before anything resolves the install.
pub fn mount_picked_files() {
    console_error_panic_hook::set_once();
    let cache = Arc::new(Mutex::new(Cache::default()));
    let archives = (0..io_count())
        .map(|i| {
            let source: Arc<dyn Source> = Arc::new(PickedSource {
                index: i,
                size: io_size(i) as u64,
                cache: cache.clone(),
            });
            (io_name(i), source)
        })
        .collect();
    benilla_formats::virtual_data::mount(archives);
}

/// Whether this is a worker, not the page. A threaded build runs `main` in every instance of the
/// module, and only the page's is the game.
pub fn on_worker() -> bool {
    #[wasm_bindgen(
        inline_js = "export function on_worker() { return typeof document === 'undefined'; }"
    )]
    extern "C" {
        fn on_worker() -> bool;
    }
    on_worker()
}

/// Start the thread pool, if this build has threads and the page allows them (`web/build.mjs`).
pub fn start_threads() {
    #[cfg(target_feature = "atomics")]
    pool::start();
}

/// The jobs [`benilla_assets::offload`] hands off, run by workers sharing this module's memory.
#[cfg(target_feature = "atomics")]
mod pool {
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex};

    use benilla_assets::offload::{set_runner, Job};
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        /// Start the page's workers on this module and memory (`web/thread-bridge.js`); how many.
        #[wasm_bindgen(js_namespace = ["globalThis", "__benillaThreads"], js_name = spawn)]
        fn spawn(module: JsValue, memory: JsValue) -> u32;
    }

    static QUEUE: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());
    static READY: Condvar = Condvar::new();

    /// Called on the page's thread, which a browser forbids to block (`Atomics.wait` traps there):
    /// a contended `lock` would kill the calling task and its load. A worker holds the queue only
    /// to pop, so a short spin on `try_lock` is the wait.
    fn submit(job: Job) {
        let mut queue = loop {
            match QUEUE.try_lock() {
                Ok(q) => break q,
                Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => std::hint::spin_loop(),
            }
        };
        queue.push_back(job);
        drop(queue);
        READY.notify_one();
    }

    pub fn start() {
        if spawn(wasm_bindgen::module(), wasm_bindgen::memory()) > 0 {
            set_runner(submit);
        }
    }

    /// A worker's whole life: take a job, run it, wait for the next.
    #[wasm_bindgen]
    pub fn thread_entry() {
        loop {
            let job = {
                let mut queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
                loop {
                    if let Some(job) = queue.pop_front() {
                        break job;
                    }
                    queue = READY.wait(queue).unwrap_or_else(|p| p.into_inner());
                }
            };
            job();
        }
    }
}

/// The allocator of a threaded build. std's takes a lock that waits (`memory.atomic.wait`), which a
/// browser forbids on the page's thread, and a pool thread allocating at the wrong moment would
/// trap it. This one spins: allocations are short, so the wait is not.
#[cfg(target_feature = "atomics")]
mod spin_alloc {
    use std::alloc::{GlobalAlloc, Layout};
    use std::cell::UnsafeCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    use dlmalloc::Dlmalloc;

    pub struct SpinDlmalloc {
        locked: AtomicBool,
        heap: UnsafeCell<Dlmalloc>,
    }

    // SAFETY: `heap` is only touched while `locked` is held.
    unsafe impl Sync for SpinDlmalloc {}

    impl SpinDlmalloc {
        pub const fn new() -> Self {
            Self {
                locked: AtomicBool::new(false),
                heap: UnsafeCell::new(Dlmalloc::new()),
            }
        }

        fn with<R>(&self, f: impl FnOnce(&mut Dlmalloc) -> R) -> R {
            while self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                std::hint::spin_loop();
            }
            // SAFETY: the lock is held.
            let out = f(unsafe { &mut *self.heap.get() });
            self.locked.store(false, Ordering::Release);
            out
        }
    }

    unsafe impl GlobalAlloc for SpinDlmalloc {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            self.with(|h| unsafe { h.malloc(l.size(), l.align()) })
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            self.with(|h| unsafe { h.calloc(l.size(), l.align()) })
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            self.with(|h| unsafe { h.free(p, l.size(), l.align()) })
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            self.with(|h| unsafe { h.realloc(p, l.size(), l.align(), new) })
        }
    }

    #[global_allocator]
    static ALLOC: SpinDlmalloc = SpinDlmalloc::new();
}
