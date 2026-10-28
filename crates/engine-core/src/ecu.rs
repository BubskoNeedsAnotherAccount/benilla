//! A basic ECU: only the OBD-style monitors that would realistically light a check-engine lamp,
//! each reading only a signal a real ECU has (crank speed, O2 reading, its own fuel trim, the
//! fuel-tank pressure sensor) - never the sim's ground-truth fault state.

use crate::constants::*;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CelState {
    Off,
    Steady,
    Flashing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvapMonitorStatus {
    NotReady,
    Testing,
    Pass,
    SmallLeak,
    LargeLeak,
}

#[derive(Clone, Copy, Debug)]
pub struct EvapMonitorCommand {
    pub vent_closed_commanded: bool,
    pub purge_commanded_fraction: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvapPhase {
    Idle,
    DrawDown,
    Hold,
    Cooldown,
}

#[derive(Clone, Debug)]
pub struct Ecu {
    segment_angle_deg: f64,

    // misfire monitor
    accumulated_angle_deg: f64,
    accumulated_weighted_speed: f64,
    accumulated_time_s: f64,
    recent_healthy_speeds: VecDeque<f64>,
    consecutive_irregular: usize,
    elapsed_monitored_time_s: f64,
    time_since_sampling_resumed_s: f64,
    irregular_event_times_s: VecDeque<f64>,
    last_severe_event_time_s: f64,
    last_irregular_event_time_s: f64,
    misfire_stored_fault: bool,

    // O2 monitor
    elapsed_sensor_time_s: f64,
    o2_disconnected_duration_s: f64,
    o2_window_elapsed_s: f64,
    o2_window_closed_loop_s: f64,
    o2_window_min_afr: f64,
    o2_window_max_afr: f64,
    o2_inactive_accumulated_s: f64,
    last_o2_fault_event_time_s: f64,
    o2_fault: bool,

    // fuel trim monitor
    fuel_trim_pegged_duration_s: f64,
    last_fuel_trim_fault_event_time_s: f64,
    fuel_trim_fault: bool,

    // EVAP monitor
    evap_phase: EvapPhase,
    evap_ready_duration_s: f64,
    evap_phase_elapsed_s: f64,
    evap_cooldown_remaining_s: f64,
    evap_hold_start_pressure_pa: f64,
    evap_last_result: EvapMonitorStatus,
    evap_fault: bool,
}

impl Ecu {
    pub fn new(cylinder_count: usize) -> Self {
        Self {
            segment_angle_deg: 720.0 / cylinder_count as f64,
            accumulated_angle_deg: 0.0,
            accumulated_weighted_speed: 0.0,
            accumulated_time_s: 0.0,
            recent_healthy_speeds: VecDeque::new(),
            consecutive_irregular: 0,
            elapsed_monitored_time_s: 0.0,
            time_since_sampling_resumed_s: 0.0,
            irregular_event_times_s: VecDeque::new(),
            last_severe_event_time_s: f64::NEG_INFINITY,
            last_irregular_event_time_s: f64::NEG_INFINITY,
            misfire_stored_fault: false,
            elapsed_sensor_time_s: 0.0,
            o2_disconnected_duration_s: 0.0,
            o2_window_elapsed_s: 0.0,
            o2_window_closed_loop_s: 0.0,
            o2_window_min_afr: f64::INFINITY,
            o2_window_max_afr: f64::NEG_INFINITY,
            o2_inactive_accumulated_s: 0.0,
            last_o2_fault_event_time_s: f64::NEG_INFINITY,
            o2_fault: false,
            fuel_trim_pegged_duration_s: 0.0,
            last_fuel_trim_fault_event_time_s: f64::NEG_INFINITY,
            fuel_trim_fault: false,
            evap_phase: EvapPhase::Idle,
            evap_ready_duration_s: 0.0,
            evap_phase_elapsed_s: 0.0,
            evap_cooldown_remaining_s: 0.0,
            evap_hold_start_pressure_pa: 0.0,
            evap_last_result: EvapMonitorStatus::NotReady,
            evap_fault: false,
        }
    }

    /// Feed one substep of crank motion to the misfire monitor.
    pub fn update(&mut self, sampling: bool, angular_velocity: f64, dt: f64) {
        if !sampling {
            self.accumulated_angle_deg = 0.0;
            self.accumulated_weighted_speed = 0.0;
            self.accumulated_time_s = 0.0;
            self.time_since_sampling_resumed_s = 0.0;
            // The operating point can change completely across a gap (a gear shift, a limiter cut,
            // a clutch dump), so the old baseline is meaningless afterwards: rebuild it.
            self.recent_healthy_speeds.clear();
            self.consecutive_irregular = 0;
            return;
        }
        self.elapsed_monitored_time_s += dt;
        self.time_since_sampling_resumed_s += dt;
        self.accumulated_angle_deg += angular_velocity.to_degrees() * dt;
        self.accumulated_weighted_speed += angular_velocity * dt;
        self.accumulated_time_s += dt;

        while self.accumulated_angle_deg >= self.segment_angle_deg && self.accumulated_time_s > 0.0
        {
            if self.time_since_sampling_resumed_s >= ECU_MISFIRE_MONITOR_ARM_DELAY_S {
                self.close_segment(self.accumulated_weighted_speed / self.accumulated_time_s);
            }
            self.accumulated_angle_deg -= self.segment_angle_deg;
            self.accumulated_weighted_speed = 0.0;
            self.accumulated_time_s = 0.0;
        }

        while let Some(&front) = self.irregular_event_times_s.front() {
            if self.elapsed_monitored_time_s - front > ECU_MISFIRE_RATE_WINDOW_S {
                self.irregular_event_times_s.pop_front();
            } else {
                break;
            }
        }

        if self.misfire_stored_fault
            && self.irregular_event_times_s.is_empty()
            && self.elapsed_monitored_time_s - self.last_irregular_event_time_s
                > ECU_FAULT_CLEAR_CLEAN_RUN_S
        {
            self.misfire_stored_fault = false;
        }
    }

    fn close_segment(&mut self, avg_speed: f64) {
        let mut irregular = false;
        if self.recent_healthy_speeds.len() >= ECU_BASELINE_MIN_SAMPLES {
            let baseline = self.recent_healthy_speeds.iter().sum::<f64>()
                / self.recent_healthy_speeds.len() as f64;
            let drop = (baseline - avg_speed) / baseline;
            if drop > ECU_MISFIRE_SPEED_DROP_THRESHOLD {
                irregular = true;
                self.irregular_event_times_s
                    .push_back(self.elapsed_monitored_time_s);
                self.last_irregular_event_time_s = self.elapsed_monitored_time_s;
                if self.irregular_event_times_s.len() >= ECU_MISFIRE_STORE_COUNT {
                    self.misfire_stored_fault = true;
                }
                if drop > ECU_MISFIRE_SEVERE_SPEED_DROP_THRESHOLD {
                    self.last_severe_event_time_s = self.elapsed_monitored_time_s;
                }
            }
        }
        if irregular {
            // A misfire is an intermittent dip. A long unbroken run of "dips" is a new operating
            // point (hard load change, dyno step...), not misfires - re-learn the baseline.
            self.consecutive_irregular += 1;
            if self.consecutive_irregular >= ECU_BASELINE_WINDOW_SEGMENTS {
                self.recent_healthy_speeds.clear();
                self.consecutive_irregular = 0;
            }
        } else {
            self.consecutive_irregular = 0;
        }
        if !irregular {
            self.recent_healthy_speeds.push_back(avg_speed);
            if self.recent_healthy_speeds.len() > ECU_BASELINE_WINDOW_SEGMENTS {
                self.recent_healthy_speeds.pop_front();
            }
        }
    }

    /// O2 circuit/activity and fuel trim monitors. Called once per engine update (spark only).
    pub fn update_sensors(
        &mut self,
        dt: f64,
        closed_loop: bool,
        o2_connected: bool,
        o2_reported_afr: f64,
        fuel_trim_afr: f64,
    ) {
        self.elapsed_sensor_time_s += dt;

        if !o2_connected {
            self.o2_disconnected_duration_s += dt;
            if self.o2_disconnected_duration_s >= ECU_O2_CIRCUIT_FAULT_CONFIRM_S {
                self.o2_fault = true;
                self.last_o2_fault_event_time_s = self.elapsed_sensor_time_s;
            }
        } else {
            self.o2_disconnected_duration_s = 0.0;
        }

        if o2_connected {
            self.o2_window_elapsed_s += dt;
            if closed_loop {
                self.o2_window_closed_loop_s += dt;
                self.o2_window_min_afr = self.o2_window_min_afr.min(o2_reported_afr);
                self.o2_window_max_afr = self.o2_window_max_afr.max(o2_reported_afr);
            }
            if self.o2_window_elapsed_s >= ECU_O2_ACTIVITY_WINDOW_S {
                if self.o2_window_closed_loop_s >= ECU_O2_ACTIVITY_MIN_COVERAGE_S {
                    let range = self.o2_window_max_afr - self.o2_window_min_afr;
                    if range < ECU_O2_MIN_SWITCHING_RANGE_AFR {
                        self.o2_inactive_accumulated_s += self.o2_window_elapsed_s;
                        if self.o2_inactive_accumulated_s >= ECU_O2_INACTIVITY_FAULT_CONFIRM_S {
                            self.o2_fault = true;
                            self.last_o2_fault_event_time_s = self.elapsed_sensor_time_s;
                        }
                    } else {
                        self.o2_inactive_accumulated_s = 0.0;
                    }
                }
                self.o2_window_elapsed_s = 0.0;
                self.o2_window_closed_loop_s = 0.0;
                self.o2_window_min_afr = f64::INFINITY;
                self.o2_window_max_afr = f64::NEG_INFINITY;
            }
        } else {
            self.o2_window_elapsed_s = 0.0;
            self.o2_window_closed_loop_s = 0.0;
            self.o2_window_min_afr = f64::INFINITY;
            self.o2_window_max_afr = f64::NEG_INFINITY;
            self.o2_inactive_accumulated_s = 0.0;
        }

        if closed_loop
            && fuel_trim_afr.abs() >= ECU_FUEL_TRIM_FAULT_FRACTION * FUEL_TRIM_MAX_ABS_AFR
        {
            self.fuel_trim_pegged_duration_s += dt;
            if self.fuel_trim_pegged_duration_s >= ECU_FUEL_TRIM_FAULT_CONFIRM_S {
                self.fuel_trim_fault = true;
                self.last_fuel_trim_fault_event_time_s = self.elapsed_sensor_time_s;
            }
        } else {
            self.fuel_trim_pegged_duration_s = 0.0;
        }

        if self.o2_fault
            && self.elapsed_sensor_time_s - self.last_o2_fault_event_time_s
                > ECU_FAULT_CLEAR_CLEAN_RUN_S
        {
            self.o2_fault = false;
        }
        if self.fuel_trim_fault
            && self.elapsed_sensor_time_s - self.last_fuel_trim_fault_event_time_s
                > ECU_FAULT_CLEAR_CLEAN_RUN_S
        {
            self.fuel_trim_fault = false;
        }
    }

    /// Runs/advances the EVAP leak-check test; returns what the ECU wants the valves to do.
    pub fn update_evap_monitor(
        &mut self,
        dt: f64,
        ready: bool,
        tank_pressure_pa: f64,
        normal_purge: f64,
    ) -> EvapMonitorCommand {
        let pass = EvapMonitorCommand {
            vent_closed_commanded: false,
            purge_commanded_fraction: normal_purge,
        };

        match self.evap_phase {
            EvapPhase::Cooldown => {
                self.evap_cooldown_remaining_s -= dt;
                if self.evap_cooldown_remaining_s <= 0.0 {
                    self.evap_phase = EvapPhase::Idle;
                }
                return pass;
            }
            EvapPhase::Idle => {
                if !ready {
                    self.evap_ready_duration_s = 0.0;
                    return pass;
                }
                self.evap_ready_duration_s += dt;
                if self.evap_ready_duration_s >= ECU_EVAP_MONITOR_MIN_READY_DURATION_S {
                    self.evap_phase = EvapPhase::DrawDown;
                    self.evap_phase_elapsed_s = 0.0;
                }
                return pass;
            }
            _ => {}
        }

        if !ready {
            self.evap_phase = EvapPhase::Idle;
            self.evap_ready_duration_s = 0.0;
            self.evap_last_result = EvapMonitorStatus::NotReady;
            return pass;
        }

        self.evap_phase_elapsed_s += dt;
        self.evap_last_result = EvapMonitorStatus::Testing;

        if self.evap_phase == EvapPhase::DrawDown {
            if tank_pressure_pa <= ECU_EVAP_TEST_TARGET_VACUUM_PA {
                self.evap_phase = EvapPhase::Hold;
                self.evap_phase_elapsed_s = 0.0;
                self.evap_hold_start_pressure_pa = tank_pressure_pa;
                return EvapMonitorCommand {
                    vent_closed_commanded: true,
                    purge_commanded_fraction: 0.0,
                };
            }
            if self.evap_phase_elapsed_s >= ECU_EVAP_TEST_DRAW_DOWN_MAX_S {
                self.conclude_evap_test(EvapMonitorStatus::LargeLeak);
                return pass;
            }
            return EvapMonitorCommand {
                vent_closed_commanded: true,
                purge_commanded_fraction: 1.0,
            };
        }

        // hold
        if self.evap_phase_elapsed_s >= ECU_EVAP_TEST_HOLD_DURATION_S {
            let decay = tank_pressure_pa - self.evap_hold_start_pressure_pa;
            if decay <= ECU_EVAP_TEST_PASS_MAX_DECAY_PA {
                self.conclude_evap_test(EvapMonitorStatus::Pass);
            } else if decay <= ECU_EVAP_TEST_SMALL_LEAK_MAX_DECAY_PA {
                self.conclude_evap_test(EvapMonitorStatus::SmallLeak);
            } else {
                self.conclude_evap_test(EvapMonitorStatus::LargeLeak);
            }
            return pass;
        }
        EvapMonitorCommand {
            vent_closed_commanded: true,
            purge_commanded_fraction: 0.0,
        }
    }

    fn conclude_evap_test(&mut self, result: EvapMonitorStatus) {
        self.evap_last_result = result;
        self.evap_phase = EvapPhase::Cooldown;
        self.evap_cooldown_remaining_s = ECU_EVAP_MONITOR_RETEST_COOLDOWN_S;
        self.evap_fault = matches!(
            result,
            EvapMonitorStatus::SmallLeak | EvapMonitorStatus::LargeLeak
        );
    }

    pub fn evap_monitor_status(&self) -> EvapMonitorStatus {
        self.evap_last_result
    }

    pub fn on_engine_stopped(&mut self) {
        self.last_severe_event_time_s = f64::NEG_INFINITY;
    }

    /// The "clear codes" scan-tool stand-in; underlying hardware faults come right back.
    pub fn clear_stored_fault(&mut self) {
        self.misfire_stored_fault = false;
        self.o2_fault = false;
        self.fuel_trim_fault = false;
        self.evap_fault = false;
        self.irregular_event_times_s.clear();
        self.o2_inactive_accumulated_s = 0.0;
        self.fuel_trim_pegged_duration_s = 0.0;
        self.evap_phase = EvapPhase::Idle;
        self.evap_ready_duration_s = 0.0;
        self.evap_last_result = EvapMonitorStatus::NotReady;
    }

    /// Which monitors currently hold a fault: (misfire, o2, fuel trim, evap).
    pub fn fault_flags(&self) -> (bool, bool, bool, bool) {
        (
            self.misfire_stored_fault,
            self.o2_fault,
            self.fuel_trim_fault,
            self.evap_fault,
        )
    }

    pub fn cel_state(&self) -> CelState {
        if self.elapsed_monitored_time_s - self.last_severe_event_time_s
            < ECU_MISFIRE_ACTIVE_FLASH_WINDOW_S
        {
            return CelState::Flashing;
        }
        if self.misfire_stored_fault || self.o2_fault || self.fuel_trim_fault || self.evap_fault {
            return CelState::Steady;
        }
        CelState::Off
    }
}
