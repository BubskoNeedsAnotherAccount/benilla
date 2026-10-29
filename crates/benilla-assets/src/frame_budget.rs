//! Spreads a burst of asset work across frames in the browser build.
//!
//! A browser has one thread, and Bevy runs a spawned task to completion on it once the frame ends.
//! A streamed burst (a tile's models, its colliders) therefore drains as one long stall. A heavy
//! task awaits [`turn`] first; once [`BUDGET`] of work has run since the frame began, the rest park
//! until the next frame's [`new_frame`] wakes them. A task already running is never cut short, so
//! the stall is bounded by the budget plus the longest single task. Native builds have threads and
//! pass straight through.

#[cfg(target_arch = "wasm32")]
mod gate {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;
    use web_time::Instant;

    /// Work let through per frame before the rest wait: a unit or two, so a burst spreads thin and
    /// the longest single unit sets the hitch.
    const BUDGET: Duration = Duration::from_millis(2);

    struct Gate {
        /// When this frame's first task was let through.
        opened: Option<Instant>,
        parked: Vec<Waker>,
    }

    static GATE: Mutex<Gate> = Mutex::new(Gate {
        opened: None,
        parked: Vec::new(),
    });

    pub struct Turn;

    impl Future for Turn {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            let mut gate = GATE.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            match gate.opened {
                None => gate.opened = Some(now),
                Some(at) if now.duration_since(at) >= BUDGET => {
                    gate.parked.push(cx.waker().clone());
                    return Poll::Pending;
                }
                Some(_) => {}
            }
            Poll::Ready(())
        }
    }

    pub fn park(waker: &Waker) {
        GATE.lock()
            .unwrap_or_else(|p| p.into_inner())
            .parked
            .push(waker.clone());
    }

    pub fn new_frame() {
        let parked = {
            let mut gate = GATE.lock().unwrap_or_else(|p| p.into_inner());
            gate.opened = None;
            std::mem::take(&mut gate.parked)
        };
        for waker in parked {
            waker.wake();
        }
    }
}

/// Wait for this frame's share of asset work; returns at once on a native build.
pub async fn turn() {
    #[cfg(target_arch = "wasm32")]
    gate::Turn.await;
}

/// Wake `waker` at the next frame, for a task that must look again then.
#[cfg(target_arch = "wasm32")]
pub(crate) fn park(waker: &std::task::Waker) {
    gate::park(waker);
}

/// Open the next frame's budget and wake the tasks that waited. Runs once per frame.
pub fn new_frame() {
    #[cfg(target_arch = "wasm32")]
    gate::new_frame();
}
