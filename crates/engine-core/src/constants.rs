//! Tuning constants, ported 1:1 from the original TypeScript sim (`shared/engine/config/*`).

// --- Gas & fuel ---
pub const GAS_CONSTANT_AIR: f64 = 287.0;
pub const AIR_DENSITY_REFERENCE: f64 = 1.225;
pub const DEFAULT_AIR_TEMP_K: f64 = 300.0;
pub const AIR_TEMP_MIN_K: f64 = 250.0;
pub const AIR_TEMP_MAX_K: f64 = 320.0;
pub const DEFAULT_ATMOSPHERIC_PA: f64 = AIR_DENSITY_REFERENCE * GAS_CONSTANT_AIR * DEFAULT_AIR_TEMP_K;
pub const POLYTROPIC_INDEX: f64 = 1.3;
pub const SPARK_AFR_SAFETY_MIN: f64 = 9.0;
pub const SPARK_AFR_SAFETY_MAX: f64 = 22.0;
pub const COMBUSTION_EFFICIENCY: f64 = 0.95;
pub const COMBUSTION_WALL_HEAT_LOSS_FRACTION: f64 = 0.22;
pub const WIEBE_A: f64 = 5.0;
pub const WIEBE_M: f64 = 2.0;

// --- Resolution ---
pub const SUBSTEP_HZ: f64 = 10_000.0;
pub const SUBSTEP_TARGET_DEG: f64 = 4.0;
pub const MAX_SUBSTEP_S: f64 = 1.0 / 250.0;
pub const MAX_SUBSTEPS_PER_FRAME: f64 = 400.0;
pub const DISPLAY_SMOOTHING_TAU_S: f64 = 0.3;

// --- Operating ---
pub const IDLE_RPM: f64 = 800.0;
pub const IDLE_GOVERNOR_GAIN: f64 = 1.0 / 1500.0;
pub const IDLE_GOVERNOR_MAX_THROTTLE: f64 = 0.2;
/// Extra throttle opening at light-off (fast idle / cold-start flare) and how fast it relaxes.
pub const START_FLARE_THROTTLE: f64 = 0.7;
pub const START_FLARE_TAU_S: f64 = 0.9;
pub const STALL_RPM: f64 = 280.0;
pub const STALL_GRACE_S: f64 = 0.2;
/// Free-running speed of the starter (torque falls linearly to zero here), so it cranks ~250 rpm.
pub const STARTER_MAX_RPM: f64 = 420.0;
/// A starter must at least push an unlit engine over compression: stall torque per litre of
/// displacement at 10.5:1 (scaled with compression ratio).
pub const STARTER_MIN_NM_PER_L: f64 = 62.0;
pub const STARTER_DISENGAGE_RPM: f64 = 500.0;
pub const REV_LIMITER_HYSTERESIS_RPM: f64 = 150.0;
pub const BRAKE_DECLUTCH_REENGAGE_HYSTERESIS_RPM: f64 = 150.0;
pub const THROTTLE_ACTUATOR_RATE: f64 = 6.0;
pub const CLUTCH_SLIP_DURATION_S: f64 = 0.25;

// --- Drivetrain ---
pub const WHEEL_RADIUS_M: f64 = 0.33;
pub const DEFAULT_VEHICLE_MASS_KG: f64 = 1600.0;
pub const VEHICLE_MASS_MIN_KG: f64 = 150.0;
pub const VEHICLE_MASS_MAX_KG: f64 = 15_000.0;
pub const ROAD_LOAD_A_N: f64 = 200.0;
pub const ROAD_LOAD_B_NS_PER_M: f64 = 10.0;
pub const ROAD_LOAD_C_NS2_PER_M2: f64 = 0.5;

// --- Breathing ---
pub const IDLE_MANIFOLD_PA: f64 = 23_000.0;
pub const MIN_CLOSED_THROTTLE_MAP_PA: f64 = 8_000.0;
pub const EXHAUST_BACKPRESSURE_COEFF: f64 = 0.021;
pub const VE_CLAMP_MIN: f64 = 0.1;
pub const VE_CLAMP_MAX: f64 = 1.15;

