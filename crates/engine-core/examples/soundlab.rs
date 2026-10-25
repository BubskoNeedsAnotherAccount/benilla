//! Sound laboratory: render an engine through the acoustic model without the viewer, then look at what
//! came out - spectra, resonance peaks against the firing order and the pipe lengths, levels - and write
//! WAV files and spectrograms.
//!
//! ```text
//! cargo run --release -p engine-core --example soundlab -- steady --engine v8 --rpm 3000 --throttle 0.8
//! cargo run --release -p engine-core --example soundlab -- sweep  --engine v8 --from 900 --to 6400 --secs 12
//! cargo run --release -p engine-core --example soundlab -- resonances --engine v8 --rpm 4000
//! cargo run --release -p engine-core --example soundlab -- compare --engine v8 --rpm 3000 --ref recording.wav
//! ```
//!
//! `resonances` lists the acoustic resonances of the exhaust (frequency-domain transfer-matrix solution, an
//! independent check of the time-domain solver) and the engine speeds at which a firing harmonic meets each.
//! `compare` renders the model at a steady operating point and sets its spectrum against a recording of the
//! same engine at the same speed (1/3-octave bands, peaks, spectral centroid; levels are compared after
//! normalising both to equal total power, since the gain of a recording is unknown).

use engine_core::acoustics::analysis::*;
use engine_core::acoustics::linear::Linear;
use engine_core::acoustics::Gas;
use engine_core::sound::{default_banks, EngineSound, Quality, SoundInput, SAMPLE_RATE};
use engine_core::*;
use std::collections::HashMap;

fn args() -> (String, HashMap<String, String>) {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let cmd = a.first().cloned().unwrap_or_else(|| "steady".into());
    let mut m = HashMap::new();
    let mut i = 1;
    while i < a.len() {
        if let Some(k) = a[i].strip_prefix("--") {
            let v = a.get(i + 1).filter(|v| !v.starts_with("--")).cloned().unwrap_or_else(|| "1".into());
            i += if v == "1" && a.get(i + 1).map_or(true, |x| x.starts_with("--")) { 1 } else { 2 };
            m.insert(k.to_string(), v);
        } else {
            i += 1;
        }
    }
    (cmd, m)
}

fn num(m: &HashMap<String, String>, k: &str, d: f32) -> f32 {
    m.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
}

/// The sound model's input for a steady operating point of the real engine model.
fn operating_point(cfg: &EngineConfig, tuning: &Tuning, rpm: f64, throttle: f64) -> SoundInput {
    let fuel = fuels().into_iter().find(|f| f.ignition_type == cfg.ignition_type).unwrap();
    let mut e = CombustionEngine::new(cfg.clone(), &transmissions()[1], &brakes()[0], fuel);
    e.set_tuning(tuning.clone());
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
        cranking: false,
        knock: 0.0,
        vehicle_speed: num_speed(rpm),
        gear_whine: 0.0,
        burn: vec![1.0; cfg.cylinder_count],
        master: 1.0,
        serial: 1,
        turbo_speed: tel.turbo_spool_fraction as f32,
        burble: 0.6,
        backfire: 0.0,
    }
}

/// Road speed (m/s) a car would be doing at this rpm in a middle gear; it sets the underbody airflow.
fn num_speed(rpm: f64) -> f32 {
    (rpm / 3000.0 * 18.0) as f32
}

fn spl(p_rms: f64) -> f64 {
    assert!(p_rms.is_finite(), "non-finite pressure");
    20.0 * (p_rms.max(1e-9) / 20e-6).log10()
}

/// Octave-ish band levels (dB SPL) of a pressure signal, for finding which source carries which band.
fn band_levels(x: &[f32], fs: f64) -> String {
    let psd = spectrum(x, 4096);
    let nfft = (psd.len() - 1) * 2;
    let df = fs / nfft as f64;
    let edges = [20.0, 100.0, 300.0, 1000.0, 3000.0, 6000.0, 10000.0, 22050.0];
    let mut parts = Vec::new();
    for w in edges.windows(2) {
        let ms: f64 = 2.0 / nfft as f64 * (1..psd.len()).filter(|k| (*k as f64 * df) >= w[0] && (*k as f64 * df) < w[1]).map(|k| psd[k]).sum::<f64>();
        parts.push(format!("{:.0}-{:.0}:{:5.1}", w[0], w[1], 10.0 * (ms.max(1e-30) / (20e-6f64).powi(2)).log10()));
    }
    parts.join(" ")
}

