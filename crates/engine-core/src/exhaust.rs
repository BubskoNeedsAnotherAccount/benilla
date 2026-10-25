//! Catalytic converter, muffler and upstream O2 sensor.

use crate::constants::*;

#[derive(Clone, Debug)]
pub struct CatalyticConverter {
    pub temperature_k: f64,
    pub clog_fraction: f64,
    pub damage_fraction: f64,
    /// Bolt-on: the converter has been replaced with a straight pipe.
    pub deleted: bool,
}

impl Default for CatalyticConverter {
    fn default() -> Self {
        Self { temperature_k: DEFAULT_AIR_TEMP_K, clog_fraction: 0.0, damage_fraction: 0.0, deleted: false }
    }
}

impl CatalyticConverter {
    pub fn is_lit_off(&self) -> bool {
        !self.deleted && self.temperature_k >= CATALYST_LIGHT_OFF_K
    }

    pub fn update(&mut self, dt: f64, exhaust_gas_temp_k: f64) {
        let alpha = 1.0 - (-dt / CATALYST_TAU_S).exp();
        self.temperature_k += (exhaust_gas_temp_k - self.temperature_k) * alpha;
    }

    pub fn register_backfire(&mut self) {
        if !self.deleted && self.temperature_k >= CATALYST_DAMAGE_HEAT_K {
            self.damage_fraction = (self.damage_fraction + CATALYST_BACKFIRE_DAMAGE).clamp(0.0, 1.0);
        }
    }

    pub fn flow_restriction_multiplier(&self) -> f64 {
        if self.deleted {
            return 1.0;
        }
        let clog = (self.clog_fraction + self.damage_fraction).clamp(0.0, 1.0);
        1.0 + clog * CATALYST_CLOG_BACKPRESSURE_MULT
    }
}

#[derive(Clone, Debug)]
pub struct OxygenSensor {
    pub connected: bool,
    pub stuck: bool,
    pub stuck_afr: f64,
    pub bias_afr: f64,
    pub reported_afr: f64,
}

impl OxygenSensor {
    pub fn new(stoich_afr: f64) -> Self {
        Self { connected: true, stuck: false, stuck_afr: stoich_afr, bias_afr: 0.0, reported_afr: stoich_afr }
    }

    pub fn is_healthy(&self) -> bool {
        self.connected && !self.stuck
    }

    pub fn update(&mut self, dt: f64, actual_afr: f64) {
        let target = if self.stuck { self.stuck_afr } else { actual_afr + self.bias_afr };
        let alpha = 1.0 - (-dt / O2_TAU_S).exp();
        self.reported_afr += (target - self.reported_afr) * alpha;
    }
}

#[derive(Clone, Debug)]
pub struct ExhaustSystem {
    pub catalyst: CatalyticConverter,
    /// Fault multiplier (a crushed or clogged muffler). 1.0 = healthy.
    pub muffler_restriction: f64,
    /// Flow multiplier from the bolt-on headers / cat / muffler / pipe diameter. 1.0 = stock.
    pub mod_flow: f64,
    pub o2: OxygenSensor,
}

impl ExhaustSystem {
    pub fn new() -> Self {
        Self {
            catalyst: CatalyticConverter::default(),
            muffler_restriction: 1.0,
            mod_flow: 1.0,
            o2: OxygenSensor::new(14.7),
        }
    }

    pub fn update(&mut self, dt: f64, exhaust_gas_temp_k: f64, actual_afr: f64) {
        self.catalyst.update(dt, exhaust_gas_temp_k);
        self.o2.update(dt, actual_afr);
    }

    pub fn backpressure_multiplier(&self) -> f64 {
        self.catalyst.flow_restriction_multiplier() * self.muffler_restriction * self.mod_flow
    }
}

impl Default for ExhaustSystem {
    fn default() -> Self {
        Self::new()
    }
}
