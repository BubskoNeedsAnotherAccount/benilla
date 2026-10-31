//! The pressure in a cylinder through the cycle, as far as the sound needs it: the torque pulses that make the crank
//! speed up and slow down (the within-cycle ripple and, over many cycles, the idle's slow wander) and how hard each
//! combustion hits the block (the peak rate of pressure rise, which is what a block rattle answers to).
//!
//! The pipe model's cylinders are gas reservoirs for the exhaust and intake flow; they hold the mass and its
//! temperature at the moment the exhaust valve opens but know nothing of the burn. This is the same model the engine
//! model uses for its own cylinders (polytropic compression and expansion, a Wiebe heat release, the same geometry,
//! heat of combustion and efficiencies), evaluated once per snapshot of the engine state into a table over the 720
//! degrees of the cycle, so that the sound's torque and rattle follow the same physics for the same reason: more
//! charge (a wider throttle, boost) is more heat is a harder pulse and a harder rattle, spark advance moves the burn
//! before top dead centre and softens the rattle, a diesel burns its premixed charge in an instant.

use crate::acoustics::cylinder_gas::CylGeom;
use crate::config::{EngineConfig, IgnitionType};
use crate::constants::{
    COMBUSTION_EFFICIENCY, COMBUSTION_WALL_HEAT_LOSS_FRACTION, GAS_CONSTANT_AIR, POLYTROPIC_INDEX,
};

/// Resolution of the tables, degrees.
const STEP_DEG: f32 = 0.5;
const N: usize = (720.0 / STEP_DEG) as usize;

/// Lower heating value of the fuel, J/kg (the engine model's gasoline and diesel).
const LHV_GASOLINE: f32 = 44.0e6;
const LHV_DIESEL: f32 = 43.0e6;

/// Everything the table depends on, from the engine state.
#[derive(Clone, Copy, Debug)]
pub struct Charge {
    /// Manifold pressure the cylinder fills to, Pa.
    pub map_pa: f32,
    /// Temperature of the charge, K.
    pub charge_k: f32,
    pub air_fuel_ratio: f32,
    /// Pressure outside (exhaust at the end of the blow-down), Pa.
    pub ambient_pa: f32,
}

#[derive(Clone, Debug)]
pub struct Combustion {
    geom: CylGeom,
    diesel: bool,
    start_deg: f32,
    duration_deg: f32,
    /// Where the exhaust valve opens (the pressure falls to ambient over the next 25 degrees).
    evo_deg: f32,
    /// Pressure of the motored cylinder (no combustion) and the extra pressure of a complete burn, Pa, every `STEP_DEG`.
    motor: Vec<f32>,
    burn: Vec<f32>,
    ambient: f32,
    /// Torque per cylinder per Pa of its gas pressure, Nm/Pa: dV/dtheta, every `STEP_DEG`.
    dv: Vec<f32>,
    /// What the tables need of the geometry and the burn timing, which a charge does not change, every
    /// `STEP_DEG`: the volume, the polytropic pressure ratio of the step that ends there, and the Wiebe
    /// fraction burned inside the burn.
    vol: Vec<f32>,
    compress: Vec<f32>,
    burned: Vec<Option<f32>>,
    /// Peak rate of pressure rise the burn adds, bar per degree (set with the tables).
    peak_rise: f32,
}

/// Mass fraction burned after `x` degrees of a burn `d` degrees long (the engine model's Wiebe function).
fn wiebe(x: f32, d: f32) -> f32 {
    crate::cylinder::wiebe_mass_fraction_burned(x as f64, d as f64) as f32
}

impl Combustion {
    pub fn new(cfg: &EngineConfig, evo_deg: f32) -> Combustion {
        let geom = CylGeom::new(cfg);
        let dv = (0..N)
            .map(|k| geom.dv_dtheta(k as f32 * STEP_DEG))
            .collect();
        let start_deg = 360.0 - cfg.ignition_advance_deg as f32;
        let duration_deg = cfg.combustion_duration_deg as f32;
        let n = POLYTROPIC_INDEX as f32;
        let vol: Vec<f32> = (0..N).map(|k| geom.volume(k as f32 * STEP_DEG)).collect();
        let compress = (0..N)
            .map(|k| {
                let v_prev = if k == 0 {
                    geom.volume(180.0)
                } else {
                    vol[k - 1]
                };
                (v_prev / vol[k]).powf(n)
            })
            .collect();
        let burned = (0..N)
            .map(|k| {
                let th = k as f32 * STEP_DEG;
                (th >= start_deg && th <= start_deg + duration_deg)
                    .then(|| wiebe(th - start_deg, duration_deg))
            })
            .collect();
        let mut c = Combustion {
            geom,
            diesel: cfg.ignition_type == IgnitionType::Compression,
            start_deg,
            duration_deg,
            evo_deg,
            motor: vec![0.0; N],
            burn: vec![0.0; N],
            ambient: 101_325.0,
            dv,
            vol,
            compress,
            burned,
            peak_rise: 0.0,
        };
        c.set_charge(&Charge {
            map_pa: 101_325.0,
            charge_k: 300.0,
            air_fuel_ratio: 14.7,
            ambient_pa: 101_325.0,
        });
        c
    }

