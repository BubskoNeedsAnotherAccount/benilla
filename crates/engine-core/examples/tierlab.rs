//! How much the solver tiers differ in what they radiate from the tailpipe, band by band.
//!
//! ```text
//! cargo run --release -p engine-core --example tierlab -- --engines v8,i4,i6 --rpms 1200,3000,4500,6000
//! ```
//!
//! For every engine and speed the monopole radiation of the tailpipes (`p_tail`) is rendered at each tier of the quality
//! ladder and compared with the finest tier in 1/3-octave bands; the table at the end is the mean and the spread of
//! that difference over all of them: what a tier change does to the sound, and what the per-tier correction
//! (`sound::tier_eq`) has to undo. `--corrected` renders with the correction on and shows what is left.

use engine_core::acoustics::analysis::*;
use engine_core::sound::{default_banks, EngineSound, Quality, SoundInput, SAMPLE_RATE};
use engine_core::*;
use std::collections::HashMap;

fn args() -> HashMap<String, String> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut m = HashMap::new();
    let mut i = 0;
    while i < a.len() {
        if let Some(k) = a[i].strip_prefix("--") {
            let v = a.get(i + 1).filter(|v| !v.starts_with("--")).cloned();
            i += if v.is_some() { 2 } else { 1 };
            m.insert(k.to_string(), v.unwrap_or_else(|| "1".into()));
        } else {
            i += 1;
        }
    }
    m
}

fn operating_point(cfg: &EngineConfig, rpm: f64, throttle: f64) -> SoundInput {
    let fuel = fuels()
        .into_iter()
        .find(|f| f.ignition_type == cfg.ignition_type)
        .unwrap();
    let mut e = CombustionEngine::new(cfg.clone(), &transmissions()[1], &brakes()[0], fuel);
    e.warm_up();
    let tel = e.steady_point(rpm, throttle);
    let tq_ref = (75.0 * cfg.displacement_l()).max(60.0);
    SoundInput {
        rpm: rpm as f32,
        throttle: throttle as f32,
        load: (tel.display_torque_nm / tq_ref).clamp(0.0, 1.3) as f32,
        map_pa: tel.manifold_pressure_pa as f32,
        atm_pa: tel.atmospheric_pressure_pa as f32,
        air_k: tel.air_temperature_k as f32,
        charge_k: tel.charge_temp_k as f32,
        afr: tel.air_fuel_ratio as f32,
        egt_k: tel.exhaust_gas_temp_k as f32,
        boost: (tel.boost_pressure_pa / 1e5) as f32,
        spool: tel.turbo_spool_fraction as f32,
        running: true,
        lit: true,
        burn: vec![1.0; cfg.cylinder_count],
        master: 1.0,
        serial: 1,
        turbo_speed: tel.turbo_spool_fraction as f32,
        vehicle_speed: (rpm / 3000.0 * 18.0) as f32,
        ..Default::default()
    }
}

/// Centres of the 1/3-octave bands compared, Hz.
fn centres() -> Vec<f64> {
    (0..20)
        .map(|i| 50.0 * 2f64.powf(i as f64 / 3.0))
        .filter(|f| *f < 3200.0)
        .collect()
}

fn bands_db(x: &[f32]) -> Vec<f64> {
    let psd = spectrum(x, 8192);
    centres()
        .iter()
        .map(|&c| {
            10.0 * band_power(
                &psd,
                SAMPLE_RATE as f64,
                c / 2f64.powf(1.0 / 6.0),
                c * 2f64.powf(1.0 / 6.0),
            )
            .max(1e-30)
            .log10()
        })
        .collect()
}

fn render(
    cfg: &EngineConfig,
    q: Quality,
    inp: &SoundInput,
    secs: f32,
    corrected: bool,
) -> Vec<f32> {
    let mut snd = EngineSound::new(cfg, &Tuning::default(), &default_banks(cfg), q);
    snd.tier_correction = corrected;
    snd.preheat(inp.egt_k * 0.9);
    snd.set_input(inp);
    snd.soak(1.2, 60.0);
    let mut sink = vec![0.0f32; 22_050];
    snd.render(&mut sink);
    (0..(secs * SAMPLE_RATE as f32) as usize)
        .map(|_| {
            snd.next_sample();
            snd.p_tail
        })
        .collect()
}

fn main() {
    let m = args();
    let engines = m
        .get("engines")
        .cloned()
        .unwrap_or_else(|| "v8,i4,i6,v6,v12,w24,dieseli6".into());
    let rpms: Vec<f64> = m
        .get("rpms")
        .cloned()
        .unwrap_or_else(|| "1200,3000,4500,6000".into())
        .split(',')
        .filter_map(|v| v.parse().ok())
        .collect();
    let thr: f64 = m
        .get("throttle")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.7);
    let secs: f32 = m.get("secs").and_then(|v| v.parse().ok()).unwrap_or(1.5);
    let corrected = m.contains_key("corrected");
    let cs = centres();
    // diffs[tier][band] over cases
    let mut diffs: Vec<Vec<Vec<f64>>> = vec![vec![Vec::new(); cs.len()]; 5];
    for id in engines.split(',') {
        let Some(cfg) = engine_by_id(id) else {
            continue;
        };
        for &rpm in &rpms {
            let inp = operating_point(&cfg, rpm, thr);
            let reference = bands_db(&render(&cfg, Quality::ULTRA, &inp, secs, false));
            for t in 1..5 {
                let b = bands_db(&render(&cfg, Quality::LADDER[t], &inp, secs, corrected));
                let row: Vec<String> = b
                    .iter()
                    .zip(&reference)
                    .map(|(a, r)| format!("{:+.0}", a - r))
                    .collect();
                println!(
                    "{id:>9} {rpm:5.0} {:>8}: {}",
                    Quality::NAMES[t],
                    row.join(" ")
                );
                for (k, (a, r)) in b.iter().zip(&reference).enumerate() {
                    diffs[t][k].push(a - r);
                }
            }
        }
    }
    println!(
        "\nbands (Hz): {}",
        cs.iter()
            .map(|c| format!("{c:.0}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    for t in 1..5 {
        let mean: Vec<String> = diffs[t]
            .iter()
            .map(|v| format!("{:+.1}", v.iter().sum::<f64>() / v.len().max(1) as f64))
            .collect();
        let sd: Vec<String> = diffs[t]
            .iter()
            .map(|v| {
                let mu = v.iter().sum::<f64>() / v.len().max(1) as f64;
                format!(
                    "{:.1}",
                    (v.iter().map(|x| (x - mu).powi(2)).sum::<f64>() / v.len().max(1) as f64)
                        .sqrt()
                )
            })
            .collect();
        println!("{:>8} mean {}", Quality::NAMES[t], mean.join(" "));
        println!("{:>8} sd   {}", "", sd.join(" "));
    }
}
