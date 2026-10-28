use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurboConfig {
    pub max_boost_pa: f64,
    pub full_spool_rpm: f64,
    pub spool_time_constant_s: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IgnitionType {
    Spark,
    Compression,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineConfig {
    pub id: String,
    pub display_name: String,
    pub cylinder_count: usize,
    pub bore_meters: f64,
    pub stroke_meters: f64,
    pub rod_length_meters: f64,
    pub compression_ratio: f64,
    pub reciprocating_mass_kg: f64,
    pub rotating_inertia_kg_m2: f64,
    pub firing_order: Vec<u32>,
    pub firing_intervals_deg: Vec<f64>,
    pub friction_base_nm: f64,
    pub friction_viscous_coeff: f64,
    pub friction_windage_coeff: f64,
    pub idle_throttle_floor: f64,
    pub redline_rpm: f64,
    pub throttle_response_rate: Option<f64>,
    pub ve_peak_rpm: f64,
    pub ve_spread_rpm: f64,
    pub ve_base: f64,
    pub ve_peak_bonus: f64,
    pub starter_torque_nm: f64,
    pub ignition_type: IgnitionType,
    pub ignition_advance_deg: f64,
    pub combustion_duration_deg: f64,
    pub thermal_mass_j_per_k: f64,
    pub knock_threshold: Option<f64>,
    pub turbo: Option<TurboConfig>,
    pub fuel_tank_capacity_l: Option<f64>,
}

impl EngineConfig {
    pub fn displacement_l(&self) -> f64 {
        let area = std::f64::consts::PI / 4.0 * self.bore_meters.powi(2);
        area * self.stroke_meters * self.cylinder_count as f64 * 1000.0
    }
}

/// The cylinder layout an engine id belongs to: the id up to the first `_` ("v8_50" -> "v8").
pub fn layout_of(id: &str) -> &str {
    id.split('_').next().unwrap_or(id)
}

/// Bank tilt angles (degrees from vertical, about the crank axis) for the renderer. The sim
/// itself is layout-agnostic; this only tells the 3D rig where to put the cylinder banks.
/// Variants of one layout share its prefix: `v8_50` is a `v8`.
pub fn bank_tilts_deg(id: &str) -> Vec<f64> {
    match layout_of(id) {
        "vtwin" => vec![-22.5, 22.5],
        "v6" => vec![-30.0, 30.0],
        "v8" => vec![-45.0, 45.0],
        "v10" => vec![-45.0, 45.0],
        "v12" => vec![-30.0, 30.0],
        "v16" => vec![-22.5, 22.5],
        "w16" => vec![-52.5, -37.5, 37.5, 52.5],
        "w24" => vec![-30.0, 0.0, 30.0],
        _ => vec![0.0],
    }
}

#[derive(Clone, Debug)]
pub struct GearDefinition {
    pub label: &'static str,
    pub ratio: f64,
}

#[derive(Clone, Debug)]
pub struct TransmissionConfig {
    pub id: &'static str,
    pub display_name: &'static str,
    pub final_drive_ratio: f64,
    pub gears: Vec<GearDefinition>,
}

fn gears(spec: &[(&'static str, f64)]) -> Vec<GearDefinition> {
    spec.iter()
        .map(|&(label, ratio)| GearDefinition { label, ratio })
        .collect()
}

pub fn transmissions() -> Vec<TransmissionConfig> {
    vec![
        TransmissionConfig {
            id: "5speed",
            display_name: "5-Speed Manual",
            final_drive_ratio: 3.73,
            gears: gears(&[
                ("R", -3.66),
                ("N", 0.0),
                ("1", 3.82),
                ("2", 2.26),
                ("3", 1.48),
                ("4", 1.0),
                ("5", 0.79),
            ]),
        },
        TransmissionConfig {
            id: "6speed",
            display_name: "6-Speed Manual",
            final_drive_ratio: 3.42,
            gears: gears(&[
                ("R", -3.58),
                ("N", 0.0),
                ("1", 3.36),
                ("2", 2.07),
                ("3", 1.36),
                ("4", 1.0),
                ("5", 0.85),
                ("6", 0.68),
            ]),
        },
        TransmissionConfig {
            id: "8speed",
            display_name: "8-Speed Auto",
            final_drive_ratio: 3.16,
            gears: gears(&[
                ("R", -3.05),
                ("N", 0.0),
                ("1", 4.71),
                ("2", 3.14),
                ("3", 2.11),
                ("4", 1.67),
                ("5", 1.29),
                ("6", 1.0),
                ("7", 0.84),
                ("8", 0.67),
            ]),
        },
        TransmissionConfig {
            id: "10speed",
            display_name: "10-Speed Auto",
            final_drive_ratio: 2.87,
            gears: gears(&[
                ("R", -4.71),
                ("N", 0.0),
                ("1", 4.7),
                ("2", 2.99),
                ("3", 2.15),
                ("4", 1.76),
                ("5", 1.52),
                ("6", 1.27),
                ("7", 1.0),
                ("8", 0.85),
                ("9", 0.69),
                ("10", 0.64),
            ]),
        },
    ]
}

#[derive(Clone, Debug)]
pub struct BrakeConfig {
    pub id: &'static str,
    pub display_name: &'static str,
    pub max_force_n: f64,
}

pub fn brakes() -> Vec<BrakeConfig> {
    vec![
        BrakeConfig {
            id: "standard",
            display_name: "Standard",
            max_force_n: 13_000.0,
        },
        BrakeConfig {
            id: "sport",
            display_name: "Sport",
            max_force_n: 16_000.0,
        },
        BrakeConfig {
            id: "track",
            display_name: "Track",
            max_force_n: 20_000.0,
        },
    ]
}

#[derive(Clone, Debug)]
pub struct FuelConfig {
    pub id: &'static str,
    pub display_name: &'static str,
    pub ignition_type: IgnitionType,
    pub density_kg_per_l: f64,
    pub lower_heating_value: f64,
    pub stoichiometric_afr: f64,
    pub wot_afr: f64,
    pub knock_resistance: f64,
}

pub fn fuels() -> Vec<FuelConfig> {
    use crate::constants::{DIESEL_IDLE_AFR, DIESEL_MIN_AFR};
    use IgnitionType::{Compression, Spark};
    let f = |id,
             display_name,
             ignition_type,
             density_kg_per_l,
             lower_heating_value,
             stoichiometric_afr,
             wot_afr,
             knock_resistance| {
        FuelConfig {
            id,
            display_name,
            ignition_type,
            density_kg_per_l,
            lower_heating_value,
            stoichiometric_afr,
            wot_afr,
            knock_resistance,
        }
    };
    vec![
        f(
            "regular87",
            "Regular 87",
            Spark,
            0.745,
            44_000_000.0,
            14.7,
            12.8,
            1.0,
        ),
        f(
            "premium93",
            "Premium 93",
            Spark,
            0.743,
            44_000_000.0,
            14.7,
            12.8,
            1.18,
        ),
        f("e85", "E85", Spark, 0.781, 29_200_000.0, 9.8, 9.2, 1.45),
        f(
            "race100",
            "Race 100",
            Spark,
            0.752,
            43_500_000.0,
            14.6,
            12.6,
            1.65,
        ),
        f(
            "diesel",
            "Diesel",
            Compression,
            0.832,
            42_800_000.0,
            DIESEL_IDLE_AFR,
            DIESEL_MIN_AFR,
            1.0,
        ),
    ]
}

static ENGINES_JSON: &str = include_str!("../data/engines.json");

pub fn engines() -> Vec<EngineConfig> {
    serde_json::from_str(ENGINES_JSON).expect("embedded engines.json is valid")
}

pub fn engine_by_id(id: &str) -> Option<EngineConfig> {
    engines().into_iter().find(|e| e.id == id)
}