// --- Cooling ---
pub const OPERATING_COOLANT_K: f64 = 363.0;
pub const OVERHEAT_WARN_K: f64 = 383.0;
pub const OVERHEAT_SEVERE_K: f64 = 403.0;
pub const COOLANT_HEAT_FRACTION: f64 = 0.35;
pub const IDLE_HEAT_FLOOR_W: f64 = 800.0;
pub const COOLING_BASE_W_PER_K: f64 = 55.0;
pub const COOLING_RPM_REF: f64 = 6000.0;
pub const COOLING_SPEED_REF_MPS: f64 = 20.0;
pub const COLD_FRICTION_MULT_MAX: f64 = 1.15;
pub const COLD_FRICTION_REF_K: f64 = AIR_TEMP_MIN_K;
pub const OVERHEAT_FRICTION_BONUS: f64 = 0.15;

// --- Diesel ---
pub const DIESEL_IDLE_AFR: f64 = 55.0;
pub const DIESEL_MIN_AFR: f64 = 21.0;
pub const DIESEL_MAX_AFR: f64 = 200.0;
pub const DIESEL_GOVERNOR_LEAN_GAIN: f64 = 0.3;
pub const DIESEL_RATED_SPEED_MARGIN_RPM: f64 = 1700.0;
pub const DIESEL_HIGH_SPEED_LEAN_GAIN: f64 = 0.0045;

// --- EGT ---
pub const EGT_BASE_SPARK: f64 = 350.0;
pub const EGT_LOAD_SPARK: f64 = 300.0;
pub const EGT_BASE_DIESEL: f64 = 170.0;
pub const EGT_LOAD_DIESEL: f64 = 550.0;

/// Fraction of volumetric efficiency lost at redline per unit of exhaust backpressure multiplier
/// above 1 (gained when the exhaust flows more freely than stock).
pub const EXHAUST_VE_LOSS: f64 = 0.025;
/// Extra exhaust gas temperature per bar of boost beyond what the stock turbo makes (kelvin).
pub const EGT_PER_EXTRA_BOOST_BAR: f64 = 70.0;

// --- Knock ---
pub const KNOCK_TEMP_EXPONENT: i32 = 4;
pub const KNOCK_SENSITIVITY: f64 = 0.5;
pub const KNOCK_RETARD_MAX_AFR_LEAN: f64 = 3.0;

// --- Fuel system ---
pub const DEFAULT_FUEL_TANK_L: f64 = 60.0;
pub const FUEL_LOW_FRACTION: f64 = 0.15;
pub const FUEL_STARVED_AFR: f64 = 999.0;

// --- Ignition faults ---
pub const SPARK_PLUG_IDEAL_GAP_MM: f64 = 1.1;
pub const SPARK_PLUG_GAP_TOLERANCE_MM: f64 = 0.25;
pub const SPARK_PLUG_GAP_MISFIRE_PER_MM: f64 = 0.7;
pub const SPARK_PLUG_FOULING_SENS: f64 = 0.95;
pub const SPARK_PLUG_WEAR_SENS: f64 = 0.5;
pub const COIL_WEAK_SENS: f64 = 0.9;
pub const COIL_REF_PRESSURE_PA: f64 = 3_000_000.0;
pub const CONNECTOR_CORROSION_SENS: f64 = 0.85;

// --- Backfire ---
pub const BACKFIRE_THRESHOLD_K: f64 = 900.0;
pub const BACKFIRE_SPREAD_K: f64 = 200.0;
pub const BACKFIRE_MAX_PROB: f64 = 0.65;

// --- Exhaust ---
pub const CATALYST_LIGHT_OFF_K: f64 = 573.0;
pub const CATALYST_TAU_S: f64 = 25.0;
pub const CATALYST_CLOG_BACKPRESSURE_MULT: f64 = 3.0;
pub const CATALYST_BACKFIRE_DAMAGE: f64 = 0.03;
pub const CATALYST_DAMAGE_HEAT_K: f64 = 1050.0;
pub const O2_TAU_S: f64 = 0.15;
pub const CLOSED_LOOP_MAX_THROTTLE: f64 = 0.6;
pub const CLOSED_LOOP_MIN_COOLANT_K: f64 = 340.0;
pub const FUEL_TRIM_GAIN_PER_S: f64 = 0.4;
pub const FUEL_TRIM_MAX_ABS_AFR: f64 = 2.5;

