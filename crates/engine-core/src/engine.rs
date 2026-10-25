//! The engine + virtual drivetrain: cylinders on a shared crank, a clutch/gearbox reflecting
//! vehicle mass back onto it, turbo, cooling, finite fuel tank and closed-loop fuel trim.
//!
//! Ported from `combustion-engine.ts`. The ECU misfire monitor and EVAP system are not ported yet.

use crate::config::*;
use crate::constants::*;
use crate::cylinder::{Cylinder, CylinderInputs, CylinderTelemetry};
use crate::ecu::{CelState, Ecu, EvapMonitorCommand, EvapMonitorStatus};
use crate::evap::EvapSystem;
use crate::exhaust::ExhaustSystem;
use crate::rng::Rng;
use crate::tuning::{Tuning, TurboSetup};
use std::f64::consts::PI;

const RAD_PER_SEC_TO_RPM: f64 = 60.0 / (2.0 * PI);
const WATTS_PER_HP: f64 = 745.7;
const NM_PER_LB_FT: f64 = 1.35582;
const DEG_PER_RAD: f64 = 180.0 / PI;

/// Discrete things that happened during an `update`, drained by the caller (for audio/visual FX).
#[derive(Clone, Debug, Default)]
pub struct EngineEvents {
    pub fired: Vec<u32>,
    pub misfired: Vec<u32>,
    pub backfired: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct ExhaustTelemetry {
    pub catalyst_temp_k: f64,
    pub catalyst_lit_off: bool,
    pub catalyst_clog_fraction: f64,
    pub catalyst_damage_fraction: f64,
    pub backpressure_multiplier: f64,
    pub o2_reported_afr: f64,
    pub closed_loop_active: bool,
    pub fuel_trim_afr: f64,
}

#[derive(Clone, Debug)]
pub struct EvapTelemetry {
    pub purge_valve_open_fraction: f64,
    pub canister_saturation: f64,
    pub leak_fraction: f64,
    /// Signed AFR points currently contributed by purge + leak.
    pub afr_adjustment: f64,
    /// Gauge Pa (0 = atmospheric).
    pub tank_pressure_pa: f64,
}

#[derive(Clone, Debug)]
pub struct EngineTelemetry {
    pub rpm: f64,
    /// Total crank angle, wrapped to 0..720 (one full four-stroke cycle).
    pub crank_angle_deg: f64,
    pub throttle: f64,
    pub brake: f64,
    pub manifold_pressure_pa: f64,
    pub air_fuel_ratio: f64,
    pub indicated_torque_nm: f64,
    pub friction_torque_nm: f64,
    pub brake_torque_nm: f64,
    pub horsepower: f64,
    pub display_torque_nm: f64,
    pub display_torque_lb_ft: f64,
    pub display_horsepower: f64,
    pub is_running: bool,
    pub is_cranking: bool,
    pub is_rev_limiting: bool,
    pub is_shifting: bool,
    pub cylinders: Vec<CylinderTelemetry>,
    pub air_temperature_k: f64,
    pub atmospheric_pressure_pa: f64,
    pub vehicle_mass_kg: f64,
    pub gear_label: String,
    pub gear_ratio: f64,
    pub total_gear_ratio: f64,
    /// 0 = clutch fully open, 1 = fully engaged (rigid coupling).
    pub clutch_engagement: f64,
    pub vehicle_speed_mps: f64,
    pub redline_rpm: f64,
    pub coolant_temp_k: f64,
    pub is_overheating: bool,
    pub exhaust_gas_temp_k: f64,
    pub boost_pressure_pa: f64,
    pub turbo_spool_fraction: f64,
    pub knock_intensity: f64,
    pub is_knocking: bool,
    pub is_misfiring: bool,
    pub fuel_level_l: f64,
    pub fuel_tank_capacity_l: f64,
    pub fuel_level_fraction: f64,
    pub fuel_consumption_l_per_hour: f64,
    pub is_out_of_fuel: bool,
    pub is_low_fuel: bool,
    pub exhaust: ExhaustTelemetry,
    pub evap: EvapTelemetry,
    /// Turbochargers fitted (0 = naturally aspirated).
    pub turbo_count: u8,
    /// Wastegate target (Pa gauge) and the rpm where the compressors start to run out of flow.
    pub boost_target_pa: f64,
    pub turbo_choke_rpm: f64,
    /// Intake charge temperature after filter / intercooler effects.
    pub charge_temp_k: f64,
    /// Check-engine light, driven by the ECU's own crank-speed misfire monitor and sensor monitors.
    pub cel_state: CelState,
    pub evap_monitor_status: EvapMonitorStatus,
}

pub struct CombustionEngine {
    config: EngineConfig,
    cylinders: Vec<Cylinder>,
    rng: Rng,

    angular_velocity: f64,
    unwrapped_crank_angle_deg: f64,
    throttle_target: f64,
    throttle_actual: f64,
    misfire_holdoff_s: f64,
    brake_input: f64,
    external_load_torque_nm: f64,

    running: bool,
    cranking: bool,
    /// How long the starter must turn the engine over before spark/fuel kick in (0 = instantly).
    ignition_holdoff_s: f64,
    crank_time_s: f64,
    /// Fast-idle flare after light-off: 1 as the first cylinders fire, decaying away (0 when the engine never waited).
    start_flare: f64,
    rev_limiting: bool,
    below_stall_duration_s: f64,

    air_temperature_k: f64,
    vehicle_mass_kg: f64,
    brake_max_force_n: f64,
    gear_label: String,
    gear_ratio: f64,
    final_drive_ratio: f64,
    vehicle_speed_mps: f64,

    is_shifting: bool,
    shift_from: f64,
    shift_to: f64,
    shift_elapsed_s: f64,
    brake_declutched: bool,