    /// Rebuilds the tables for a new charge (done once per snapshot of the engine state).
    pub fn set_charge(&mut self, c: &Charge) {
        let g = &self.geom;
        let n = POLYTROPIC_INDEX as f32;
        self.ambient = c.ambient_pa;
        let v_ivc = g.volume(180.0);
        let mass = c.map_pa * v_ivc / (GAS_CONSTANT_AIR as f32 * c.charge_k.max(200.0));
        let lhv = if self.diesel {
            LHV_DIESEL
        } else {
            LHV_GASOLINE
        };
        let heat = mass / c.air_fuel_ratio.max(5.0)
            * lhv
            * (COMBUSTION_EFFICIENCY * (1.0 - COMBUSTION_WALL_HEAT_LOSS_FRACTION)) as f32;
        // the compounding of the engine model: every step compresses or expands everything in the cylinder by the
        // polytropic law and adds the heat of that step
        let steps = (N as f32 * 0.5) as usize; // 0..360 degrees of table
        let _ = steps;
        let mut p_m = c.map_pa;
        let mut p_b = 0.0f32;
        let mut x_prev = 0.0f32;
        self.peak_rise = 0.0;
        for k in 0..N {
            let th = k as f32 * STEP_DEG;
            let v = self.vol[k];
            if th < 180.0 {
                // intake: the cylinder sits at manifold pressure
                self.motor[k] = c.map_pa;
                self.burn[k] = 0.0;
            } else if th < self.evo_deg {
                let r = self.compress[k];
                p_m *= r;
                p_b *= r;
                if let Some(x) = self.burned[k] {
                    p_b += (n - 1.0) * heat * (x - x_prev).max(0.0) / v;
                    x_prev = x;
                }
                self.motor[k] = p_m;
                self.burn[k] = p_b;
            } else {
                // the exhaust valve opens: down to ambient over 25 degrees
                let f = ((th - self.evo_deg) / 25.0).clamp(0.0, 1.0);
                let (m0, b0) = if k > 0 {
                    (
                        self.motor[(self.evo_deg / STEP_DEG) as usize],
                        self.burn[(self.evo_deg / STEP_DEG) as usize],
                    )
                } else {
                    (c.map_pa, 0.0)
                };
                self.motor[k] = m0 + (c.ambient_pa - m0) * f;
                self.burn[k] = b0 * (1.0 - f);
                p_m = c.map_pa;
                p_b = 0.0;
            }
        }
        // the engine model's cylinder takes the manifold pressure again over the intake stroke: nothing more to do
        // peak rate of pressure rise of a complete burn (bar per degree)
        let mut peak = 0.0f32;
        for k in 1..N {
            let th = k as f32 * STEP_DEG;
            if th > self.start_deg - 5.0 && th < self.start_deg + self.duration_deg + 5.0 {
                // the rise the burn adds: compressing the charge raises the pressure too, but that is not what knocks
                let d = (self.burn[k] - self.burn[k - 1]) / STEP_DEG / 1.0e5;
                peak = peak.max(d);
            }
        }
        self.peak_rise = peak;
    }

    /// Pressure in a cylinder at local crank angle `deg` (0..720) whose last charge burned to the fraction `burn`, Pa.
    #[inline]
    pub fn pressure(&self, deg: f32, burn: f32) -> f32 {
        let x = deg.rem_euclid(720.0) / STEP_DEG;
        let k = (x as usize).min(N - 1);
        let f = x - k as f32;
        let k2 = if k + 1 < N { k + 1 } else { 0 };
        let m = self.motor[k] + (self.motor[k2] - self.motor[k]) * f;
        let b = self.burn[k] + (self.burn[k2] - self.burn[k]) * f;
        m + burn * b
    }

    /// Torque this cylinder's gas puts on the crank at local angle `deg`, Nm.
    #[inline]
    pub fn torque(&self, deg: f32, burn: f32) -> f32 {
        let x = deg.rem_euclid(720.0) / STEP_DEG;
        let k = (x as usize).min(N - 1);
        let f = x - k as f32;
        let k2 = if k + 1 < N { k + 1 } else { 0 };
        let dv = self.dv[k] + (self.dv[k2] - self.dv[k]) * f;
        (self.pressure(deg, burn) - self.ambient) * dv
    }

