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
    /// Tell the engine-sound worker the game's state and the output rate (`web/synth-bridge.js`).
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaSynth"], js_name = params)]
    fn synth_params(rpm: f32, throttle: f32, volume: f32, speed: f32, shifting: bool, rate: u32);
    /// Fill `out` with the samples the worker has rendered; how many it had.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaSynth"], js_name = read)]
    fn synth_read(out: &mut [f32]) -> u32;
}

/// The engine sound for the next `out.len()` frames: the state goes to the worker and the samples
/// it has rendered come back, the rest of `out` untouched (the caller silences it).
pub fn pull_synth(p: &benilla_lambo_synth::Params, rate: u32, out: &mut [f32]) -> usize {
    synth_params(p.rpm, p.throttle, p.volume, p.speed, p.shifting, rate);
    synth_read(out) as usize
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
