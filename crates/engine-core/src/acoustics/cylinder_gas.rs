//! Cylinder volume, valve timing and valve flow area: what makes a cylinder a gas reservoir that
//! breathes through two orifices.
//!
//! Crank angles are *local* to a cylinder, 0..720 degrees, 0 = intake TDC (the same convention as
//! `cylinder.rs`): the power stroke starts at 360, exhaust BDC is 540, exhaust TDC is 720.

use crate::config::{EngineConfig, IgnitionType};

/// Slider-crank geometry of one cylinder.
#[derive(Clone, Copy, Debug)]
pub struct CylGeom {
    pub bore: f32,
    pub stroke: f32,
    pub rod: f32,
    pub clearance: f32,
    pub piston_area: f32,
}

impl CylGeom {
    pub fn new(cfg: &EngineConfig) -> CylGeom {
        let bore = cfg.bore_meters as f32;
        let stroke = cfg.stroke_meters as f32;
        let piston_area = std::f32::consts::FRAC_PI_4 * bore * bore;
        let clearance = piston_area * stroke / (cfg.compression_ratio as f32 - 1.0);
        CylGeom { bore, stroke, rod: cfg.rod_length_meters as f32, clearance, piston_area }
    }

    /// Cylinder volume at a local crank angle, m^3.
    #[inline]
    pub fn volume(&self, local_deg: f32) -> f32 {
        let th = local_deg.to_radians();
        let r = 0.5 * self.stroke;
        let (s, c) = th.sin_cos();
        let x = r * c + (self.rod * self.rod - r * r * s * s).sqrt();
        self.clearance + self.piston_area * (r + self.rod - x)
    }

    /// Volume rate of change per radian of crank rotation, m^3/rad.
    #[inline]
    pub fn dv_dtheta(&self, local_deg: f32) -> f32 {
        let th = local_deg.to_radians();
        let r = 0.5 * self.stroke;
        let (s, c) = th.sin_cos();
        let root = (self.rod * self.rod - r * r * s * s).sqrt();
        self.piston_area * r * s * (1.0 + r * c / root)
    }
}

/// One valve's opening window and peak effective flow area (discharge coefficient included).
#[derive(Clone, Copy, Debug)]
pub struct Valve {
    /// Local crank angle at which it starts to lift, and the duration (seat to seat), degrees.
    pub open: f32,
    pub dur: f32,
    /// Effective flow area at full lift, m^2.
    pub peak_area: f32,
}

impl Valve {
    /// Effective flow area at a local crank angle: a sin^2 lift profile, and the curtain-to-throat
    /// saturation of a real valve (flow stops growing once the lift passes ~a quarter of the diameter).
    #[inline]
    pub fn area(&self, local_deg: f32) -> f32 {
        let x = (local_deg - self.open).rem_euclid(720.0) / self.dur;
        if x >= 1.0 {
            return 0.0;
        }
        let s = (std::f32::consts::PI * x).sin();
        let lift = s * s;
        self.peak_area * (1.0 - (-5.0 * lift).exp()) / (1.0 - (-5.0f32).exp())
    }

    pub fn is_open(&self, local_deg: f32) -> bool {
        (local_deg - self.open).rem_euclid(720.0) < self.dur
    }

    /// Crank angle at which the valve closes.
    pub fn close(&self) -> f32 {
        (self.open + self.dur).rem_euclid(720.0)
    }
}

/// Intake and exhaust valve of an engine.
#[derive(Clone, Copy, Debug)]
pub struct ValveTrain {
    pub intake: Valve,
    pub exhaust: Valve,
}

/// A plausible cam for the engine: longer duration and more overlap for the high-revving ones,
/// short and nearly overlap-free for the diesel.
pub fn valve_train(cfg: &EngineConfig) -> ValveTrain {
    let g = CylGeom::new(cfg);
    let diesel = cfg.ignition_type == IgnitionType::Compression;
    let sport = (cfg.ve_peak_rpm / 8000.0).clamp(0.25, 1.0) as f32;
    let dur_in = if diesel { 232.0 } else { 235.0 + 55.0 * sport };
    let dur_ex = dur_in - 8.0;
    let overlap = if diesel { 14.0 } else { 8.0 + 40.0 * sport };
    let ivo = 720.0 - 0.5 * overlap;
    let evc_abs = 720.0 + 0.5 * overlap;
    let evo = evc_abs - dur_ex;
    let (a_ex, a_in) = if diesel { (0.085, 0.12) } else { (0.105, 0.145) };
    ValveTrain {
        intake: Valve { open: ivo, dur: dur_in, peak_area: a_in * g.piston_area },
        exhaust: Valve { open: evo, dur: dur_ex, peak_area: a_ex * g.piston_area },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::engines;

    #[test]
    fn volume_matches_the_engine_geometry() {
        for cfg in engines() {
            let g = CylGeom::new(&cfg);
            let (tdc, bdc) = (g.volume(0.0), g.volume(180.0));
            assert!((bdc / tdc - cfg.compression_ratio as f32).abs() < 0.05, "{}: CR {}", cfg.id, bdc / tdc);
            // derivative agrees with a finite difference
            let d = (g.volume(91.0) - g.volume(89.0)) / (2.0f32).to_radians();
            assert!((g.dv_dtheta(90.0) / d - 1.0).abs() < 0.01, "{}", cfg.id);
        }
    }

    #[test]
    fn valves_open_and_close_in_the_right_places() {
        for cfg in engines() {
            let vt = valve_train(&cfg);
            // exhaust opens before bottom dead centre (540) and closes just after top (720 -> ~10)
            assert!(vt.exhaust.open > 440.0 && vt.exhaust.open < 520.0, "{}: EVO {}", cfg.id, vt.exhaust.open);
            assert!(vt.exhaust.close() < 40.0, "{}: EVC {}", cfg.id, vt.exhaust.close());
            // intake opens a little before top and closes after bottom (180)
            assert!(vt.intake.open > 690.0, "{}: IVO {}", cfg.id, vt.intake.open);
            assert!(vt.intake.close() > 200.0 && vt.intake.close() < 280.0, "{}: IVC {}", cfg.id, vt.intake.close());
            // they overlap at TDC
            assert!(vt.exhaust.is_open(715.0) && vt.intake.is_open(725.0 % 720.0 + 0.0) || cfg.ignition_type == IgnitionType::Compression);
            assert_eq!(vt.exhaust.area(300.0), 0.0);
            assert!(vt.exhaust.area(600.0) > 0.9 * vt.exhaust.peak_area * 0.9);
        }
    }
}