// --- ECU monitors ---
pub const ECU_BASELINE_WINDOW_SEGMENTS: usize = 8;
pub const ECU_BASELINE_MIN_SAMPLES: usize = 3;
pub const ECU_MISFIRE_MONITOR_ARM_DELAY_S: f64 = 2.0;
pub const ECU_MISFIRE_SPEED_DROP_THRESHOLD: f64 = 0.04;
pub const ECU_MISFIRE_SEVERE_SPEED_DROP_THRESHOLD: f64 = 0.10;
pub const ECU_MISFIRE_THROTTLE_HOLDOFF_S: f64 = 1.5;
pub const ECU_MISFIRE_RATE_WINDOW_S: f64 = 10.0;
pub const ECU_MISFIRE_STORE_COUNT: usize = 3;
pub const ECU_MISFIRE_ACTIVE_FLASH_WINDOW_S: f64 = 1.5;
pub const ECU_FAULT_CLEAR_CLEAN_RUN_S: f64 = 20.0;
pub const ECU_O2_CIRCUIT_FAULT_CONFIRM_S: f64 = 1.0;
pub const ECU_O2_ACTIVITY_WINDOW_S: f64 = 8.0;
pub const ECU_O2_MIN_SWITCHING_RANGE_AFR: f64 = 0.3;
pub const ECU_O2_ACTIVITY_MIN_COVERAGE_S: f64 = 6.0;
pub const ECU_O2_INACTIVITY_FAULT_CONFIRM_S: f64 = 6.0;
pub const ECU_FUEL_TRIM_FAULT_FRACTION: f64 = 0.9;
pub const ECU_FUEL_TRIM_FAULT_CONFIRM_S: f64 = 8.0;
pub const ECU_EVAP_TEST_TARGET_VACUUM_PA: f64 = -3000.0;
pub const ECU_EVAP_TEST_DRAW_DOWN_MAX_S: f64 = 20.0;
pub const ECU_EVAP_TEST_HOLD_DURATION_S: f64 = 15.0;
pub const ECU_EVAP_TEST_PASS_MAX_DECAY_PA: f64 = 300.0;
pub const ECU_EVAP_TEST_SMALL_LEAK_MAX_DECAY_PA: f64 = 2500.0;
pub const ECU_EVAP_TEST_MIN_FUEL_LEVEL_FRACTION: f64 = 0.15;
pub const ECU_EVAP_TEST_MAX_FUEL_LEVEL_FRACTION: f64 = 0.85;
pub const ECU_EVAP_MONITOR_MIN_READY_DURATION_S: f64 = 5.0;
pub const ECU_EVAP_MONITOR_RETEST_COOLDOWN_S: f64 = 30.0;

// --- EVAP system ---
pub const EVAP_PURGE_COMMANDED_FRACTION: f64 = 0.25;
pub const EVAP_PURGE_RICHEN_SENSITIVITY: f64 = 3.0;
pub const EVAP_LEAK_LEAN_SENSITIVITY: f64 = 4.0;
pub const DEFAULT_CANISTER_SATURATION: f64 = 0.3;
pub const EVAP_VENT_OPEN_TIME_CONSTANT_S: f64 = 0.5;
pub const EVAP_LEAK_CONDUCTANCE_MAX_PER_S: f64 = 0.5;
pub const EVAP_PURGE_VACUUM_COUPLING_GAIN_PER_S: f64 = 0.008;

/// Out of gear the crank also carries the flywheel, clutch and accessories, which the bare crank
/// inertia in the config leaves out (kg m^2 per litre of displacement). Without it a V8 blip takes
/// <0.5 s to redline.
pub const FREE_REV_EXTRA_INERTIA_PER_L: f64 = 0.07;