    last_indicated_torque_nm: f64,
    last_friction_torque_nm: f64,
    last_manifold_pressure_pa: f64,
    last_afr: f64,
    smoothed_brake_torque_nm: f64,

    coolant_temp_k: f64,
    turbo_spool_fraction: f64,
    last_boost_pa: f64,
    last_egt_k: f64,
    last_knock_intensity: f64,

    fuel_tank_capacity_l: f64,
    fuel_remaining_kg: f64,
    smoothed_fuel_consumption_l_per_hour: f64,
    fuel: FuelConfig,

    pub exhaust: ExhaustSystem,
    pub evap: EvapSystem,
    tuning: Tuning,
    turbo_setup: Option<TurboSetup>,
    last_charge_temp_k: f64,
    ecu: Ecu,
    last_purge_commanded_fraction: f64,
    last_evap_afr_adjustment: f64,
    fuel_trim_afr: f64,
    last_closed_loop_active: bool,

    events: EngineEvents,
    pinned_angular_velocity: Option<f64>,
    last_frame_avg_brake_torque_nm: f64,
}

impl CombustionEngine {
    pub fn new(config: EngineConfig, transmission: &TransmissionConfig, brake: &BrakeConfig, fuel: FuelConfig) -> Self {
        let tank_l = config.fuel_tank_capacity_l.unwrap_or(DEFAULT_FUEL_TANK_L);
        let mut cylinders = Vec::with_capacity(config.cylinder_count);
        let mut cumulative = 0.0;
        for i in 0..config.cylinder_count {
            cylinders.push(Cylinder::new(&config, config.firing_order[i], cumulative));
            cumulative += config.firing_intervals_deg[i];
        }
        let mut exhaust = ExhaustSystem::new();
        exhaust.o2.reported_afr = fuel.stoichiometric_afr;
        exhaust.o2.stuck_afr = fuel.stoichiometric_afr;
        let mut engine = Self {
            cylinders,
            rng: Rng::new(0x9E3779B97F4A7C15),
            angular_velocity: 0.0,
            unwrapped_crank_angle_deg: 0.0,
            throttle_target: 0.0,
            throttle_actual: 0.0,
            misfire_holdoff_s: 0.0,
            brake_input: 0.0,
            external_load_torque_nm: 0.0,
            running: false,
            cranking: false,
            ignition_holdoff_s: 0.0,
            crank_time_s: 0.0,
            start_flare: 0.0,
            rev_limiting: false,
            below_stall_duration_s: 0.0,
            air_temperature_k: DEFAULT_AIR_TEMP_K,
            vehicle_mass_kg: DEFAULT_VEHICLE_MASS_KG,
            brake_max_force_n: brake.max_force_n,
            gear_label: "N".to_string(),
            gear_ratio: 0.0,
            final_drive_ratio: transmission.final_drive_ratio,
            vehicle_speed_mps: 0.0,
            is_shifting: false,
            shift_from: 0.0,
            shift_to: 0.0,
            shift_elapsed_s: 0.0,
            brake_declutched: false,
            last_indicated_torque_nm: 0.0,
            last_friction_torque_nm: 0.0,
            last_manifold_pressure_pa: DEFAULT_ATMOSPHERIC_PA,
            last_afr: fuel.stoichiometric_afr,
            smoothed_brake_torque_nm: 0.0,
            coolant_temp_k: DEFAULT_AIR_TEMP_K,
            turbo_spool_fraction: 0.0,
            last_boost_pa: 0.0,
            last_egt_k: DEFAULT_AIR_TEMP_K,
            last_knock_intensity: 0.0,
            fuel_tank_capacity_l: tank_l,
            fuel_remaining_kg: tank_l * fuel.density_kg_per_l,
            smoothed_fuel_consumption_l_per_hour: 0.0,
            fuel,
            exhaust,
            evap: EvapSystem::default(),
            tuning: Tuning::default(),
            turbo_setup: None,
            last_charge_temp_k: DEFAULT_AIR_TEMP_K,
            ecu: Ecu::new(config.cylinder_count),
            last_purge_commanded_fraction: 0.0,
            last_evap_afr_adjustment: 0.0,
            fuel_trim_afr: 0.0,
            last_closed_loop_active: false,
            events: EngineEvents::default(),
            pinned_angular_velocity: None,
            last_frame_avg_brake_torque_nm: 0.0,
            config,
        };
        engine.set_tuning(Tuning::default());
        engine
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn tuning(&self) -> &Tuning {
        &self.tuning
    }

    /// Bolts on a new set of intake / exhaust / turbo parts. A default `Tuning` is the stock engine.
    pub fn set_tuning(&mut self, tuning: Tuning) {
        self.turbo_setup = tuning.turbo_setup(&self.config);
        self.exhaust.mod_flow = tuning.exhaust_flow_multiplier();
        self.exhaust.catalyst.deleted = tuning.is_cat_deleted();
        if self.turbo_setup.is_none() {
            self.turbo_spool_fraction = 0.0;
            self.last_boost_pa = 0.0;
        }
        self.tuning = tuning;
    }

    // ---- inputs ----
    pub fn set_throttle(&mut self, v: f64) {
        self.throttle_target = v.clamp(0.0, 1.0);
    }
    pub fn set_brake(&mut self, v: f64) {
        self.brake_input = v.clamp(0.0, 1.0);
    }
    pub fn set_dyno_load(&mut self, torque_nm: f64) {
        self.external_load_torque_nm = torque_nm.max(0.0);
    }
    pub fn set_air_temperature(&mut self, kelvin: f64) {
        self.air_temperature_k = kelvin.clamp(AIR_TEMP_MIN_K, AIR_TEMP_MAX_K);
    }
    pub fn set_vehicle_mass(&mut self, kg: f64) {
        self.vehicle_mass_kg = kg.clamp(VEHICLE_MASS_MIN_KG, VEHICLE_MASS_MAX_KG);
    }
    /// 0 = empty, 1 = full tank.
    pub fn set_fuel_level(&mut self, fraction: f64) {
        self.fuel_remaining_kg = fraction.clamp(0.0, 1.0) * self.fuel_tank_capacity_l * self.fuel.density_kg_per_l;
    }
    pub fn refuel(&mut self) {
        self.fuel_remaining_kg = self.fuel_tank_capacity_l * self.fuel.density_kg_per_l;
    }
    pub fn set_fuel(&mut self, fuel: FuelConfig) {
        let fraction = self.fuel_remaining_kg / (self.fuel_tank_capacity_l * self.fuel.density_kg_per_l);
        self.fuel_remaining_kg = fraction.clamp(0.0, 1.0) * self.fuel_tank_capacity_l * fuel.density_kg_per_l;
        self.fuel = fuel;
    }
    pub fn set_transmission(&mut self, t: &TransmissionConfig) {
        self.final_drive_ratio = t.final_drive_ratio;
        self.gear_label = "N".to_string();
        self.gear_ratio = 0.0;
        self.is_shifting = false;
    }
    pub fn set_brake_system(&mut self, b: &BrakeConfig) {
        self.brake_max_force_n = b.max_force_n;
    }

    pub fn set_gear(&mut self, label: &str, ratio: f64) {
        if ratio == self.gear_ratio {
            return;
        }
        let old_total = self.gear_ratio * self.final_drive_ratio;
        let new_total = ratio * self.final_drive_ratio;

        if old_total != 0.0 && !self.is_shifting {
            self.vehicle_speed_mps = (self.angular_velocity / old_total).abs() * WHEEL_RADIUS_M;
        }
        self.gear_label = label.to_string();
        self.gear_ratio = ratio;

        if old_total != 0.0 && new_total != 0.0 {
            self.shift_from = self.angular_velocity;
            self.shift_to = (self.vehicle_speed_mps / WHEEL_RADIUS_M) * new_total.abs();
            self.shift_elapsed_s = 0.0;
            self.is_shifting = true;
        } else {
            self.is_shifting = false;
        }
    }

    /// Engages the starter and enables spark/fuel. No-op while in gear (neutral safety switch).
    pub fn engage_starter(&mut self) {
        if self.gear_ratio != 0.0 {
            return;
        }
        if !self.cranking {
            self.crank_time_s = 0.0;
        }
        self.cranking = true;
        self.running = true;
        self.below_stall_duration_s = 0.0;
    }
    pub fn release_starter(&mut self) {
        // letting go before the engine has lit means it never caught
        if self.cranking && self.crank_time_s < self.ignition_holdoff_s {
            self.running = false;
        }
        self.cranking = false;
    }
    /// Seconds of cranking before spark/fuel are enabled, so the starter is audible on its own.
    pub fn set_ignition_holdoff(&mut self, seconds: f64) {
        self.ignition_holdoff_s = seconds.max(0.0);
    }
    /// True once the engine is cranking but has not lit yet.
    pub fn is_cranking_unlit(&self) -> bool {
        self.cranking && self.crank_time_s < self.ignition_holdoff_s
    }
    pub fn shut_off(&mut self) {
        self.running = false;
        self.cranking = false;
        self.below_stall_duration_s = 0.0;
        self.ecu.on_engine_stopped();
    }

    /// Which ECU monitors hold a fault: (misfire, o2, fuel trim, evap).
    pub fn ecu_fault_flags(&self) -> (bool, bool, bool, bool) {
        self.ecu.fault_flags()
    }

    /// Scan-tool "clear codes": clears every stored ECU fault. Underlying hardware faults return.
    pub fn clear_cel(&mut self) {
        self.ecu.clear_stored_fault();
    }

    /// Cheap per-cylinder sample (no allocation) for fine-grained recording by a renderer.
    pub fn cylinder_sample(&self, i: usize) -> crate::cylinder::CylinderSample {
        self.cylinders[i].sample()
    }

    pub fn cylinder_count(&self) -> usize {
        self.cylinders.len()
    }

    pub fn rpm(&self) -> f64 {
        self.angular_velocity * RAD_PER_SEC_TO_RPM
    }

    pub fn take_events(&mut self) -> EngineEvents {
        std::mem::take(&mut self.events)
    }

    // ---- ignition / exhaust fault injection (by cylinder index in physical order) ----
    pub fn cylinder_ignition_mut(&mut self, label: u32) -> Option<&mut crate::ignition::IgnitionChannel> {
        self.cylinders.iter_mut().find(|c| c.label == label).map(|c| &mut c.ignition)
    }
    pub fn set_catalyst_clog(&mut self, fraction: f64) {
        self.exhaust.catalyst.clog_fraction = fraction.clamp(0.0, 1.0);
    }
    pub fn set_muffler_restriction(&mut self, multiplier: f64) {
        self.exhaust.muffler_restriction = multiplier.max(0.0);
    }

    // ---- model helpers ----
    /// The breathing curve: the stock hump, reshaped by the manifold runners, then scaled by the
    /// inlet tract, header scavenging and how freely the exhaust flows.
    fn volumetric_efficiency_at(&self, rpm: f64) -> f64 {
        let c = &self.config;
        let t = &self.tuning;
        let x = (rpm - t.ve_peak_rpm(c.ve_peak_rpm)) / t.ve_spread_rpm(c.ve_spread_rpm);
        let curve = t.ve_base(c.ve_base) + t.ve_peak_bonus(c.ve_peak_bonus) * (-x * x).exp();
        if t.is_stock() && self.exhaust.backpressure_multiplier() == 1.0 {
            return curve.clamp(VE_CLAMP_MIN, VE_CLAMP_MAX);
        }
        // A restricted exhaust (clogged cat, crushed muffler) costs breathing at high rpm; a free
        // flowing one gains a little. Scales with rpm^2 like the flow itself.
        let r = (rpm / c.redline_rpm).clamp(0.0, 1.2);
        let exhaust = (1.0 - EXHAUST_VE_LOSS * (self.exhaust.backpressure_multiplier().clamp(0.25, 6.0) - 1.0) * r * r).clamp(0.7, 1.05);
        // clamp the stock part first so the V12's saturated curve is unchanged, then apply mods
        curve.clamp(VE_CLAMP_MIN, VE_CLAMP_MAX) * t.ve_multiplier(c, rpm) * exhaust
    }

    fn closed_throttle_manifold_pressure_at(&self, rpm: f64) -> f64 {
        if rpm <= IDLE_RPM {
            return IDLE_MANIFOLD_PA;
        }
        MIN_CLOSED_THROTTLE_MAP_PA.max(IDLE_MANIFOLD_PA - (rpm - IDLE_RPM) * 60.0)
    }

    fn manifold_pressure_for(&self, throttle: f64, rpm: f64, atmospheric_pa: f64, boost_pa: f64) -> f64 {
        let ve = self.volumetric_efficiency_at(rpm);
        let wot = atmospheric_pa * ve.clamp(VE_CLAMP_MIN, VE_CLAMP_MAX * 1.2) + boost_pa;
        let closed = self.closed_throttle_manifold_pressure_at(rpm);
        closed + (wot - closed) * throttle
    }

    fn friction_temp_multiplier_at(&self, coolant_k: f64) -> f64 {
        if coolant_k < OPERATING_COOLANT_K {
            let cold = ((OPERATING_COOLANT_K - coolant_k) / (OPERATING_COOLANT_K - COLD_FRICTION_REF_K)).clamp(0.0, 1.0);
            return 1.0 + (COLD_FRICTION_MULT_MAX - 1.0) * cold;
        }
        if coolant_k > OVERHEAT_SEVERE_K {
            let hot = ((coolant_k - OVERHEAT_SEVERE_K) / 20.0).clamp(0.0, 1.0);
            return 1.0 + OVERHEAT_FRICTION_BONUS * hot;
        }
        1.0
    }

    fn cooling_effectiveness_w_per_k(&self, speed_mps: f64, rpm: f64) -> f64 {
        COOLING_BASE_W_PER_K * (1.0 + rpm / COOLING_RPM_REF) * (1.0 + speed_mps / COOLING_SPEED_REF_MPS)
    }

    fn idle_throttle_floor_at(&self, rpm: f64) -> f64 {
        (self.config.idle_throttle_floor + (IDLE_RPM - rpm) * IDLE_GOVERNOR_GAIN).clamp(0.0, IDLE_GOVERNOR_MAX_THROTTLE)
    }

    fn friction_torque_at(&self, w: f64) -> f64 {
        let c = &self.config;
        c.friction_base_nm + c.friction_viscous_coeff * w + c.friction_windage_coeff * w * w
    }

    fn starter_torque_at(&self, rpm: f64) -> f64 {
        let compression_floor = STARTER_MIN_NM_PER_L * self.config.displacement_l() * self.config.compression_ratio / 10.5;
        self.config.starter_torque_nm.max(compression_floor) * (1.0 - rpm / STARTER_MAX_RPM).max(0.0)
    }

    /// Boost the stock hardware could make right now (the part of boost that does not add EGT).
    fn stock_boost_pa(&self, boost_pa: f64) -> f64 {
        match &self.config.turbo {
            Some(t) => boost_pa.min(t.max_boost_pa),
            None => 0.0,
        }
    }

    fn fuel_level_fraction(&self) -> f64 {
        self.fuel_remaining_kg / (self.fuel_tank_capacity_l * self.fuel.density_kg_per_l)
    }

    /// Advances the simulation by `raw_dt` seconds, internally substepped for accuracy.
    pub fn update(&mut self, raw_dt: f64) {
        if raw_dt <= 0.0 {
            return;
        }

        let angular_speed_deg_per_s = self.angular_velocity.abs() * DEG_PER_RAD;
        let sub_dt = if angular_speed_deg_per_s > 0.0 {
            (SUBSTEP_TARGET_DEG / angular_speed_deg_per_s).clamp(1.0 / SUBSTEP_HZ, MAX_SUBSTEP_S)
        } else {
            MAX_SUBSTEP_S
        };
        let dt = raw_dt.min(MAX_SUBSTEPS_PER_FRAME * sub_dt);

        let rate = self.config.throttle_response_rate.unwrap_or(THROTTLE_ACTUATOR_RATE);
        let throttle_before = self.throttle_actual;
        self.throttle_actual += (self.throttle_target - self.throttle_actual).clamp(-rate * dt, rate * dt);
        // A moving throttle makes the crank speed rise or fall on its own; a real ECU suspends its
        // misfire monitor through that transient and a short settling period after it.
        if (self.throttle_actual - throttle_before).abs() > 1.0e-6 {
            self.misfire_holdoff_s = ECU_MISFIRE_THROTTLE_HOLDOFF_S;
        } else {
            self.misfire_holdoff_s = (self.misfire_holdoff_s - dt).max(0.0);
        }

        let substeps = (dt / sub_dt).ceil().max(1.0) as usize;
        // Exact substep so any call length integrates exactly `dt` of simulated time.
        let sub_dt = dt / substeps as f64;
        let atmospheric_pa = AIR_DENSITY_REFERENCE * GAS_CONSTANT_AIR * self.air_temperature_k;
        let rpm_at_start = self.angular_velocity * RAD_PER_SEC_TO_RPM;

        // Turbo
        let mut boost_pa = 0.0;
        if let Some(turbo) = self.turbo_setup {
            let target = (rpm_at_start / turbo.full_spool_rpm).clamp(0.0, 1.0) * self.throttle_actual;
            let alpha = 1.0 - (-dt / turbo.spool_tau_s).exp();
            self.turbo_spool_fraction += (target - self.turbo_spool_fraction) * alpha;
            boost_pa = turbo.max_boost_pa * self.turbo_spool_fraction * Tuning::flow_droop(&turbo, rpm_at_start);
        }
        self.last_boost_pa = boost_pa;
        // Charge temperature: compressor heat the intercooler leaves behind, plus the inlet tract.
        let charge_temp_k = self.air_temperature_k + self.tuning.charge_temp_delta_k(boost_pa);
        self.last_charge_temp_k = charge_temp_k;

        // Knock
        let mut knock_intensity = 0.0;
        if self.config.ignition_type == IgnitionType::Spark {
            if let Some(threshold) = self.config.knock_threshold {
                let representative = self.manifold_pressure_for(
                    self.throttle_actual.max(self.idle_throttle_floor_at(rpm_at_start)),
                    rpm_at_start,
                    atmospheric_pa,
                    boost_pa,
                );
                let risk = (representative / atmospheric_pa)
                    * (self.coolant_temp_k / OPERATING_COOLANT_K).powi(KNOCK_TEMP_EXPONENT)
                    * (charge_temp_k / self.air_temperature_k).powi(2);
                let effective_threshold = threshold * self.fuel.knock_resistance;
                knock_intensity = ((risk - effective_threshold) / KNOCK_SENSITIVITY).clamp(0.0, 1.0);
            }
        }
        self.last_knock_intensity = knock_intensity;

        // Cooling
        let friction_temp_mult = self.friction_temp_multiplier_at(self.coolant_temp_k);
        let heat_in_w = self.last_indicated_torque_nm.max(0.0) * self.angular_velocity * COOLANT_HEAT_FRACTION
            + if self.running { IDLE_HEAT_FLOOR_W } else { 0.0 };
        let heat_out_w = self.cooling_effectiveness_w_per_k(self.vehicle_speed_mps, rpm_at_start)
            * (self.coolant_temp_k - self.air_temperature_k);
        self.coolant_temp_k += (heat_in_w - heat_out_w) / self.config.thermal_mass_j_per_k * dt;

        // EGT (flavor)
        let is_compression = self.config.ignition_type == IgnitionType::Compression;
        self.last_egt_k = self.air_temperature_k
            + if is_compression { EGT_BASE_DIESEL } else { EGT_BASE_SPARK }
            + if is_compression { EGT_LOAD_DIESEL } else { EGT_LOAD_SPARK } * self.throttle_actual
            + EGT_PER_EXTRA_BOOST_BAR * ((boost_pa - self.stock_boost_pa(boost_pa)) / 1.0e5).max(0.0);

        self.exhaust.update(dt, self.last_egt_k, self.last_afr);

        // Closed-loop fuel trim
        let closed_loop = !is_compression
            && self.running
            && !self.cranking
            && self.throttle_actual <= CLOSED_LOOP_MAX_THROTTLE
            && self.coolant_temp_k >= CLOSED_LOOP_MIN_COOLANT_K;
        self.last_closed_loop_active = closed_loop;
        let mut trim_target = 0.0;
        if closed_loop && self.exhaust.o2.is_healthy() {
            trim_target = (-(self.exhaust.o2.reported_afr - self.fuel.stoichiometric_afr))
                .clamp(-FUEL_TRIM_MAX_ABS_AFR, FUEL_TRIM_MAX_ABS_AFR);
        }
        let trim_alpha = 1.0 - (-FUEL_TRIM_GAIN_PER_S * dt).exp();
        self.fuel_trim_afr += (trim_target - self.fuel_trim_afr) * trim_alpha;

        // O2 circuit/activity and fuel-trim monitors (spark engines only)
        if !is_compression {
            self.ecu.update_sensors(dt, closed_loop, self.exhaust.o2.connected, self.exhaust.o2.reported_afr, self.fuel_trim_afr);
        }

        // EVAP purge normally runs on the ECU's own schedule; its leak-check test can override it.
        let normal_purge = if closed_loop { EVAP_PURGE_COMMANDED_FRACTION } else { 0.0 };
        let fuel_level_fraction = self.fuel_level_fraction();
        let evap_ready = closed_loop
            && fuel_level_fraction >= ECU_EVAP_TEST_MIN_FUEL_LEVEL_FRACTION
            && fuel_level_fraction <= ECU_EVAP_TEST_MAX_FUEL_LEVEL_FRACTION;
        let evap_cmd = if !is_compression {
            self.ecu.update_evap_monitor(dt, evap_ready, self.evap.tank_pressure_pa(), normal_purge)
        } else {
            EvapMonitorCommand { vent_closed_commanded: false, purge_commanded_fraction: 0.0 }
        };
        self.last_purge_commanded_fraction = evap_cmd.purge_commanded_fraction;
        let evap_afr_adjustment = if !is_compression {
            self.evap.purge_afr_adjustment(evap_cmd.purge_commanded_fraction)
                + if self.running { self.evap.leak_afr_adjustment() } else { 0.0 }
        } else {
            0.0
        };
        self.last_evap_afr_adjustment = evap_afr_adjustment;
        if !is_compression {
            let manifold_vacuum = self.last_manifold_pressure_pa - atmospheric_pa;
            self.evap.update_tank_pressure(dt, manifold_vacuum, evap_cmd.vent_closed_commanded, evap_cmd.purge_commanded_fraction);
        }

        let backpressure_mult = self.exhaust.backpressure_multiplier();
        let total_gear_ratio = self.gear_ratio * self.final_drive_ratio;
        let in_gear = total_gear_ratio != 0.0;
        let rev_cut = self.config.redline_rpm;
        let rev_resume = self.config.redline_rpm - REV_LIMITER_HYSTERESIS_RPM;

        let mut brake_torque_sum = 0.0;
        let mut fuel_burned_kg = 0.0;

        for _ in 0..substeps {
            if self.is_shifting {
                self.shift_elapsed_s += sub_dt;
                let f = (self.shift_elapsed_s / CLUTCH_SLIP_DURATION_S).clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - f) * (1.0 - f);
                self.angular_velocity = self.shift_from + (self.shift_to - self.shift_from) * eased;
                if f >= 1.0 {
                    self.is_shifting = false;
                }
            }

            if let Some(w) = self.pinned_angular_velocity {
                self.angular_velocity = w;
            }
            let rpm = self.angular_velocity * RAD_PER_SEC_TO_RPM;
            if rpm >= rev_cut {
                self.rev_limiting = true;
            } else if rpm <= rev_resume {
                self.rev_limiting = false;
            }
            if self.cranking {
                let before = self.crank_time_s;
                self.crank_time_s += sub_dt;
                // light-off: the first firings come with a cold-start enrichment and a throttle-plate
                // opening that flare the engine well above idle before the idle control gets it back
                if self.ignition_holdoff_s > 0.0 && before < self.ignition_holdoff_s && self.crank_time_s >= self.ignition_holdoff_s {
                    self.start_flare = 1.0;
                }
            }
            if self.start_flare > 0.0 {
                self.start_flare *= (-sub_dt / START_FLARE_TAU_S).exp();
                if self.start_flare < 1.0e-3 || !self.running {
                    self.start_flare = 0.0;
                }
            }
            let ignition_enabled = self.running && !self.rev_limiting && !self.is_cranking_unlit();
            let effective_throttle = if self.running {
                self.throttle_actual.max((self.idle_throttle_floor_at(rpm) + START_FLARE_THROTTLE * self.start_flare).min(0.9))
            } else {
                0.0
            };

            let (mut afr, manifold_pa);
            if is_compression {
                manifold_pa = atmospheric_pa + boost_pa;
                let idle_authority = (1.0 - self.throttle_actual / IDLE_GOVERNOR_MAX_THROTTLE).clamp(0.0, 1.0);
                let governor_lean = ((rpm - IDLE_RPM) * DIESEL_GOVERNOR_LEAN_GAIN).max(0.0) * idle_authority;
                let high_speed_lean = (rpm - (self.config.redline_rpm - DIESEL_RATED_SPEED_MARGIN_RPM)).max(0.0)
                    * DIESEL_HIGH_SPEED_LEAN_GAIN;
                afr = (DIESEL_IDLE_AFR - (DIESEL_IDLE_AFR - DIESEL_MIN_AFR) * effective_throttle
                    + governor_lean
                    + high_speed_lean)
                    .clamp(DIESEL_MIN_AFR, DIESEL_MAX_AFR);
            } else {
                let enrichment = effective_throttle * effective_throttle;
                let knock_lean = knock_intensity * KNOCK_RETARD_MAX_AFR_LEAN;
                afr = (self.fuel.stoichiometric_afr - (self.fuel.stoichiometric_afr - self.fuel.wot_afr) * enrichment
                    + knock_lean
                    + evap_afr_adjustment
                    + self.fuel_trim_afr)
                    .clamp(SPARK_AFR_SAFETY_MIN, SPARK_AFR_SAFETY_MAX);
                manifold_pa = self.manifold_pressure_for(effective_throttle, rpm, atmospheric_pa, boost_pa);
            }
            if self.fuel_remaining_kg <= 0.0 {
                afr = FUEL_STARVED_AFR;
            }

            let new_angle = self.unwrapped_crank_angle_deg + self.angular_velocity * sub_dt * DEG_PER_RAD;

            let inputs = CylinderInputs {
                unwrapped_crank_angle_deg: new_angle,
                manifold_pressure_pa: manifold_pa,
                air_fuel_ratio: afr,
                ignition_enabled,
                angular_velocity: self.angular_velocity,
                atmospheric_pressure_pa: atmospheric_pa,
                exhaust_backpressure_multiplier: backpressure_mult,
                intake_air_temp_k: charge_temp_k,
                exhaust_gas_temp_k: self.last_egt_k,
                fuel_lhv: self.fuel.lower_heating_value,
            };
            let mut indicated = 0.0;
            for cyl in self.cylinders.iter_mut() {
                indicated += cyl.update(&inputs, &mut self.rng);
                if cyl.just_ignited() {
                    self.events.fired.push(cyl.label);
                }
                if cyl.just_misfired() {
                    self.events.misfired.push(cyl.label);
                }
                if cyl.just_backfired() {
                    self.events.backfired.push(cyl.label);
                    self.exhaust.catalyst.register_backfire();
                }
                fuel_burned_kg += cyl.fuel_metered_this_call_kg();
            }

            let friction = self.friction_torque_at(self.angular_velocity) * friction_temp_mult;
            let starter = if self.cranking && rpm < STARTER_DISENGAGE_RPM { self.starter_torque_at(rpm) } else { 0.0 };

            // Automatic clutch at idle under brake (stand-in for a driver covering the clutch).
            if self.brake_declutched
                && (self.brake_input <= 0.0 || rpm > IDLE_RPM + BRAKE_DECLUTCH_REENGAGE_HYSTERESIS_RPM || !in_gear)
            {
                if in_gear && !self.is_shifting {
                    self.shift_from = self.angular_velocity;
                    self.shift_to = (IDLE_RPM / RAD_PER_SEC_TO_RPM)
                        .max((self.vehicle_speed_mps / WHEEL_RADIUS_M) * total_gear_ratio.abs());
                    self.shift_elapsed_s = 0.0;
                    self.is_shifting = true;
                }
                self.brake_declutched = false;
            } else if !self.brake_declutched && in_gear && !self.is_shifting && self.brake_input > 0.0 && rpm <= IDLE_RPM {
                self.brake_declutched = true;
            }

            let rigidly_coupled = in_gear && !self.is_shifting && !self.brake_declutched;
            let brake_force_n = self.brake_max_force_n * self.brake_input;
            let speed = self.vehicle_speed_mps;
            let road_load = |v: f64| ROAD_LOAD_A_N + ROAD_LOAD_B_NS_PER_M * v + ROAD_LOAD_C_NS2_PER_M2 * v * v;

            let mut effective_inertia = self.config.rotating_inertia_kg_m2
                + if rigidly_coupled { 0.0 } else { FREE_REV_EXTRA_INERTIA_PER_L * self.config.displacement_l() };
            let mut gear_load_torque = 0.0;
            if rigidly_coupled {
                self.vehicle_speed_mps = (self.angular_velocity / total_gear_ratio).abs() * WHEEL_RADIUS_M;
                let rl = road_load(self.vehicle_speed_mps);
                gear_load_torque = (rl + brake_force_n) * WHEEL_RADIUS_M / total_gear_ratio.abs();
                effective_inertia += self.vehicle_mass_kg * WHEEL_RADIUS_M.powi(2) / total_gear_ratio.powi(2);
            } else {
                let rl = road_load(speed);
                self.vehicle_speed_mps =
                    (self.vehicle_speed_mps - (rl + brake_force_n) / self.vehicle_mass_kg * sub_dt).max(0.0);
            }

            let mut net = indicated - friction + starter - self.external_load_torque_nm - gear_load_torque;
            if self.rev_limiting && rpm >= rev_cut {
                net = net.min(0.0);
            }
            if !self.is_shifting && self.pinned_angular_velocity.is_none() {
                self.angular_velocity = (self.angular_velocity + net / effective_inertia * sub_dt).max(0.0);
            }
            self.unwrapped_crank_angle_deg = new_angle;

            // Misfire monitor reads only crank speed; gated off while cranking, on the rev limiter
            // (a commanded cut, not a fault), mid-shift (kinematic speed blend) and through throttle transients.
            let misfire_sampling =
                self.running && !self.cranking && !self.rev_limiting && !self.is_shifting && self.misfire_holdoff_s <= 0.0;
            self.ecu.update(misfire_sampling, self.angular_velocity, sub_dt);

            self.last_indicated_torque_nm = indicated;
            self.last_friction_torque_nm = friction;
            self.last_manifold_pressure_pa = manifold_pa;
            self.last_afr = afr;
            brake_torque_sum += indicated - friction;

            if self.running && !self.cranking {
                if self.angular_velocity * RAD_PER_SEC_TO_RPM < STALL_RPM {
                    self.below_stall_duration_s += sub_dt;
                    if self.below_stall_duration_s >= STALL_GRACE_S {
                        self.running = false;
                    }
                } else {
                    self.below_stall_duration_s = 0.0;
                }
            }
        }

        self.fuel_remaining_kg = (self.fuel_remaining_kg - fuel_burned_kg).max(0.0);

        let frame_avg = brake_torque_sum / substeps as f64;
        self.last_frame_avg_brake_torque_nm = frame_avg;
        let alpha = 1.0 - (-dt / DISPLAY_SMOOTHING_TAU_S).exp();
        self.smoothed_brake_torque_nm += (frame_avg - self.smoothed_brake_torque_nm) * alpha;
        let target_consumption = (fuel_burned_kg / dt) / self.fuel.density_kg_per_l * 3600.0;
        self.smoothed_fuel_consumption_l_per_hour +=
            (target_consumption - self.smoothed_fuel_consumption_l_per_hour) * alpha;
    }

