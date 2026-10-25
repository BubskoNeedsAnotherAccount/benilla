//! One cylinder: slider-crank volume trace, polytropic compression, Wiebe combustion.

use crate::config::EngineConfig;
use crate::constants::*;
use crate::ignition::IgnitionChannel;
use crate::rng::Rng;
use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokePhase {
    Intake,
    Compression,
    Power,
    Exhaust,
}

/// Fraction of the charge's chemical energy released after `angle_into_burn_deg` of a burn
/// lasting `duration_deg` - the S-shaped heat-release curve of a real flame front.
pub fn wiebe_mass_fraction_burned(angle_into_burn_deg: f64, duration_deg: f64) -> f64 {
    if angle_into_burn_deg <= 0.0 {
        return 0.0;
    }
    if angle_into_burn_deg >= duration_deg {
        return 1.0;
    }
    let ratio = angle_into_burn_deg / duration_deg;
    1.0 - (-WIEBE_A * ratio.powf(WIEBE_M + 1.0)).exp()
}

#[derive(Clone, Debug)]
pub struct CylinderTelemetry {
    pub label: u32,
    pub phase: StrokePhase,
    pub local_crank_angle_deg: f64,
    pub pressure_pa: f64,
    pub volume_m3: f64,
    pub torque_nm: f64,
    /// 0 = TDC, 1 = BDC.
    pub piston_position_fraction: f64,
    pub spark_fired: bool,
    /// 0..1 how far into the combustion burn this cylinder is (for flame visuals).
    pub burn_fraction: f64,
}

/// Allocation-free snapshot of one cylinder, taken after every micro-step by the renderer.
#[derive(Clone, Copy, Debug)]
pub struct CylinderSample {
    pub label: u32,
    pub local_angle_deg: f64,
    pub pressure_pa: f64,
    pub burn_fraction: f64,
    pub phase: StrokePhase,
    pub ignited: bool,
    pub misfired: bool,
    pub backfired: bool,
}

#[derive(Clone, Debug)]
pub struct Cylinder {
    pub label: u32,
    pub ignition: IgnitionChannel,

    phase_offset_deg: f64,
    piston_area_m2: f64,
    crank_radius_m: f64,
    rod_length_m: f64,
    reciprocating_mass_kg: f64,
    clearance_volume_m3: f64,
    max_volume_m3: f64,
    combustion_start_deg: f64,
    combustion_duration_deg: f64,

    pressure_pa: f64,
    volume_m3: f64,
    phase: StrokePhase,
    prev_local_angle_deg: f64,
    mass_fraction_burned: f64,
    last_torque_nm: f64,
    trapped_air_mass_kg: f64,
    cycle_fuel_mass_kg: f64,
    cycle_heat_energy_j: f64,
    ignited_this_call: bool,
    spark_fired_this_cycle: bool,
    last_genuine_spark_fired: bool,
    misfired_this_call: bool,
    unburned_fuel_mass_kg: f64,
    backfired_this_call: bool,
    fuel_metered_this_call: f64,
}

/// Everything a cylinder needs from the engine for one substep.
pub struct CylinderInputs {
    pub unwrapped_crank_angle_deg: f64,
    pub manifold_pressure_pa: f64,
    pub air_fuel_ratio: f64,
    pub ignition_enabled: bool,
    pub angular_velocity: f64,
    pub atmospheric_pressure_pa: f64,
    pub exhaust_backpressure_multiplier: f64,
    pub intake_air_temp_k: f64,
    pub exhaust_gas_temp_k: f64,
    pub fuel_lhv: f64,
}

