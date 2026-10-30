//! Runs a CPU-heavy job off the main thread in the browser build.
//!
//! A browser build with threads (`benilla-lambo/web`, `build.mjs`) has a small pool of workers
//! sharing the game's memory. The host installs it with [`set_runner`], and [`run`] hands a job to
//! it and waits for the result a frame at a time, so a job of several hundred milliseconds (a
//! terrain tile's collision tree) stalls no frame. Without a runner, and natively, the job runs
//! where it is called; the browser build without threads waits its frame's turn
//! ([`crate::frame_budget`]) first.

#[cfg(target_arch = "wasm32")]
mod pool {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::task::{Context, Poll};

    /// A unit of work for a pool thread.
    pub type Job = Box<dyn FnOnce() + Send + 'static>;

    static RUNNER: OnceLock<fn(Job)> = OnceLock::new();

    /// Install the pool: `run` must start the job on another thread without blocking.
    pub fn set_runner(run: fn(Job)) {
        let _ = RUNNER.set(run);
    }

    pub fn threaded() -> bool {
        RUNNER.get().is_some()
    }

    /// A job running on the pool; ready once it has stored its result.
    pub struct Offloaded<T> {
        job: Option<Box<dyn FnOnce() -> T + Send + 'static>>,
        slot: Arc<Mutex<Option<T>>>,
    }

    impl<T: Send + 'static> Offloaded<T> {
        pub fn new(job: impl FnOnce() -> T + Send + 'static) -> Self {
            Self {
                job: Some(Box::new(job)),
                slot: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl<T: Send + 'static> Future for Offloaded<T> {
        type Output = T;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
            if let (Some(job), Some(run)) = (self.job.take(), RUNNER.get()) {
                let slot = self.slot.clone();
                run(Box::new(move || {
                    let value = job();
                    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(value);
                }));
            }
            // `try_lock`, never `lock`: this polls on the page's thread, where a contended lock's
            // `Atomics.wait` traps. A slot the worker is filling right now reads as not ready.
            let done = match self.slot.try_lock() {
                Ok(mut slot) => slot.take(),
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().take(),
                Err(std::sync::TryLockError::WouldBlock) => None,
            };
            match done {
                Some(value) => Poll::Ready(value),
                None => {
                    // The worker cannot wake this task (a task is woken on its own thread), so
                    // look again next frame.
                    crate::frame_budget::park(cx.waker());
                    Poll::Pending
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use pool::{set_runner, Job};

/// Run `job` off the main thread where the host has a pool, else here.
pub async fn run<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> T {
    #[cfg(target_arch = "wasm32")]
    {
        if pool::threaded() {
            return pool::Offloaded::new(job).await;
        }
        crate::frame_budget::turn().await;
    }
    job()
}