    /// Test/dyno helper: warms the engine to operating temperature.
    pub fn warm_up(&mut self) {
        self.coolant_temp_k = OPERATING_COOLANT_K;
    }

    /// Dyno helper: pins the crank at `rpm` at wide-open throttle for `seconds` and returns the
    /// mean brake torque (Nm) over that window, averaged over many firing events.
    pub fn dyno_point(&mut self, rpm: f64, seconds: f64) -> f64 {
        self.running = true;
        self.cranking = false;
        self.throttle_target = 1.0;
        self.throttle_actual = 1.0;
        self.pinned_angular_velocity = Some(rpm / RAD_PER_SEC_TO_RPM);
        self.angular_velocity = rpm / RAD_PER_SEC_TO_RPM;
        // a dyno holds each point until the turbo is settled: start from the steady-state spool
        if let Some(turbo) = self.turbo_setup {
            self.turbo_spool_fraction = (rpm / turbo.full_spool_rpm).clamp(0.0, 1.0);
        }
        for _ in 0..20 {
            self.warm_up();
            self.update(0.05);
        }
        self.smoothed_brake_torque_nm = 0.0;
        let mut acc = 0.0;
        let mut n = 0.0;
        let step = 0.01;
        let mut t = 0.0;
        while t < seconds {
            self.warm_up();
            self.update(step);
            acc += self.smoothed_brake_torque_nm_frame_avg();
            n += 1.0;
            t += step;
        }
        self.pinned_angular_velocity = None;
        acc / n
    }

