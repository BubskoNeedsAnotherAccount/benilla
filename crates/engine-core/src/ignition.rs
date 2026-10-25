//! Per-cylinder connector -> coil -> plug signal path with fault injection.

use crate::constants::*;
use crate::rng::Rng;

#[derive(Clone, Debug)]
pub struct IgnitionCoilConnector {
    pub connected: bool,
    pub corrosion: f64,
}

impl Default for IgnitionCoilConnector {
    fn default() -> Self {
        Self { connected: true, corrosion: 0.0 }
    }
}

impl IgnitionCoilConnector {
    fn conducts_this_attempt(&self, rng: &mut Rng) -> bool {
        if !self.connected {
            return false;
        }
        if self.corrosion <= 0.0 {
            return true;
        }
        rng.next_f64() >= self.corrosion * CONNECTOR_CORROSION_SENS
    }
}

#[derive(Clone, Debug)]
pub struct IgnitionCoil {
    pub health: f64,
    pub shorted: bool,
}

impl Default for IgnitionCoil {
    fn default() -> Self {
        Self { health: 1.0, shorted: false }
    }
}

impl IgnitionCoil {
    fn weakness_misfire_probability(&self, cylinder_pressure_pa: f64) -> f64 {
        if self.shorted {
            return 1.0;
        }
        let pressure_factor = (cylinder_pressure_pa / COIL_REF_PRESSURE_PA).clamp(0.0, 1.5);
        (1.0 - self.health) * COIL_WEAK_SENS * pressure_factor
    }
}

#[derive(Clone, Debug)]
pub struct SparkPlug {
    pub gap_mm: f64,
    pub fouling: f64,
    pub electrode_wear: f64,
}

impl Default for SparkPlug {
    fn default() -> Self {
        Self { gap_mm: SPARK_PLUG_IDEAL_GAP_MM, fouling: 0.0, electrode_wear: 0.0 }
    }
}

impl SparkPlug {
    fn condition_misfire_probability(&self) -> f64 {
        let gap_dev = ((self.gap_mm - SPARK_PLUG_IDEAL_GAP_MM).abs() - SPARK_PLUG_GAP_TOLERANCE_MM).max(0.0);
        gap_dev * SPARK_PLUG_GAP_MISFIRE_PER_MM
            + self.fouling * SPARK_PLUG_FOULING_SENS
            + self.electrode_wear * SPARK_PLUG_WEAR_SENS
    }
}

#[derive(Clone, Debug, Default)]
pub struct IgnitionChannel {
    pub connector: IgnitionCoilConnector,
    pub coil: IgnitionCoil,
    pub spark_plug: SparkPlug,
}

impl IgnitionChannel {
    /// The single roll for "did this cylinder's spark actually happen this cycle".
    pub fn attempt_spark(&self, cylinder_pressure_pa: f64, rng: &mut Rng) -> bool {
        if !self.connector.conducts_this_attempt(rng) {
            return false;
        }
        if self.coil.shorted {
            return false;
        }
        let p = (self.coil.weakness_misfire_probability(cylinder_pressure_pa)
            + self.spark_plug.condition_misfire_probability())
        .clamp(0.0, 1.0);
        rng.next_f64() >= p
    }

    pub fn is_faulted(&self) -> bool {
        !self.connector.connected
            || self.connector.corrosion > 0.0
            || self.coil.shorted
            || self.coil.health < 1.0
            || self.spark_plug.fouling > 0.0
            || self.spark_plug.electrode_wear > 0.0
            || (self.spark_plug.gap_mm - SPARK_PLUG_IDEAL_GAP_MM).abs() > SPARK_PLUG_GAP_TOLERANCE_MM
    }
}