    /// Peak rate of pressure rise of a complete burn, bar per crank degree: how hard the block is struck. A diesel
    /// adds the premixed flash of its ignition delay, which the single smooth Wiebe curve does not have.
    pub fn peak_rise_bar_per_deg(&self) -> f32 {
        self.peak_rise * if self.diesel { DIESEL_PREMIX } else { 1.0 }
    }

    /// The same without the diesel's premixed flash (the reference the rattle level is set against).
    pub fn peak_rise_raw(&self) -> f32 {
        self.peak_rise
    }

    pub fn is_diesel(&self) -> bool {
        self.diesel
    }
}

/// Multiplier on a diesel's rate of rise for the premixed burn (the fuel that mixed during the ignition delay burns in
/// a few degrees); the clatter's own gain is set in `mechanical` (a diesel's impulse is 7x a petrol engine's there).
const DIESEL_PREMIX: f32 = 1.5;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::engine_by_id;

    fn charge(map: f32) -> Charge {
        Charge {
            map_pa: map,
            charge_k: 310.0,
            air_fuel_ratio: 14.7,
            ambient_pa: 101_325.0,
        }
    }

    #[test]
    fn a_firing_cylinder_peaks_after_top_dead_centre_and_a_motored_one_before() {
        let cfg = engine_by_id("v8").unwrap();
        let mut c = Combustion::new(&cfg, 490.0);
        c.set_charge(&charge(95_000.0));
        let peak = |burn: f32| {
            (360..540)
                .map(|d| (d, c.pressure(d as f32, burn)))
                .fold((0, 0.0f32), |a, b| if b.1 > a.1 { b } else { a })
        };
        let (d_fired, p_fired) = peak(1.0);
        let (d_motor, p_motor) = peak(0.0);
        println!(
            "v8: fired peak {:.1} bar at {d_fired} deg, motored {:.1} bar at {d_motor} deg",
            p_fired / 1e5,
            p_motor / 1e5
        );
        assert!(
            d_motor <= 363 && p_motor > 1.0e6,
            "motored peak {p_motor} at {d_motor}"
        );
        assert!(
            (368..390).contains(&d_fired),
            "fired cylinder peaks at {d_fired}"
        );
        assert!(
            p_fired > 2.5 * p_motor && p_fired < 1.0e7,
            "fired {p_fired}, motored {p_motor}"
        );
    }

    #[test]
    fn the_rattle_follows_the_charge_for_petrol_and_diesel_alike() {
        let v8 = engine_by_id("v8").unwrap();
        let mut c = Combustion::new(&v8, 490.0);
        c.set_charge(&charge(35_000.0));
        let idle = c.peak_rise_bar_per_deg();
        c.set_charge(&charge(100_000.0));
        let wot = c.peak_rise_bar_per_deg();
        let di = engine_by_id("dieseli6").unwrap();
        let mut d = Combustion::new(&di, 490.0);
        let at = |afr: f32| Charge {
            map_pa: 100_000.0,
            charge_k: 310.0,
            air_fuel_ratio: afr,
            ambient_pa: 101_325.0,
        };
        d.set_charge(&at(60.0));
        let light = d.peak_rise_bar_per_deg();
        d.set_charge(&at(21.0));
        let loaded = d.peak_rise_bar_per_deg();
        println!("rate of rise, bar per degree: v8 idle {idle:.2}, v8 wot {wot:.2}, diesel light {light:.2}, loaded {loaded:.2}");
        assert!(
            wot > 2.0 * idle && wot < 6.0 * idle,
            "idle {idle}, wot {wot}"
        );
        assert!(idle > 0.3 && wot < 8.0);
        assert!(
            loaded > 2.0 * light,
            "a loaded diesel burns more fuel per cycle: light {light}, loaded {loaded}"
        );
    }

    #[test]
    fn the_mean_torque_of_the_cycle_is_positive_and_the_ripple_is_there() {
        let cfg = engine_by_id("i4_16").unwrap_or_else(|| engine_by_id("i4").unwrap());
        let mut c = Combustion::new(&cfg, 490.0);
        c.set_charge(&charge(50_000.0));
        let n = 720;
        let t: Vec<f32> = (0..n).map(|d| c.torque(d as f32, 1.0)).collect();
        let mean = t.iter().sum::<f32>() / n as f32;
        let (lo, hi) = (
            t.iter().cloned().fold(f32::MAX, f32::min),
            t.iter().cloned().fold(f32::MIN, f32::max),
        );
        println!(
            "one cylinder: mean {mean:.1} Nm per cycle-degree-average, range {lo:.0}..{hi:.0}"
        );
        assert!(mean > 0.0 && hi > 4.0 * mean);
    }
}
