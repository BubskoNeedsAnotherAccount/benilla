//! EVAP system: purge valve, canister vent valve, vapor canister, line integrity and the simulated
//! fuel-tank pressure signal the ECU's leak-check test reads.

use crate::constants::*;

#[derive(Clone, Debug)]
pub struct PurgeValve {
    pub connected: bool,
    pub stuck_open: bool,
    pub stuck_closed: bool,
}

impl Default for PurgeValve {
    fn default() -> Self {
        Self {
            connected: true,
            stuck_open: false,
            stuck_closed: false,
        }
    }
}

impl PurgeValve {
    pub fn effective_open_fraction(&self, commanded: f64) -> f64 {
        if self.stuck_open {
            return 1.0;
        }
        if !self.connected || self.stuck_closed {
            return 0.0;
        }
        commanded.clamp(0.0, 1.0)
    }
}

/// Normally-open vent: losing power fails it open, not closed.
#[derive(Clone, Debug)]
pub struct CanisterVentValve {
    pub connected: bool,
    pub stuck_open: bool,
    pub stuck_closed: bool,
}

impl Default for CanisterVentValve {
    fn default() -> Self {
        Self {
            connected: true,
            stuck_open: false,
            stuck_closed: false,
        }
    }
}

impl CanisterVentValve {
    pub fn is_effectively_closed(&self, commanded_closed: bool) -> bool {
        if self.stuck_open || !self.connected {
            return false;
        }
        if self.stuck_closed {
            return true;
        }
        commanded_closed
    }
}

#[derive(Clone, Debug)]
pub struct EvapSystem {
    pub purge_valve: PurgeValve,
    pub vent_valve: CanisterVentValve,
    pub canister_saturation: f64,
    leak_fraction: f64,
    tank_pressure_pa: f64,
}

impl Default for EvapSystem {
    fn default() -> Self {
        Self {
            purge_valve: PurgeValve::default(),
            vent_valve: CanisterVentValve::default(),
            canister_saturation: DEFAULT_CANISTER_SATURATION,
            leak_fraction: 0.0,
            tank_pressure_pa: 0.0,
        }
    }
}

impl EvapSystem {
    pub fn set_leak_fraction(&mut self, f: f64) {
        self.leak_fraction = f.clamp(0.0, 1.0);
    }

    pub fn leak_fraction(&self) -> f64 {
        self.leak_fraction
    }

    pub fn tank_pressure_pa(&self) -> f64 {
        self.tank_pressure_pa
    }

    /// Negative = richening, from vapor purge.
    pub fn purge_afr_adjustment(&self, commanded: f64) -> f64 {
        -self.purge_valve.effective_open_fraction(commanded)
            * self.canister_saturation
            * EVAP_PURGE_RICHEN_SENSITIVITY
    }

    /// Positive = leaning, from an unmetered line leak.
    pub fn leak_afr_adjustment(&self) -> f64 {
        self.leak_fraction * EVAP_LEAK_LEAN_SENSITIVITY
    }

    pub fn update_tank_pressure(
        &mut self,
        dt: f64,
        manifold_vacuum_pa: f64,
        vent_commanded_closed: bool,
        purge_open_fraction: f64,
    ) {
        if !self.vent_valve.is_effectively_closed(vent_commanded_closed) {
            let alpha = 1.0 - (-dt / EVAP_VENT_OPEN_TIME_CONSTANT_S).exp();
            self.tank_pressure_pa += (0.0 - self.tank_pressure_pa) * alpha;
            return;
        }
        let purge_rate = purge_open_fraction * EVAP_PURGE_VACUUM_COUPLING_GAIN_PER_S;
        let leak_rate = self.leak_fraction * EVAP_LEAK_CONDUCTANCE_MAX_PER_S;
        let total = purge_rate + leak_rate;
        if total <= 0.0 {
            return;
        }
        let target = purge_rate * manifold_vacuum_pa / total;
        let alpha = 1.0 - (-total * dt).exp();
        self.tank_pressure_pa += (target - self.tank_pressure_pa) * alpha;
    }
}
