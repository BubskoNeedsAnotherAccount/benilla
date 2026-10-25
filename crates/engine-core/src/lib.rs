//! Headless piston-engine + drivetrain simulation. No graphics or audio dependencies.

pub mod acoustics;
pub mod config;
pub mod constants;
pub mod cylinder;
pub mod ecu;
pub mod engine;
pub mod evap;
pub mod exhaust;
pub mod ignition;
pub mod rng;
pub mod sound;
pub mod tuning;

pub use config::{
    bank_tilts_deg, brakes, engine_by_id, engines, fuels, layout_of, transmissions, BrakeConfig, EngineConfig, FuelConfig,
    IgnitionType, TransmissionConfig, TurboConfig,
};
pub use acoustics::PipeMaterial;
pub use cylinder::{CylinderSample, CylinderTelemetry, StrokePhase};
pub use ecu::{CelState, EvapMonitorStatus};
pub use engine::{CombustionEngine, EngineEvents, EngineTelemetry};
pub use tuning::{
    stock_turbo_count, AirFilter, BlowOff, Cat, Crossover, Header, Intercooler, Muffler, Runners, Tuning, TurboSetup, BOOST_MAX_BAR, BOOST_MIN_BAR,
};

/// Convenience: a ready-to-run engine with the default 6-speed, standard brakes and a fuel
/// matching the engine's ignition type.
pub fn build_default(config: EngineConfig) -> CombustionEngine {
    let fuel = fuels()
        .into_iter()
        .find(|f| f.ignition_type == config.ignition_type)
        .expect("a fuel exists for every ignition type");
    let tx = transmissions().into_iter().find(|t| t.id == "6speed").unwrap();
    let brake = brakes().into_iter().next().unwrap();
    CombustionEngine::new(config, &tx, &brake, fuel)
}

/// One steady-state wide-open-throttle point on a dyno sweep.
#[derive(Clone, Copy, Debug)]
pub struct DynoPoint {
    pub rpm: f64,
    pub torque_nm: f64,
    pub hp: f64,
    pub boost_pa: f64,
}

/// Runs a full wide-open-throttle sweep (idle to redline) on a fresh warm engine with `tuning`
/// bolted on. Headless and quick (tens of milliseconds in release), so a UI can re-run it on
/// every parts swap and overlay it on the stock curve.
pub fn dyno_curve(config: &EngineConfig, fuel: &FuelConfig, tuning: &Tuning, step_rpm: f64) -> Vec<DynoPoint> {
    let tx = transmissions().into_iter().find(|t| t.id == "6speed").unwrap();
    let brake = brakes().into_iter().next().unwrap();
    let mut e = CombustionEngine::new(config.clone(), &tx, &brake, fuel.clone());
    e.set_tuning(tuning.clone());
    e.warm_up();
    let mut out = Vec::new();
    let mut rpm = (config.redline_rpm * 0.2).max(1000.0);
    while rpm <= config.redline_rpm + 1.0 {
        let tq = e.dyno_point(rpm, 0.3);
        let hp = tq * rpm * std::f64::consts::PI / 30.0 / 745.7;
        out.push(DynoPoint { rpm, torque_nm: tq, hp, boost_pa: e.telemetry().boost_pressure_pa });
        rpm += step_rpm;
    }
    out
}
