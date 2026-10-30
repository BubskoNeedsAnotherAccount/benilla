//! The streaming systems' shared frame budget: spawning tiles, placements, cells, model meshes
//! and colliders each take a fixed slice of the frame, and together on a heavy frame (walking up
//! to a city) the slices overrun it. [`StreamBudget`] scales every slice by how the last frames
//! ran against the recent baseline, so a streaming burst gives way before it shows as a hitch and
//! takes the whole slice again once frames are calm. The loading cover (unpaced focus) bypasses
//! the caps entirely, as before. `WOW_STREAM_BUDGET=off` pins the scale at 1 for an A/B run.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::{Real, TimeSystems};

/// The smallest scale: streaming always keeps this share of its base slice.
const MIN_SCALE: f32 = 0.25;
/// How much of the previous scale a frame over the threshold keeps.
const BACKOFF: f32 = 0.5;
/// Scale regained per calm frame, so a full recovery from the floor takes ~15 frames.
const RECOVER: f32 = 0.05;
/// Weight of the newest frame in the short average, which smooths the 26 ms / 8 ms mis-split of a
/// pipelined renderer ([`crate::frame_pace`]) into one figure.
const FAST_ALPHA: f32 = 0.5;
/// Weight of a calm frame in the baseline, about a second at 60 Hz.
const SLOW_ALPHA: f32 = 0.02;
/// A short average past `baseline × OVER` plus [`OVER_SLACK_MS`] is a hitch.
const OVER: f32 = 1.25;
const OVER_SLACK_MS: f32 = 2.0;
/// Frames past this multiple of the baseline are stalls elsewhere and do not move the baseline.
const BASELINE_GATE: f32 = 1.5;

/// The scale the streaming budgets share this frame, in `[MIN_SCALE, 1]`.
#[derive(Resource)]
pub struct StreamBudget {
    scale: f32,
    /// Recent frame time, ms, short average.
    fast_ms: f32,
    /// Calm-frame time, ms, the figure a hitch is measured against; 0 until the first frame.
    baseline_ms: f32,
    /// `WOW_STREAM_BUDGET=off`.
    off: bool,
}

impl Default for StreamBudget {
    fn default() -> Self {
        Self {
            scale: 1.0,
            fast_ms: 0.0,
            baseline_ms: 0.0,
            off: std::env::var("WOW_STREAM_BUDGET").as_deref() == Ok("off"),
        }
    }
}

impl StreamBudget {
    /// The current scale.
    pub fn scale(&self) -> f32 {
        if self.off {
            1.0
        } else {
            self.scale
        }
    }

    /// A time slice scaled; never below a tenth of a millisecond.
    pub fn duration(&self, base: Duration) -> Duration {
        base.mul_f32(self.scale()).max(Duration::from_micros(100))
    }

    /// A count slice scaled; never below one.
    pub fn count(&self, base: usize) -> usize {
        ((base as f32 * self.scale()).round() as usize).max(1)
    }

    /// Feed one frame's real time (ms).
    fn observe(&mut self, frame_ms: f32) {
        if self.baseline_ms == 0.0 {
            self.baseline_ms = frame_ms;
            self.fast_ms = frame_ms;
            return;
        }
        self.fast_ms += (frame_ms - self.fast_ms) * FAST_ALPHA;
        let hitch = self.fast_ms > self.baseline_ms * OVER + OVER_SLACK_MS;
        if hitch {
            self.scale = (self.scale * BACKOFF).max(MIN_SCALE);
        } else {
            self.scale = (self.scale + RECOVER).min(1.0);
        }
        // A sustained slow stretch must become the new normal (a heavier scene, a slower display),
        // or the scale would sit at the floor for good; only a spike is kept out of the baseline.
        if self.fast_ms <= self.baseline_ms * BASELINE_GATE || self.scale <= MIN_SCALE {
            self.baseline_ms += (self.fast_ms - self.baseline_ms) * SLOW_ALPHA;
        }
    }
}

/// [`StreamBudget::duration`] where the resource may be absent (a program with no streamer).
pub(crate) fn duration(budget: Option<&StreamBudget>, base: Duration) -> Duration {
    budget.map_or(base, |b| b.duration(base))
}

/// [`StreamBudget::count`] where the resource may be absent.
pub(crate) fn count(budget: Option<&StreamBudget>, base: usize) -> usize {
    budget.map_or(base, |b| b.count(base))
}

fn observe_frame(time: Res<Time<Real>>, mut budget: ResMut<StreamBudget>) {
    let ms = time.delta_secs() * 1000.0;
    // The first frame carries startup; a zero delta is a paused clock.
    if ms > 0.0 && ms < 1000.0 {
        budget.observe(ms);
    }
}

pub(crate) struct StreamBudgetPlugin;

impl Plugin for StreamBudgetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StreamBudget>()
            .add_systems(First, observe_frame.after(TimeSystems));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(b: &mut StreamBudget, ms: f32, frames: usize) {
        for _ in 0..frames {
            b.observe(ms);
        }
    }

    fn fresh() -> StreamBudget {
        StreamBudget {
            off: false,
            ..Default::default()
        }
    }

    #[test]
    fn steady_frames_keep_the_full_budget() {
        let mut b = fresh();
        run(&mut b, 16.7, 300);
        assert_eq!(b.scale(), 1.0);
    }

    #[test]
    fn a_hitch_backs_streaming_off_and_calm_restores_it() {
        let mut b = fresh();
        run(&mut b, 16.7, 120);
        run(&mut b, 45.0, 3);
        assert!(
            b.scale() <= 0.25 + 1e-6,
            "a sustained spike reaches the floor, got {}",
            b.scale()
        );
        run(&mut b, 16.7, 40);
        assert_eq!(b.scale(), 1.0, "calm frames restore the whole budget");
    }

    #[test]
    fn one_slightly_long_frame_in_a_calm_run_does_not_trigger() {
        let mut b = fresh();
        run(&mut b, 16.7, 120);
        b.observe(20.0);
        assert_eq!(b.scale(), 1.0);
    }

    #[test]
    fn a_pipelined_split_frame_pair_is_not_a_hitch() {
        let mut b = fresh();
        for _ in 0..200 {
            b.observe(26.4);
            b.observe(8.0);
        }
        assert_eq!(b.scale(), 1.0);
    }

    #[test]
    fn a_chronically_slower_display_becomes_the_baseline() {
        let mut b = fresh();
        run(&mut b, 16.7, 120);
        run(&mut b, 33.3, 600);
        assert_eq!(
            b.scale(),
            1.0,
            "a steady 30 fps is not a hitch once learned"
        );
    }

    #[test]
    fn slices_never_reach_zero() {
        let mut b = fresh();
        b.scale = MIN_SCALE;
        assert!(b.count(1) >= 1);
        assert!(b.duration(Duration::from_millis(4)) >= Duration::from_millis(1));
    }

    #[test]
    fn off_pins_the_scale() {
        let mut b = fresh();
        b.off = true;
        run(&mut b, 16.7, 120);
        run(&mut b, 80.0, 5);
        assert_eq!(b.scale(), 1.0);
    }
}