    /// Holds the crank at `rpm` and the throttle at `throttle` until the engine settles, and returns
    /// the telemetry: a steady operating point for sound analysis and for dyno-style sweeps.
    pub fn steady_point(&mut self, rpm: f64, throttle: f64) -> EngineTelemetry {
        self.running = true;
        self.cranking = false;
        self.throttle_target = throttle.clamp(0.0, 1.0);
        self.throttle_actual = self.throttle_target;
        self.pinned_angular_velocity = Some(rpm / RAD_PER_SEC_TO_RPM);
        self.angular_velocity = rpm / RAD_PER_SEC_TO_RPM;
        if let Some(turbo) = self.turbo_setup {
            self.turbo_spool_fraction = (rpm / turbo.full_spool_rpm).clamp(0.0, 1.0) * throttle;
        }
        for _ in 0..30 {
            self.warm_up();
            self.update(0.05);
        }
        self.pinned_angular_velocity = None;
        self.telemetry()
    }

    fn smoothed_brake_torque_nm_frame_avg(&self) -> f64 {
        self.last_frame_avg_brake_torque_nm
    }

    fn clutch_engagement(&self) -> f64 {
        if self.gear_ratio == 0.0 {
            return 0.0;
        }
        if self.brake_declutched {
            return 0.0;
        }
        if self.is_shifting {
            let f = (self.shift_elapsed_s / CLUTCH_SLIP_DURATION_S).clamp(0.0, 1.0);
            let t = ((f - 0.3) / 0.7).clamp(0.0, 1.0);
            return t * t * (3.0 - 2.0 * t);
        }
        1.0
    }