impl Cylinder {
    pub fn new(config: &EngineConfig, label: u32, phase_offset_deg: f64) -> Self {
        let piston_area_m2 = PI / 4.0 * config.bore_meters.powi(2);
        let displacement = piston_area_m2 * config.stroke_meters;
        let clearance = displacement / (config.compression_ratio - 1.0);
        Self {
            label,
            ignition: IgnitionChannel::default(),
            phase_offset_deg,
            piston_area_m2,
            crank_radius_m: config.stroke_meters / 2.0,
            rod_length_m: config.rod_length_meters,
            reciprocating_mass_kg: config.reciprocating_mass_kg,
            clearance_volume_m3: clearance,
            max_volume_m3: clearance + displacement,
            combustion_start_deg: 360.0 - config.ignition_advance_deg,
            combustion_duration_deg: config.combustion_duration_deg,
            pressure_pa: DEFAULT_ATMOSPHERIC_PA,
            volume_m3: clearance,
            phase: StrokePhase::Intake,
            prev_local_angle_deg: 0.0,
            mass_fraction_burned: 0.0,
            last_torque_nm: 0.0,
            trapped_air_mass_kg: 0.0,
            cycle_fuel_mass_kg: 0.0,
            cycle_heat_energy_j: 0.0,
            ignited_this_call: false,
            spark_fired_this_cycle: true,
            last_genuine_spark_fired: true,
            misfired_this_call: false,
            unburned_fuel_mass_kg: 0.0,
            backfired_this_call: false,
            fuel_metered_this_call: 0.0,
        }
    }

    fn volume_at(&self, local_angle_deg: f64) -> f64 {
        let theta = local_angle_deg.to_radians();
        let r = self.crank_radius_m;
        let l = self.rod_length_m;
        let s = r * theta.cos() + (l * l - r * r * theta.sin() * theta.sin()).sqrt();
        self.clearance_volume_m3 + self.piston_area_m2 * (r + l - s)
    }

    fn torque_at(&self, local_angle_deg: f64, angular_velocity: f64, atmospheric_pa: f64) -> f64 {
        let theta = local_angle_deg.to_radians();
        let r = self.crank_radius_m;
        let l = self.rod_length_m;
        let (sin_t, cos_t) = theta.sin_cos();
        let cos_2t = (2.0 * theta).cos();
        let rod_ratio = r / l;
        let gas_force = (self.pressure_pa - atmospheric_pa) * self.piston_area_m2;
        let piston_accel = r * angular_velocity * angular_velocity * (cos_t + rod_ratio * cos_2t);
        let inertia_force = -self.reciprocating_mass_kg * piston_accel;
        let denom = (l * l - r * r * sin_t * sin_t).sqrt();
        let dx_dtheta = r * sin_t * (1.0 + (r * cos_t) / denom);
        (gas_force + inertia_force) * dx_dtheta
    }

    fn phase_for(local_angle_deg: f64) -> StrokePhase {
        if local_angle_deg < 180.0 {
            StrokePhase::Intake
        } else if local_angle_deg < 360.0 {
            StrokePhase::Compression
        } else if local_angle_deg < 540.0 {
            StrokePhase::Power
        } else {
            StrokePhase::Exhaust
        }
    }