fn main() {
    let (cmd, m) = args();
    let id = m.get("engine").cloned().unwrap_or_else(|| "v8".into());
    let cfg = engine_by_id(&id).expect("unknown engine id");
    let tuning = Tuning::parse_spec(m.get("tune").map(String::as_str).unwrap_or("")).expect("bad --tune");
    let out_dir = m.get("out").cloned().unwrap_or_else(|| "target/soundlab".into());
    std::fs::create_dir_all(&out_dir).ok();
    let mut quality = match m.get("quality").map(String::as_str) {
        Some("ultra") => Quality::ULTRA,
        Some("high") => Quality::HIGH,
        Some("fast") => Quality::FAST,
        Some("eco") => Quality::ECO,
        _ => Quality::BALANCED,
    };
    quality.tol = num(&m, "tol", quality.tol);
    quality.dx = num(&m, "dx", quality.dx);
    quality.div = num(&m, "div", quality.div as f32) as u32;
    let mut banks = default_banks(&cfg);
    if let Some(b) = m.get("banks") {
        // --banks 0,0,1,1: bank of every cylinder (the viewer's own layout is the default)
        banks = b.split(',').map(|v| v.parse().expect("bad --banks")).collect();
    }
    let t_build = std::time::Instant::now();
    let mut snd = EngineSound::new(&cfg, &tuning, &banks, quality);
    println!("built in {:.0} ms", t_build.elapsed().as_secs_f64() * 1e3);
    let r = snd.gas.exhaust.report.clone();
    println!(
        "{} - {} cells, {} nodes, {} tail(s); primary {:.2} m x {:.0} mm ({}), main pipe {:.0} mm ({}), path {:.2} m",
        cfg.display_name, r.cells, r.nodes, r.tails, r.primary_length, r.primary_diameter * 1e3, r.header_material, r.main_diameter * 1e3, r.pipe_material, r.path_length
    );
    let fs = SAMPLE_RATE as f64;

    match cmd.as_str() {
        "steady" => {
            let rpm = num(&m, "rpm", 3000.0);
            let thr = num(&m, "throttle", 0.8);
            let secs = num(&m, "secs", 2.0);
            let mut inp = operating_point(&cfg, &tuning, rpm as f64, thr as f64);
            if m.contains_key("crank") {
                inp.lit = false;
                inp.cranking = true;
                inp.burn = vec![0.0; cfg.cylinder_count];
                inp.map_pa = inp.atm_pa * 0.55;
                inp.load = 0.0;
            }
            println!("operating point: {rpm:.0} rpm, throttle {thr:.2}: MAP {:.0} kPa, AFR {:.1}, EGT {:.0} K, load {:.2}", inp.map_pa / 1e3, inp.afr, inp.egt_k, inp.load);
            snd.preheat(inp.egt_k * 0.9);
            snd.set_input(&inp);
            println!("rattle level {:.2} (petrol idle = 0.72), input spool {:.2} boost {:.2}", snd.rattle_level(), inp.spool, inp.boost);

            snd.soak(num(&m, "soak", 2.0), 60.0);
            println!("turbo shaft {:.2}", snd.turbo_shaft());
            let warm = (num(&m, "warm", 1.0) * fs as f32) as usize;
            let mut sink = vec![0.0f32; warm];
            snd.render(&mut sink);
            let n = (secs as f64 * fs) as usize;
            let (mut full, mut tail) = (Vec::with_capacity(n), Vec::with_capacity(n));
            let mut parts = [0.0f64; 5];
            let mut srcs: [Vec<f32>; 7] = Default::default();
            let mut full_pa: Vec<f32> = Vec::with_capacity(n);
            let mut body_sq = vec![0.0f64; snd.shell_out.len()];
            let t0 = std::time::Instant::now();
            for _ in 0..n {
                full.push(snd.next_sample());
                tail.push(snd.tail_pressure());
                full_pa.push(snd.p_tail + snd.p_jet + snd.p_shell + snd.p_mech + snd.p_intake);
                srcs[0].push(snd.p_tail);
                srcs[1].push(snd.p_jet);
                srcs[2].push(snd.p_shell);
                srcs[3].push(snd.p_mech);
                srcs[4].push(snd.p_intake);
                srcs[5].push(*full.last().unwrap());
                srcs[6].push(snd.gas.crank);
                parts[0] += (snd.p_tail as f64).powi(2);
                parts[1] += (snd.p_jet as f64).powi(2);
                parts[2] += (snd.p_shell as f64).powi(2);
                parts[3] += (snd.p_mech as f64).powi(2);
                parts[4] += (snd.p_intake as f64).powi(2);
                for (b, o) in snd.shell_out.iter().enumerate() {
                    body_sq[b] += (*o as f64).powi(2);
                }
            }
            let el = t0.elapsed().as_secs_f64();
            println!("rendered {secs:.1} s in {el:.2} s ({:.1}x real time, {:.2} solver sub-steps per step, {} Newton fallbacks)", secs as f64 / el, snd.gas.sub_total as f64 / snd.gas.calls.max(1) as f64, snd.gas.exhaust.net.fallbacks());
            {
                let n = snd.sample_index as f64;
                let g = &snd.gas;
                let np = g.exhaust.net.prof;
                println!("network cycles/sample: primitives {:.0}, valve nodes {:.0}, other nodes {:.0}, advance {:.0}", np[0] as f64 / n, np[1] as f64 / n, np[2] as f64 / n, np[3] as f64 / n);
                {
                    let mut by: Vec<(String, f64, f64)> = Vec::new();
                    for nd in &g.exhaust.net.nodes {
                        if nd.solves > 0 {
                            by.push((nd.name.clone(), nd.iters as f64 / nd.solves as f64, nd.legs.len() as f64 + nd.slugs.len() as f64));
                        }
                    }
                    by.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                    let top: Vec<String> = by.iter().take(8).map(|x| format!("{} {:.2} it/{} legs", x.0, x.1, x.2)).collect();
                    println!("busiest junctions: {}; mean iterations {:.2}", top.join(", "), g.exhaust.net.mean_iterations());
                }
                {
                    // where the node time goes: by number of legs and valve presence
                    let mut groups: std::collections::BTreeMap<String, (usize, u64, u64, u64, u32)> = Default::default();
                    for nd in &g.exhaust.net.nodes {
                        let kind = match nd.terminal {
                            engine_core::acoustics::node::Terminal::Junction => format!("junction {} legs {} valves {} necks", nd.legs.len(), nd.valves.len(), nd.slugs.len()),
                            engine_core::acoustics::node::Terminal::Wall => "wall".to_string(),
                            engine_core::acoustics::node::Terminal::Open(_) => "open end".to_string(),
                            engine_core::acoustics::node::Terminal::Tank(_) => "tank".to_string(),
                        };
                        let e = groups.entry(kind).or_default();
                        e.0 += 1;
                        e.1 += nd.cycles;
                        e.2 += nd.solves;
                        e.3 += nd.iters;
                        e.4 += nd.fallbacks;
                    }
                    for (k, (cnt, cyc, solves, iters, fb)) in &groups {
                        println!("  {k:<34} x{cnt:<3} {:>6.0} cycles/solve, {:.2} it/solve, {:.4}% fallbacks, {:.0} cycles/sample", *cyc as f64 / (*solves).max(1) as f64, *iters as f64 / (*solves).max(1) as f64, 100.0 * *fb as f64 / (*solves).max(1) as f64, *cyc as f64 / n);
                    }
                }
                if m.contains_key("cells") {
                    let mut d: Vec<_> = g.exhaust.net.grid.ducts.iter().enumerate().map(|(i, d)| (d.dx, d.n, d.length, d.name.clone(), i)).collect();
                    d.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                    for (dx, n, len, name, _) in d.iter().take(12) {
                        println!("  smallest cells: {name:<14} dx {:.1} mm, {n} cells, length {:.0} mm", dx * 1e3, len * 1e3);
                    }
                    {
                        let gr = &g.exhaust.net.grid;
                        let mut r: Vec<(f32, String)> = gr.ducts.iter().map(|d| ((d.base..d.base + d.n).map(|k| (gr.u[k].abs() + gr.c[k]) * gr.inv_dx_of(k)).fold(0.0f32, f32::max), format!("{} (dx {:.0} mm, T {:.0} K)", d.name, d.dx * 1e3, gr.mean_temperature_of(d.base)))).collect();
                        r.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
                        for (v, name) in r.iter().take(6) {
                            println!("  fastest-rate duct: {name} {v:.0} 1/s");
                        }
                    }
                    println!("  exhaust max_rate {:.0} 1/s, intake max_rate {:.0} 1/s", g.exhaust.net.grid.max_rate, g.intake.net.grid.max_rate);
                    let mut d: Vec<_> = g.intake.net.grid.ducts.iter().map(|d| (d.dx, d.n, d.length, d.name.clone())).collect();
                    d.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                    for (dx, n, len, name) in d.iter().take(5) {
                        println!("  smallest intake cells: {name:<14} dx {:.1} mm, {n} cells, length {:.0} mm", dx * 1e3, len * 1e3);
                    }
                }
                println!("cycles/sample: gas {:.0} (cylinders+valves {:.0}, network {:.0}, reads {:.0}), radiation {:.0}, mechanical {:.0}", snd.prof[0] as f64 / n, g.prof[0] as f64 / n, g.prof[1] as f64 / n, g.prof[2] as f64 / n, snd.prof[2] as f64 / n, snd.prof[3] as f64 / n);
            }
            println!("sources (dB SPL at the listener): tailpipe monopole {:.1}, jet noise {:.1}, pipe shells {:.1}, machinery {:.1}, intake {:.1}", spl((parts[0] / n as f64).sqrt()), spl((parts[1] / n as f64).sqrt()), spl((parts[2] / n as f64).sqrt()), spl((parts[3] / n as f64).sqrt()), spl((parts[4] / n as f64).sqrt()));
            if m.contains_key("bands") {
                println!("band levels, dB SPL at the listener (final output is in dB FS):");
                for (name, x) in ["tailpipe monopole", "jet noise", "pipe shells", "machinery", "intake", "final output"].iter().zip(srcs.iter()) {
                    println!("  {name:<18} {}", band_levels(x, fs));
                }
                let fmax = num(&m, "fmax", 22050.0);
                for (name, x) in ["tail", "jet", "shell", "mech", "intake", "final"].iter().zip(srcs.iter()) {
                    spectrogram_png(&format!("{out_dir}/bands_{id}_{rpm:.0}_{name}.png"), x, SAMPLE_RATE as f32, fmax, 70.0).unwrap();
                }
            }
            if m.contains_key("dump") {
                // raw little-endian f32 pressure (Pa at the listener; the last is the final output) per source, for
                // analysis outside the lab (numpy: np.fromfile(path, "<f4"))
                for (name, x) in ["tail", "jet", "shell", "mech", "intake", "final", "crank"].iter().zip(srcs.iter()) {
                    let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
                    std::fs::write(format!("{out_dir}/raw_{id}_{rpm:.0}_{name}.f32"), bytes).unwrap();
                }
            }
            if m.contains_key("shells") {
                for (b, (name, modes)) in snd.shell_report().iter().enumerate() {
                    let bd = &snd.gas.exhaust.bodies[b];
                    println!("  shell {name:<12} ducts {} surface {:.2} m2 dia {:.0} mm wall {:.1} mm {:?}: {:.1} dB, modes {:?}", bd.ducts.len(), bd.surface, bd.diameter * 1e3, bd.wall.thickness_m * 1e3, bd.wall.material, spl((body_sq[b] / n as f64).sqrt()), modes.iter().map(|f| f.round() as i32).collect::<Vec<_>>());
                }
            }
            println!("p_evo of every cylinder (bar): {}", snd.gas.p_evo.iter().map(|p| format!("{:.2}", p / 1e5)).collect::<Vec<_>>().join(" "));
            println!("cylinders at their last exhaust-valve opening: {:.2} bar (cyl 1), {:.0} K", snd.gas.p_evo[0] / 1e5, snd.gas.t_evo[0]);
            let rms_pa = (tail.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / n as f64).sqrt();
            println!("tailpipe sound at {:.1} m: {:.1} dB SPL = {:.1} dB(A) (rms {:.2} Pa); all sources {:.1} dB(A)", snd.listener_r, spl(rms_pa), a_weighted_spl(&tail, fs), rms_pa, a_weighted_spl(&full_pa, fs));
            {
                let peak = full.iter().fold(0.0f32, |a, v| a.max(v.abs()));
                let over = full.iter().filter(|v| v.abs() > 0.7).count() as f32 / full.len() as f32;
                println!("final output: rms {:.3}, peak {peak:.3}, {:.2}% of samples beyond 0.7 of full scale", rms(&full), over * 100.0);
            }
            let psd = spectrum(&tail, 16384);
            let f_fire = rpm as f64 / 60.0 * cfg.cylinder_count as f64 / 2.0;
            println!("firing frequency {f_fire:.1} Hz; strongest spectral lines (annotated with firing orders):");
            for (f, pw) in peaks(&psd, fs, 25.0, 2500.0, 6.0).iter().take(14) {
                let order = f / (rpm as f64 / 60.0);
                println!("  {f:7.1} Hz  engine order {order:5.2}  ({:.2} x firing)  level {:.1} dB", f / f_fire, db(*pw));
            }
            if m.contains_key("abs") {
                // absolute tailpipe levels per 1/3 octave, dB SPL at the listener (to compare solver settings)
                let nfft = 16384.0;
                let line: Vec<String> = third_octaves(&psd, fs, 63.0, 6300.0).iter().map(|(f, l)| format!("{f:.0}:{:.1}", l + 10.0 * (2.0 / nfft / (20e-6f64).powi(2)).log10())).collect();
                println!("1/3-octave SPL: {}", line.join(" "));
            }
            println!("1/3-octave levels (dB, relative):");
            let tho = third_octaves(&psd, fs, 31.5, 4000.0);
            let top = tho.iter().map(|x| x.1).fold(f64::MIN, f64::max);
            let line: Vec<String> = tho.iter().map(|(f, l)| format!("{f:.0}:{:.0}", l - top)).collect();
            println!("  {}", line.join(" "));
            let base = format!("{out_dir}/steady_{id}_{rpm:.0}");
            write_wav(&format!("{base}.wav"), &full, SAMPLE_RATE).unwrap();
            spectrogram_png(&format!("{base}.png"), &tail, SAMPLE_RATE as f32, 3000.0, 70.0).unwrap();
            println!("wrote {base}.wav and {base}.png");
        }
        "off" => {
            // the engine is not turning: what does the sound model still say? (--hot: pipes at running temperature)
            let secs = num(&m, "secs", 3.0);
            let n_cyl = cfg.cylinder_count;
            // the same state the viewer publishes for an engine that is standing still
            let fuel = fuels().into_iter().find(|f| f.ignition_type == cfg.ignition_type).unwrap();
            let e = CombustionEngine::new(cfg.clone(), &transmissions()[1], &brakes()[0], fuel);
            let tel = e.telemetry();
            let inp = SoundInput {
                atm_pa: tel.atmospheric_pressure_pa as f32,
                map_pa: tel.manifold_pressure_pa as f32,
                air_k: tel.air_temperature_k as f32,
                charge_k: tel.charge_temp_k as f32,
                afr: tel.air_fuel_ratio as f32,
                egt_k: tel.exhaust_gas_temp_k as f32,
                burn: vec![0.0; n_cyl],
                master: 1.0,
                serial: 1,
                ..Default::default()
            };
            println!("telemetry at rest: MAP {:.0} Pa, atm {:.0} Pa, rpm {:.1}, running {}", inp.map_pa, inp.atm_pa, tel.rpm, tel.is_running);
            if m.contains_key("hot") {
                snd.preheat(850.0);
            }
            snd.set_input(&inp);
            let mut sink = vec![0.0f32; fs as usize];
            snd.render(&mut sink);
            let n = (secs as f64 * fs) as usize;
            let (mut out, mut tail, mut jet, mut shell, mut mech, mut intake) = (vec![], vec![], vec![], vec![], vec![], vec![]);
            let mut qs: Vec<f32> = Vec::new();
            for _ in 0..n {
                out.push(snd.next_sample());
                qs.push(snd.gas.mouths[0].q);
                tail.push(snd.p_tail);
                jet.push(snd.p_jet);
                shell.push(snd.p_shell);
                mech.push(snd.p_mech);
                intake.push(snd.p_intake);
            }
            println!("engine off, {secs:.0} s: output rms {:.5} peak {:.5}", rms(&out), out.iter().fold(0.0f32, |a, v| a.max(v.abs())));
            for (name, x) in [("tailpipe monopole", &tail), ("jet noise", &jet), ("pipe shells", &shell), ("machinery", &mech), ("intake", &intake)] {
                println!("  {name:<18} {:6.1} dB SPL | {}", spl(rms(x) as f64), band_levels(x, fs));
            }
            println!("  final output       {:6.1} dB FS  | {}", 20.0 * (rms(&out).max(1e-9)).log10(), band_levels(&out, fs));
            let (mq, mu) = (snd.gas.mouths.iter().map(|m| m.q).fold(0.0f32, |a, v| a.max(v.abs())), snd.gas.mouths.iter().map(|m| m.u).fold(0.0f32, |a, v| a.max(v.abs())));
            println!("  tail mouth flow {mq:.2e} m3/s, face velocity {mu:.2e} m/s");
            {
                let mean = qs.iter().map(|v| *v as f64).sum::<f64>() / qs.len() as f64;
                let sd = (qs.iter().map(|v| (*v as f64 - mean).powi(2)).sum::<f64>() / qs.len() as f64).sqrt();
                let (mn, mx) = qs.iter().fold((f32::MAX, f32::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
                println!("  mouth 0 flow: mean {mean:.3e} sd {sd:.3e} min {mn:.3e} max {mx:.3e} m3/s; spectrum of q: {}", band_levels(&qs.iter().map(|v| v * 1.0e3).collect::<Vec<_>>(), fs));
                let d: Vec<f32> = qs.windows(2).map(|w| (w[1] - w[0]) * 1.0e3).collect();
                println!("  first differences of q (x1e3): rms {:.3e}", rms(&d));
                if m.contains_key("trace") {
                    for (k, c) in qs.chunks(fs as usize / 8).enumerate() {
                        println!("   t={:.3}s q={:.3e}", k as f32 / 8.0, c.iter().sum::<f32>() / c.len() as f32);
                    }
                    for (i, r) in snd.gas.res.iter().enumerate().take(snd.cfg.cylinder_count + 4) {
                        println!("   res {i}: p {:.0} Pa, T {:.0} K, mass {:.3e}", r.p(), r.t(), r.mass);
                    }
                    for (i, nd) in snd.gas.exhaust.net.nodes.iter().enumerate().take(12) {
                        println!("   exh node {i} {}: legs {:?} valves {:?}", nd.name, nd.legs.iter().map(|l| l.p_face).collect::<Vec<_>>(), nd.valves.iter().map(|v| v.area).collect::<Vec<_>>());
                    }
                }
            }
            write_wav(&format!("{out_dir}/off_{id}.wav"), &out, SAMPLE_RATE).unwrap();
        }
        "start" => {
            // a key turn on the real engine model: starter, ignition hold-off, light-off and the settle to idle,
            // fed to the sound model once per 60 Hz frame exactly as the viewer's sim does
            let secs = num(&m, "secs", 7.0);
            let hold = num(&m, "hold", 2.4);
            let t_key = 0.4f32;
            let fuel = fuels().into_iter().find(|f| f.ignition_type == cfg.ignition_type).unwrap();
            let mut e = CombustionEngine::new(cfg.clone(), &transmissions()[1], &brakes()[0], fuel);
            e.set_tuning(tuning.clone());
            e.set_ignition_holdoff(1.1);
            if !m.contains_key("cold") {
                e.warm_up();
            }
            let n_cyl = cfg.cylinder_count;
            let tq_ref = (75.0 * cfg.displacement_l()).max(60.0);
            if m.contains_key("hot") {
                snd.preheat(650.0);
            }
            let mut burn = vec![0.0f32; n_cyl];
            let mut out: Vec<f32> = Vec::new();
            let mut tail: Vec<f32> = Vec::new();
            // the sources behind the mix: tailpipe, valve-jet noise, pipe shells, machinery (starter, rattle, ...), intake, low-end layer
            let mut layers: [Vec<f32>; 6] = Default::default();
            let mut rpm_log: Vec<(f32, f32)> = Vec::new();
            let frame = 1.0 / 60.0f64;
            let per_frame = (fs / 60.0) as usize;
            let mut serial = 1u64;
            let (mut was_unlit, mut lit_at) = (false, None::<f32>);
            // speed ripple while cranking, as the engine model has it and as the sound model reconstructs it
            let (mut sim_min, mut sim_max, mut snd_min, mut snd_max) = (f64::MAX, 0.0f64, f32::MAX, f32::MIN);
            for f in 0..(secs as f64 * 60.0) as usize {
                let t = f as f32 / 60.0;
                let held = t >= t_key && t < t_key + hold || (t >= t_key + hold && e.rpm() < 500.0 && t < t_key + hold + 2.0);
                if held {
                    e.engage_starter();
                } else {
                    e.release_starter();
                }
                let (mut sim_dt, mut acc, mut tacc) = (frame, 0.0f64, 0.0f64);
                while sim_dt > 1e-9 {
                    let w_deg = (e.rpm() * 6.0).max(1.0);
                    let h = (1.2 / w_deg).clamp(1.0 / 20_000.0, 1.0 / 240.0).min(sim_dt);
                    e.update(h);
                    acc += e.rpm() * h;
                    tacc += h;
                    sim_dt -= h;
                    if (0.9..1.4).contains(&t) {
                        sim_min = sim_min.min(e.rpm());
                        sim_max = sim_max.max(e.rpm());
                    }
                    for l in 0..n_cyl {
                        let s = e.cylinder_sample(l);
                        if (520.0..530.0).contains(&s.local_angle_deg) {
                            burn[(s.label - 1) as usize] = s.burn_fraction as f32;
                        }
                    }
                }
                let tel = e.telemetry();
                let unlit = e.is_cranking_unlit();
                if was_unlit && !unlit && tel.is_running {
                    lit_at = Some(t);
                }
                was_unlit = unlit;
                let rpm_avg = (acc / tacc.max(1e-9)) as f32;
                rpm_log.push((t, rpm_avg));
                let inp = SoundInput {
                    rpm: rpm_avg,
                    throttle: tel.throttle as f32,
                    load: (tel.display_torque_nm / tq_ref).clamp(0.0, 1.3) as f32,
                    map_pa: tel.manifold_pressure_pa as f32,
                    atm_pa: tel.atmospheric_pressure_pa as f32,
                    air_k: tel.air_temperature_k as f32,
                    charge_k: tel.charge_temp_k as f32,
                    afr: tel.air_fuel_ratio as f32,
                    egt_k: tel.exhaust_gas_temp_k as f32,
                    boost: (tel.boost_pressure_pa / 1e5) as f32,
                    spool: tel.turbo_spool_fraction as f32,
                    running: tel.is_running,
                    lit: !unlit,
                    cranking: tel.is_cranking,
                    knock: tel.knock_intensity as f32,
                    vehicle_speed: 0.0,
                    gear_whine: 0.0,
                    burn: burn.clone(),
                    master: 1.0,
                    serial,
                    turbo_speed: tel.turbo_spool_fraction as f32,
                    burble: 0.6,
                    backfire: 0.0,
                };
                serial += 1;
                snd.set_input(&inp);
                for _ in 0..per_frame {
                    out.push(snd.next_sample());
                    tail.push(snd.p_tail + snd.p_jet + snd.p_shell + snd.p_mech + snd.p_intake);
                    for (l, v) in layers.iter_mut().zip([snd.p_tail, snd.p_jet, snd.p_shell, snd.p_mech, snd.p_intake - 0.0, snd.p_low]) {
                        l.push(v);
                    }
                    if (0.9..1.4).contains(&t) {
                        let d = snd.crank_speed_deviation();
                        snd_min = snd_min.min(d);
                        snd_max = snd_max.max(d);
                    }
                }
            }
            println!("cranking speed ripple 0.9-1.4 s: engine model {:.0}..{:.0} rpm; sound model reconstruction {:+.0}%..{:+.0}%", sim_min, sim_max, snd_min * 100.0, snd_max * 100.0);
            println!("start: key at {t_key:.1} s, light-off at {:?}; rpm trace:", lit_at);
            let line: Vec<String> = rpm_log.iter().step_by(12).map(|(t, r)| format!("{t:.1}s:{r:.0}")).collect();
            println!("  {}", line.join(" "));
            let seg = |a: f32, b: f32| -> (usize, usize) { ((a * fs as f32) as usize, ((b * fs as f32) as usize).min(out.len())) };
            for (name, a, b) in [("silence before key", 0.1, 0.35), ("cranking", 0.9, 1.45), ("after light-off", lit_at.unwrap_or(2.0) + 0.5, lit_at.unwrap_or(2.0) + 1.5), ("late idle", secs - 1.2, secs - 0.2)] {
                let (i0, i1) = seg(a, b);
                if i1 > i0 + 4096 {
                    println!("  {name:<20} out rms {:.4} | {}", rms(&out[i0..i1]), band_levels(&out[i0..i1], fs));
                    println!("  {:<20} pa      | {}", "", band_levels(&tail[i0..i1], fs));
                    if m.contains_key("layers") {
                        for (nm, l) in ["tail", "jet", "shell", "mech", "intake", "low"].iter().zip(layers.iter()) {
                            println!("  {:<20} {nm:<7}| {}", "", band_levels(&l[i0..i1], fs));
                        }
                    }
                }
            }
            let base = format!("{out_dir}/start_{id}");
            write_wav(&format!("{base}.wav"), &out, SAMPLE_RATE).unwrap();
            spectrogram_png(&format!("{base}.png"), &out, SAMPLE_RATE as f32, num(&m, "fmax", 8000.0), 70.0).unwrap();
            spectrogram_png(&format!("{base}_lf.png"), &out, SAMPLE_RATE as f32, 1000.0, 70.0).unwrap();
            println!("wrote {base}.wav and {base}.png");
        }
        "thermal" => {
            let rpm = num(&m, "rpm", 3000.0);
            let thr = num(&m, "throttle", 0.8);
            let inp = operating_point(&cfg, &tuning, rpm as f64, thr as f64);
            snd.set_input(&inp);
            let secs = num(&m, "secs", 4.0);
            snd.soak(secs, num(&m, "accel", 60.0));
            println!("after {secs:.0} s at {rpm:.0} rpm x{:.0} thermal acceleration ({:.0} s of heating):", num(&m, "accel", 60.0), secs * num(&m, "accel", 60.0));
            let sys = &snd.gas.exhaust;
            let mut rows: Vec<(f32, String, f32, f32, f32)> = Vec::new();
            for (d, di) in sys.net.grid.ducts.iter().enumerate() {
                if di.name.ends_with('0') || di.name.contains("0_") || di.name.contains("0a") || di.name.contains("0b") || di.name.starts_with("primary1") || di.name.starts_with("collector0") {
                    rows.push((sys.dist[d], di.name.clone(), sys.net.grid.mean_temperature(d), sys.net.grid.tw_at(d, di.n / 2), di.length));
                }
            }
            rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            for (dist, name, tg, tw, len) in rows {
                println!("  {dist:5.2} m  {name:<14} gas {:4.0} C  wall {:4.0} C  (c = {:3.0} m/s, {len:.2} m long)", tg - 273.15, tw - 273.15, Gas::EXHAUST.sound_speed(tg));
            }
        }
        "ripple" => {
            // the crank speed of a running engine: the engine model's own, within the cycle and over many cycles, next to what
            // the sound model reconstructs from its combustion torque (`--throttle` 0 is the idle)
            let thr = num(&m, "throttle", 0.0) as f64;
            let secs = num(&m, "secs", 6.0) as f64;
            let fuel = fuels().into_iter().find(|f| f.ignition_type == cfg.ignition_type).unwrap();
            let mut e = CombustionEngine::new(cfg.clone(), &transmissions()[1], &brakes()[0], fuel);
            e.set_tuning(tuning.clone());
            e.warm_up();
            let rpm0 = num(&m, "rpm", cfg.redline_rpm as f32 * 0.13) as f64;
            let _ = e.steady_point(rpm0, thr.max(0.05));
            e.set_throttle(thr);
            for _ in 0..(2.0 / (1.0 / 5000.0)) as usize {
                e.update(1.0 / 5000.0);
            }
            let (mut lo, mut hi, mut acc, mut acc2, mut cnt) = (f64::MAX, 0.0f64, 0.0f64, 0.0f64, 0usize);
            let mut slow: Vec<f64> = Vec::new();
            let h = 1.0 / 20_000.0;
            let mut tmp = 0.0;
            for k in 0..(secs / h) as usize {
                e.update(h);
                let r = e.rpm();
                lo = lo.min(r);
                hi = hi.max(r);
                acc += r;
                acc2 += r * r;
                cnt += 1;
                tmp += r;
                if (k + 1) % 2000 == 0 {
                    slow.push(tmp / 2000.0);
                    tmp = 0.0;
                }
            }
            let mean = acc / cnt as f64;
            let sd_slow = {
                let mu = slow.iter().sum::<f64>() / slow.len() as f64;
                (slow.iter().map(|v| (v - mu).powi(2)).sum::<f64>() / slow.len() as f64).sqrt()
            };
            println!("engine model at {mean:.0} rpm: peak to peak {:.2} % within the run, sd of the 0.1 s means {:.2} rpm ({:.2} %)", (hi - lo) / mean * 100.0, sd_slow, sd_slow / mean * 100.0);
            let _ = acc2;
            let mut inp = operating_point(&cfg, &tuning, mean, thr.max(0.0));
            inp.load = (e.telemetry().display_torque_nm / (75.0 * cfg.displacement_l()).max(60.0)).clamp(0.0, 1.3) as f32;
            snd.preheat(inp.egt_k * 0.9);
            snd.set_input(&inp);
            snd.soak(2.0, 60.0);
            let (mut dmin, mut dmax) = (f32::MAX, f32::MIN);
            let mut slow_s: Vec<f64> = Vec::new();
            let mut tmp = 0.0f64;
            let n = (secs * fs) as usize;
            for k in 0..n {
                snd.next_sample();
                let d = snd.crank_speed_deviation();
                dmin = dmin.min(d);
                dmax = dmax.max(d);
                tmp += d as f64;
                if (k + 1) % 4410 == 0 {
                    slow_s.push(tmp / 4410.0);
                    tmp = 0.0;
                }
            }
            let sd = {
                let mu = slow_s.iter().sum::<f64>() / slow_s.len() as f64;
                (slow_s.iter().map(|v| (v - mu).powi(2)).sum::<f64>() / slow_s.len() as f64).sqrt()
            };
            println!("sound model: peak to peak {:.2} %, sd of the 0.1 s means {:.2} rpm ({:.2} %)", (dmax - dmin) as f64 * 100.0, sd * mean, sd * 100.0);
        }
        "lift" => {
            // wide open at the start, then off the throttle: the overrun burble
            let (from, to, secs) = (num(&m, "from", 5500.0), num(&m, "to", 2200.0), num(&m, "secs", 5.0));
            let wot = operating_point(&cfg, &tuning, from as f64, 1.0);
            let off = operating_point(&cfg, &tuning, to as f64, 0.0);
            println!("wide open: spool {:.2}, boost {:.2} bar, MAP {:.0} kPa", wot.spool, wot.boost, wot.map_pa / 1e3);
            snd.preheat(wot.egt_k * 0.9);
            snd.set_input(&wot);
            snd.soak(2.0, 60.0);
            let n = (secs as f64 * fs) as usize;
            let mut full = Vec::with_capacity(n);
            let mut serial = 2u64;
            let block = 735;
            let (mut jet_peak, mut jet_big) = (0.0f32, 0usize);
            let mut burble = wot.clone();
            burble.burble = num(&m, "burble", 1.0);
            for b in 0..(n / block) {
                let t = (b * block) as f32 / fs as f32;
                let mut cur = if t < 1.0 { wot.clone() } else { off.clone() };
                cur.burble = burble.burble;
                if t >= 1.0 {
                    cur.rpm = from + (to - from) * (1.0 - (-(t - 1.0) * 1.1).exp()) / (1.0 - (-(secs - 1.0) * 1.1).exp());
                    // the turbo's spool and the charge pressure fall away over a few tenths of a second, as the engine model has them
                    let k = (-(t - 1.0) / 0.35).exp();
                    cur.spool = off.spool + (wot.spool - off.spool) * k;
                    cur.boost = off.boost + (wot.boost - off.boost) * k;
                    cur.turbo_speed = cur.spool;
                }
                cur.serial = serial;
                serial += 1;
                snd.set_input(&cur);
                for _ in 0..block {
                    full.push(snd.next_sample());
                    if t >= 1.0 {
                        jet_peak = jet_peak.max(snd.p_jet.abs());
                        if snd.p_jet.abs() > 0.5 {
                            jet_big += 1;
                        }
                    }
                }
            }
            println!("jet layer peak {jet_peak:.2} Pa, {jet_big} samples above 0.5 Pa");
            println!("pops fired during the overrun: {} ({:.1} per second)", snd.gas.pops_fired, snd.gas.pops_fired as f32 / (secs - 1.0));
            let base = format!("{out_dir}/lift_{id}");
            write_wav(&format!("{base}.wav"), &full, SAMPLE_RATE).unwrap();
            spectrogram_png(&format!("{base}.png"), &full, SAMPLE_RATE as f32, 4000.0, 70.0).unwrap();
            println!("wrote {base}.wav and {base}.png");
        }
        "sweep" => {
            let (from, to, secs) = (num(&m, "from", 900.0), num(&m, "to", cfg.redline_rpm as f32 * 0.95), num(&m, "secs", 12.0));
            let n = (secs as f64 * fs) as usize;
            let mut full = Vec::with_capacity(n);
            let mut tail = Vec::with_capacity(n);
            let mut inp = operating_point(&cfg, &tuning, from as f64, 1.0);
            snd.preheat(inp.egt_k * 0.9);
            let hi = operating_point(&cfg, &tuning, to as f64, 1.0);
            let lo = operating_point(&cfg, &tuning, from as f64, 0.2);
            let t0 = std::time::Instant::now();
            let block = 735;
            let mut serial = 1u64;
            for b in 0..(n / block) {
                let x = (b * block) as f32 / n as f32;
                let ramp = (x * 1.25).min(1.0);
                // lerp the operating point between the low and high ends
                let mut cur = if x < 0.12 { lo.clone() } else { hi.clone() };
                cur.rpm = from + (to - from) * ramp * ramp.sqrt();
                let w = ((cur.rpm - from) / (to - from)).clamp(0.0, 1.0);
                cur.map_pa = lo.map_pa + (hi.map_pa - lo.map_pa) * (if x < 0.12 { 0.0 } else { 1.0 });
                cur.load = if x < 0.12 { lo.load } else { hi.load * (0.5 + 0.5 * w) };
                cur.throttle = if x < 0.12 { 0.2 } else { 1.0 };
                cur.serial = serial;
                serial += 1;
                inp = cur;
                snd.set_input(&inp);
                for _ in 0..block {
                    full.push(snd.next_sample());
                    tail.push(snd.tail_pressure());
                }
            }
            let el = t0.elapsed().as_secs_f64();
            println!("rendered {secs:.1} s in {el:.2} s ({:.1}x real time)", secs as f64 / el);
            let base = format!("{out_dir}/sweep_{id}");
            write_wav(&format!("{base}.wav"), &full, SAMPLE_RATE).unwrap();
            spectrogram_png(&format!("{base}.png"), &tail, SAMPLE_RATE as f32, 2500.0, 70.0).unwrap();
            println!("wrote {base}.wav and {base}.png");
        }
        "resonances" => {
            let rpm = num(&m, "rpm", 4000.0);
            let thr = num(&m, "throttle", 1.0);
            let f_max = num(&m, "fmax", 600.0) as f64;
            let inp = operating_point(&cfg, &tuning, rpm as f64, thr as f64);
            snd.preheat(inp.egt_k * 0.9);
            snd.set_input(&inp);
            snd.soak(2.0, 60.0);
            let sys = &snd.gas.exhaust;
            // length-weighted mean gas temperature of the live pipes
            let (mut tw, mut lw) = (0.0f64, 0.0f64);
            for (d, di) in sys.net.grid.ducts.iter().enumerate() {
                tw += sys.net.grid.mean_temperature(d) as f64 * di.length as f64;
                lw += di.length as f64;
            }
            let t_gas = num(&m, "temp", (tw / lw) as f32);
            let lin = Linear { temperature: Some(t_gas), alpha: 0.01, ..Linear::new(&sys.net, &sys.chambers, sys.res_base) };
            let mut pk = lin.resonances(sys.port_node[0], 20.0, f_max, 60);
            let strongest = pk.iter().map(|p| p.1).fold(0.0f64, f64::max);
            pk.retain(|p| p.1 > strongest * 0.02);
            pk.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let c = Gas::EXHAUST.sound_speed(t_gas) as f64;
            let l = sys.report.path_length as f64;
            println!("mean exhaust gas {t_gas:.0} K (c = {c:.0} m/s), pipe path {l:.2} m from port to tailpipe");
            println!("quarter-wave estimate of the whole pipe (closed port, open tail): c/4L = {:.0} Hz, then odd multiples; half-wave: c/2L = {:.0} Hz and multiples", c / (4.0 * l), c / (2.0 * l));
            println!("transfer-matrix resonances of the radiated sound (relative level, dB):");
            let f_fire_order = cfg.cylinder_count as f64 / 2.0;
            for (f, r) in &pk {
                // a firing harmonic h is order h * cylinders / 2 of the crank speed; it meets this resonance at 60 f / order
                let meets: Vec<String> = (1..=4)
                    .filter_map(|h| {
                        let rpm_hit = 60.0 * f / (h as f64 * f_fire_order);
                        (rpm_hit >= 600.0 && rpm_hit <= cfg.redline_rpm as f64 * 1.05).then(|| format!("{rpm_hit:.0} rpm (firing harmonic {h})"))
                    })
                    .collect();
                println!("  {f:6.1} Hz  {:+5.1} dB   {}", 20.0 * (r / strongest).log10(), meets.join(", "));
            }
        }
        "compare" => {
            let Some(path) = m.get("ref") else {
                eprintln!("compare needs --ref recording.wav (a recording of the same engine at a steady speed; --rpm gives that speed)");
                return;
            };
            let (reference, ref_fs) = match read_wav(path) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("cannot read {path}: {e}");
                    return;
                }
            };
            let rpm = num(&m, "rpm", 3000.0);
            let thr = num(&m, "throttle", 0.8);
            let secs = num(&m, "secs", 4.0);
            let inp = operating_point(&cfg, &tuning, rpm as f64, thr as f64);
            snd.preheat(inp.egt_k * 0.9);
            snd.set_input(&inp);
            snd.soak(2.0, 60.0);
            let mut sink = vec![0.0f32; fs as usize];
            snd.render(&mut sink);
            let n = (secs as f64 * fs) as usize;
            let mut model = Vec::with_capacity(n);
            for _ in 0..n {
                snd.next_sample();
                model.push(snd.p_tail + snd.p_jet + snd.p_shell + snd.p_mech + snd.p_intake);
            }
            let ref_fs = ref_fs as f64;
            let nfft = 16384;
            let (pm, pr) = (spectrum(&model, nfft), spectrum(&reference, nfft));
            let (bm, br) = (third_octaves(&pm, fs, 50.0, 8000.0), third_octaves(&pr, ref_fs, 50.0, 8000.0));
            let norm = |b: &[(f64, f64)]| db(b.iter().map(|x| 10f64.powf(x.1 / 10.0)).sum::<f64>());
            let (nm, nr) = (norm(&bm), norm(&br));
            println!("model: {:.1} dB(A) at {:.1} m; reference: {} samples at {ref_fs:.0} Hz ({:.1} s)", a_weighted_spl(&model, fs), snd.listener_r, reference.len(), reference.len() as f64 / ref_fs);
            println!("1/3-octave bands, each normalised to its own total power (dB):");
            println!("   band Hz   model  reference  difference");
            let (mut sq, mut cnt) = (0.0f64, 0usize);
            for ((f, a), (_, b)) in bm.iter().zip(&br) {
                if *f > ref_fs / 2.0 * 0.9 {
                    continue;
                }
                let (a, b) = (a - nm, b - nr);
                println!("  {f:7.0}  {a:6.1}  {b:6.1}     {:+5.1}", a - b);
                if *f >= 63.0 && *f <= 4000.0 {
                    sq += (a - b).powi(2);
                    cnt += 1;
                }
            }
            println!("rms band difference 63 Hz - 4 kHz: {:.1} dB over {cnt} bands", (sq / cnt.max(1) as f64).sqrt());
            let (cm, cr) = (centroid(&pm, fs, 50.0, 4000.0), centroid(&pr, ref_fs, 50.0, 4000.0));
            println!("spectral centroid 50 Hz - 4 kHz: model {cm:.0} Hz, reference {cr:.0} Hz");
            let f_fire = rpm as f64 / 60.0 * cfg.cylinder_count as f64 / 2.0;
            println!("firing frequency {f_fire:.1} Hz. Strongest lines (frequency, in firing orders):");
            for (label, psd, f_s) in [("model    ", &pm, fs), ("reference", &pr, ref_fs)] {
                let lines: Vec<String> = peaks(psd, f_s, 25.0, 2000.0, 6.0).iter().take(8).map(|(f, _)| format!("{f:.0} Hz ({:.2})", f / f_fire)).collect();
                println!("  {label}: {}", lines.join(", "));
            }
        }
        other => {
            eprintln!("unknown command {other}: use steady | sweep | lift | thermal | resonances | compare");
        }
    }
}