    pub fn telemetry(&self) -> EngineTelemetry {
        let rpm = self.angular_velocity * RAD_PER_SEC_TO_RPM;
        let brake_torque = self.last_indicated_torque_nm - self.last_friction_torque_nm;
        let watts = brake_torque * self.angular_velocity;
        let display_watts = self.smoothed_brake_torque_nm * self.angular_velocity;
        let cylinders: Vec<CylinderTelemetry> = self.cylinders.iter().map(|c| c.telemetry()).collect();
        let fraction = self.fuel_level_fraction();
        EngineTelemetry {
            rpm,
            crank_angle_deg: self.unwrapped_crank_angle_deg.rem_euclid(720.0),
            throttle: self.throttle_actual,
            brake: self.brake_input,
            manifold_pressure_pa: self.last_manifold_pressure_pa,
            air_fuel_ratio: self.last_afr,
            indicated_torque_nm: self.last_indicated_torque_nm,
            friction_torque_nm: self.last_friction_torque_nm,
            brake_torque_nm: brake_torque,
            horsepower: watts / WATTS_PER_HP,
            display_torque_nm: self.smoothed_brake_torque_nm,
            display_torque_lb_ft: self.smoothed_brake_torque_nm / NM_PER_LB_FT,
            display_horsepower: display_watts / WATTS_PER_HP,
            is_running: self.running,
            is_cranking: self.cranking,
            is_rev_limiting: self.rev_limiting,
            is_shifting: self.is_shifting,
            is_misfiring: self.running && cylinders.iter().any(|c| !c.spark_fired),
            cylinders,
            air_temperature_k: self.air_temperature_k,
            atmospheric_pressure_pa: AIR_DENSITY_REFERENCE * GAS_CONSTANT_AIR * self.air_temperature_k,
            vehicle_mass_kg: self.vehicle_mass_kg,
            gear_label: self.gear_label.clone(),
            gear_ratio: self.gear_ratio,
            total_gear_ratio: self.gear_ratio * self.final_drive_ratio,
            clutch_engagement: self.clutch_engagement(),
            vehicle_speed_mps: self.vehicle_speed_mps,
            redline_rpm: self.config.redline_rpm,
            coolant_temp_k: self.coolant_temp_k,
            is_overheating: self.coolant_temp_k > OVERHEAT_WARN_K,
            exhaust_gas_temp_k: self.last_egt_k,
            boost_pressure_pa: self.last_boost_pa,
            turbo_spool_fraction: self.turbo_spool_fraction,
            knock_intensity: self.last_knock_intensity,
            is_knocking: self.last_knock_intensity > 0.05,
            fuel_level_l: self.fuel_remaining_kg / self.fuel.density_kg_per_l,
            fuel_tank_capacity_l: self.fuel_tank_capacity_l,
            fuel_level_fraction: fraction,
            fuel_consumption_l_per_hour: self.smoothed_fuel_consumption_l_per_hour,
            is_out_of_fuel: self.fuel_remaining_kg <= 0.0,
            is_low_fuel: fraction <= FUEL_LOW_FRACTION,
            exhaust: ExhaustTelemetry {
                catalyst_temp_k: self.exhaust.catalyst.temperature_k,
                catalyst_lit_off: self.exhaust.catalyst.is_lit_off(),
                catalyst_clog_fraction: self.exhaust.catalyst.clog_fraction,
                catalyst_damage_fraction: self.exhaust.catalyst.damage_fraction,
                backpressure_multiplier: self.exhaust.backpressure_multiplier(),
                o2_reported_afr: self.exhaust.o2.reported_afr,
                closed_loop_active: self.last_closed_loop_active,
                fuel_trim_afr: self.fuel_trim_afr,
            },
            evap: EvapTelemetry {
                purge_valve_open_fraction: self.evap.purge_valve.effective_open_fraction(self.last_purge_commanded_fraction),
                canister_saturation: self.evap.canister_saturation,
                leak_fraction: self.evap.leak_fraction(),
                afr_adjustment: self.last_evap_afr_adjustment,
                tank_pressure_pa: self.evap.tank_pressure_pa(),
            },
            turbo_count: self.turbo_setup.map_or(0, |t| t.count),
            boost_target_pa: self.turbo_setup.map_or(0.0, |t| t.max_boost_pa),
            turbo_choke_rpm: self.turbo_setup.map_or(0.0, |t| t.choke_rpm),
            charge_temp_k: self.last_charge_temp_k,
            cel_state: self.ecu.cel_state(),
            evap_monitor_status: self.ecu.evap_monitor_status(),
        }
    }
}