    /// Advances to the engine's total (never wrapped) crank angle; returns this cylinder's torque.
    pub fn update(&mut self, inp: &CylinderInputs, rng: &mut Rng) -> f64 {
        let local = (inp.unwrapped_crank_angle_deg - self.phase_offset_deg).rem_euclid(720.0);
        let prev_local = self.prev_local_angle_deg;
        self.phase = Self::phase_for(local);
        self.ignited_this_call = false;
        self.misfired_this_call = false;
        self.backfired_this_call = false;
        self.fuel_metered_this_call = 0.0;

        match self.phase {
            StrokePhase::Intake => {
                self.pressure_pa = inp.manifold_pressure_pa;
                self.volume_m3 = self.volume_at(local);
            }
            StrokePhase::Compression | StrokePhase::Power => {
                if prev_local < 180.0 {
                    // Intake valve just closed: lock in this cycle's trapped charge and fuel energy.
                    let ivc_volume = self.volume_at(180.0);
                    self.trapped_air_mass_kg =
                        inp.manifold_pressure_pa * ivc_volume / (GAS_CONSTANT_AIR * inp.intake_air_temp_k);
                    self.cycle_fuel_mass_kg = self.trapped_air_mass_kg / inp.air_fuel_ratio;
                    self.fuel_metered_this_call = self.cycle_fuel_mass_kg;
                    self.cycle_heat_energy_j = self.cycle_fuel_mass_kg
                        * inp.fuel_lhv
                        * COMBUSTION_EFFICIENCY
                        * (1.0 - COMBUSTION_WALL_HEAT_LOSS_FRACTION);
                    self.mass_fraction_burned = 0.0;
                }

                let new_volume = self.volume_at(local);
                let mut pressure = self.pressure_pa * (self.volume_m3 / new_volume).powf(POLYTROPIC_INDEX);

                if local >= self.combustion_start_deg
                    && local <= self.combustion_start_deg + self.combustion_duration_deg
                    && self.cycle_heat_energy_j > 0.0
                {
                    if prev_local < self.combustion_start_deg {
                        let genuine = if inp.ignition_enabled {
                            self.ignition.attempt_spark(self.pressure_pa, rng)
                        } else {
                            true
                        };
                        if inp.ignition_enabled {
                            self.last_genuine_spark_fired = genuine;
                        }
                        self.spark_fired_this_cycle = inp.ignition_enabled && genuine;

                        if self.spark_fired_this_cycle {
                            self.ignited_this_call = true;
                        } else if inp.ignition_enabled {
                            self.misfired_this_call = true;
                            self.unburned_fuel_mass_kg = self.cycle_fuel_mass_kg;
                        }
                    }

                    if inp.ignition_enabled && self.spark_fired_this_cycle {
                        let new_fraction = wiebe_mass_fraction_burned(
                            local - self.combustion_start_deg,
                            self.combustion_duration_deg,
                        );
                        let delta = (new_fraction - self.mass_fraction_burned).max(0.0);
                        let heat_added = self.cycle_heat_energy_j * delta;
                        pressure += (POLYTROPIC_INDEX - 1.0) * heat_added / new_volume;
                        self.mass_fraction_burned = new_fraction;
                    }
                }

                self.pressure_pa = pressure.max(1_000.0);
                self.volume_m3 = new_volume;
            }
            StrokePhase::Exhaust => {
                if prev_local < 540.0 && self.unburned_fuel_mass_kg > 0.0 {
                    let chance = ((inp.exhaust_gas_temp_k - BACKFIRE_THRESHOLD_K) / BACKFIRE_SPREAD_K)
                        .clamp(0.0, 1.0)
                        * BACKFIRE_MAX_PROB;
                    self.backfired_this_call = rng.next_f64() < chance;
                    self.unburned_fuel_mass_kg = 0.0;
                }
                self.pressure_pa = inp.atmospheric_pressure_pa
                    + EXHAUST_BACKPRESSURE_COEFF
                        * inp.exhaust_backpressure_multiplier
                        * inp.angular_velocity
                        * inp.angular_velocity;
                self.volume_m3 = self.volume_at(local);
            }
        }

        self.prev_local_angle_deg = local;
        self.last_torque_nm = self.torque_at(local, inp.angular_velocity, inp.atmospheric_pressure_pa);
        self.last_torque_nm
    }

    pub fn just_ignited(&self) -> bool {
        self.ignited_this_call
    }
    pub fn just_misfired(&self) -> bool {
        self.misfired_this_call
    }
    pub fn just_backfired(&self) -> bool {
        self.backfired_this_call
    }
    pub fn fuel_metered_this_call_kg(&self) -> f64 {
        self.fuel_metered_this_call
    }

    pub fn sample(&self) -> CylinderSample {
        CylinderSample {
            label: self.label,
            local_angle_deg: self.prev_local_angle_deg,
            pressure_pa: self.pressure_pa,
            burn_fraction: if self.spark_fired_this_cycle { self.mass_fraction_burned } else { 0.0 },
            phase: self.phase,
            ignited: self.ignited_this_call,
            misfired: self.misfired_this_call,
            backfired: self.backfired_this_call,
        }
    }

    pub fn telemetry(&self) -> CylinderTelemetry {
        CylinderTelemetry {
            label: self.label,
            phase: self.phase,
            local_crank_angle_deg: self.prev_local_angle_deg,
            pressure_pa: self.pressure_pa,
            volume_m3: self.volume_m3,
            torque_nm: self.last_torque_nm,
            piston_position_fraction: (self.volume_m3 - self.clearance_volume_m3)
                / (self.max_volume_m3 - self.clearance_volume_m3),
            spark_fired: self.last_genuine_spark_fired,
            burn_fraction: if self.spark_fired_this_cycle { self.mass_fraction_burned } else { 0.0 },
        }
    }
}
