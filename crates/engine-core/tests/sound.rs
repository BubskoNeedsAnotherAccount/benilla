//! The sound model must stay finite, bounded and physically sensible for every engine and every parts combination.

use engine_core::sound::{default_banks, EngineSound, Quality, SoundInput};
use engine_core::*;

fn input(cfg: &EngineConfig, tuning: &Tuning, rpm: f64, throttle: f64) -> SoundInput {
    let fuel = fuels()
        .into_iter()
        .find(|f| f.ignition_type == cfg.ignition_type)
        .unwrap();
    let mut e = CombustionEngine::new(cfg.clone(), &transmissions()[1], &brakes()[0], fuel);
    e.set_tuning(tuning.clone());
    e.warm_up();
    let tel = e.steady_point(rpm, throttle);
    SoundInput {
        rpm: rpm as f32,
        throttle: throttle as f32,
        load: (tel.display_torque_nm / (75.0 * cfg.displacement_l()).max(60.0)).clamp(0.0, 1.3)
            as f32,
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
        burble: 0.5,
        vehicle_speed: 10.0,
        ..Default::default()
    }
}

fn parts() -> Vec<(&'static str, Tuning)> {
    vec![
        ("stock", Tuning::default()),
        ("itb + open stack + long tubes + straight pipes", Tuning::parse_spec("runners=itb,filter=open,header=long,cat=delete,muffler=delete,pipe=1.3,cross=x").unwrap()),
        ("titanium sport", Tuning::parse_spec("pipemat=ti,headermat=ti,wall=0.7,muffler=sport,cat=highflow,plen=0.7,pdia=1.2,mid=0.6,tail=2.0,cross=none").unwrap()),
        ("turbo kit, cast headers, wrapped", Tuning::parse_spec("turbos=2,boost=1.2,size=0.8,headermat=iron,wrap=1,muffler=straight").unwrap()),
        ("four turbos, long runners", Tuning::parse_spec("turbos=4,runners=long,filter=cold,header=shorty,muffler=sport").unwrap()),
    ]
}

#[test]
fn every_engine_and_every_kit_renders_finite_bounded_sound() {
    let mut worst_load = 0.0f64;
    for cfg in engines() {
        for (name, tuning) in parts() {
            for (rpm, thr) in [(cfg.redline_rpm * 0.12, 0.0), (cfg.redline_rpm * 0.8, 1.0)] {
                let inp = input(&cfg, &tuning, rpm, thr);
                let mut snd = EngineSound::new(&cfg, &tuning, &default_banks(&cfg), Quality::FAST);
                snd.preheat(inp.egt_k * 0.9);
                snd.set_input(&inp);
                let n = (0.7 * 44_100.0) as usize;
                let t0 = std::time::Instant::now();
                let (mut peak, mut q_sum, mut q_n) = (0.0f32, 0.0f64, 0usize);
                for i in 0..n {
                    let s = snd.next_sample();
                    assert!(
                        s.is_finite(),
                        "{} / {name} at {rpm:.0} rpm: non-finite sample {i}",
                        cfg.id
                    );
                    peak = peak.max(s.abs());
                    if i > n / 2 {
                        q_sum += snd.gas.mouths.iter().map(|m| m.q as f64).sum::<f64>();
                        q_n += 1;
                    }
                }
                worst_load = worst_load.max(t0.elapsed().as_secs_f64() / 0.7);
                assert!(
                    peak <= 1.0,
                    "{} / {name}: output beyond full scale ({peak})",
                    cfg.id
                );
                let mean_q = q_sum / q_n as f64;
                assert!(mean_q > 0.0, "{} / {name} at {rpm:.0} rpm: gas must leave the tailpipes on average (mean flow {mean_q})", cfg.id);
                // the pipes must not have run away thermally or in pressure
                let g = &snd.gas.exhaust.net.grid;
                for k in 0..g.m {
                    assert!(
                        g.p[k].is_finite() && g.p[k] > 5.0e3 && g.p[k] < 3.0e6,
                        "{} / {name}: pressure {} Pa in cell {k}",
                        cfg.id,
                        g.p[k]
                    );
                }
            }
        }
    }
    println!(
        "slowest case rendered at {:.1}x real time",
        1.0 / worst_load
    );
}

#[test]
fn materials_change_the_shell_radiation_and_not_the_firing_note() {
    let cfg = engine_by_id("v8").unwrap();
    // averaged over a sweep: where a shell mode happens to meet an engine order swings the level by 10 dB
    let render = |spec: &str| {
        let tuning = Tuning::parse_spec(spec).unwrap();
        let (mut tail, mut shell, mut n) = (0.0f64, 0.0f64, 0usize);
        for rpm in [2500.0, 3100.0, 3700.0, 4300.0, 4900.0, 5500.0] {
            let inp = input(&cfg, &tuning, rpm, 1.0);
            let mut snd = EngineSound::new(&cfg, &tuning, &default_banks(&cfg), Quality::FAST);
            snd.preheat(inp.egt_k * 0.9);
            snd.set_input(&inp);
            snd.soak(0.5, 80.0);
            for _ in 0..20_000 {
                snd.next_sample();
                tail += (snd.p_tail as f64).powi(2);
                shell += (snd.p_shell as f64).powi(2);
                n += 1;
            }
        }
        ((tail / n as f64).sqrt(), (shell / n as f64).sqrt())
    };
    let (t_steel, s_steel) = render("pipemat=steel");
    let (t_thin, s_thin) = render("pipemat=steel,wall=0.6");
    let (t_thick, s_thick) = render("pipemat=steel,wall=2.0");
    println!("tail {t_steel:.3} / {t_thin:.3} / {t_thick:.3} Pa, shells {s_steel:.4} / {s_thin:.4} / {s_thick:.4} Pa");
    // the tailpipe note barely moves with the wall gauge, the shells move, in the direction physics says
    assert!((t_thin / t_steel - 1.0).abs() < 0.15 && (t_thick / t_steel - 1.0).abs() < 0.15);
    assert!(
        s_thin > s_thick * 1.3,
        "a thin shell must radiate more than a thick one ({s_thin} vs {s_thick})"
    );
}

#[test]
fn longer_pipes_pack_in_more_resonances_and_hotter_gas_raises_them_all() {
    use engine_core::acoustics::linear::Linear;
    let cfg = engine_by_id("i4").unwrap();
    let peaks = |spec: &str, t_gas: f32| {
        let tuning = Tuning::parse_spec(spec).unwrap();
        let env = engine_core::acoustics::Env::new(101_325.0, 293.0);
        let sys =
            engine_core::acoustics::ExhaustSystem::build(&engine_core::acoustics::ExhaustInputs {
                cfg: &cfg,
                tuning: &tuning,
                banks: &default_banks(&cfg),
                dx: 0.04,
                env,
                res_base: 4,
            });
        let lin = Linear {
            temperature: Some(t_gas),
            alpha: 0.01,
            ..Linear::new(&sys.net, &sys.chambers, sys.res_base)
        };
        lin.resonances(sys.port_node[0], 20.0, 600.0, 200)
    };
    // the mode density of a pipe grows with its length: count the resonances below 300 Hz
    let count = |spec: &str| peaks(spec, 650.0).iter().filter(|p| p.0 < 300.0).count();
    let (short, stock, long) = (
        count("mid=0.5,tail=0.5,muffler=delete,cat=delete"),
        count("muffler=delete,cat=delete"),
        count("mid=1.8,tail=2.0,muffler=delete,cat=delete"),
    );
    println!("resonances below 300 Hz: short pipes {short}, stock {stock}, long {long}");
    assert!(short < long, "{short} < {stock} < {long}");
    assert!(short <= stock && stock <= long);
    // every resonance scales with the speed of sound: the whole set of peaks moves up with sqrt(T)
    let set = |t: f32| {
        let mut p: Vec<f64> = peaks("muffler=delete,cat=delete", t)
            .iter()
            .filter(|x| x.0 < 450.0)
            .map(|x| x.0)
            .collect();
        p.sort_by(|a, b| a.partial_cmp(b).unwrap());
        p
    };
    let (cool, hot) = (set(450.0), set(900.0));
    // the c ratio: gamma and R vary a little with temperature
    let ratio = (engine_core::acoustics::Gas::EXHAUST.sound_speed(900.0)
        / engine_core::acoustics::Gas::EXHAUST.sound_speed(450.0)) as f64;
    // every cool peak has a hot partner at c-ratio times the frequency (within 12 %: the cold-air end correction does not scale)
    let matched = cool
        .iter()
        .filter(|f| hot.iter().any(|g| (g / (*f * ratio) - 1.0).abs() < 0.12))
        .count();
    println!("{matched} of {} resonances found again at {:.2}x the frequency when the gas is 450 K hotter", cool.len(), ratio);
    assert!(
        matched * 2 >= cool.len(),
        "only {matched} of {} resonances moved with the speed of sound",
        cool.len()
    );
}

// ------------------------------------------------------------------ the engine at rest and being started

/// The state the viewer publishes for an engine that is standing still.
fn resting_input(cfg: &EngineConfig) -> SoundInput {
    let fuel = fuels()
        .into_iter()
        .find(|f| f.ignition_type == cfg.ignition_type)
        .unwrap();
    let e = CombustionEngine::new(cfg.clone(), &transmissions()[1], &brakes()[0], fuel);
    let tel = e.telemetry();
    SoundInput {
        atm_pa: tel.atmospheric_pressure_pa as f32,
        map_pa: tel.manifold_pressure_pa as f32,
        air_k: tel.air_temperature_k as f32,
        charge_k: tel.charge_temp_k as f32,
        afr: tel.air_fuel_ratio as f32,
        egt_k: tel.exhaust_gas_temp_k as f32,
        burn: vec![0.0; cfg.cylinder_count],
        master: 1.0,
        serial: 1,
        ..Default::default()
    }
}

fn rms_of(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn an_engine_that_is_off_is_silent() {
    // The loudness compressor lifts whatever is left of a quiet signal by up to 15 dB, and the pipe solver always
    // has a floor of round-off noise: with the engine off that floor was a faint airy hiss. It must stay digital zero.
    for id in ["i4", "v8", "i4_16", "w24"] {
        let cfg = engine_by_id(id).unwrap();
        let mut snd = EngineSound::new(
            &cfg,
            &Tuning::default(),
            &default_banks(&cfg),
            Quality::FAST,
        );
        let inp = resting_input(&cfg);
        snd.set_input(&inp);
        let x: Vec<f32> = (0..(2.0 * 44_100.0) as usize)
            .map(|_| snd.next_sample())
            .collect();
        let r = rms_of(&x[22_050..]);
        println!("{id}: cold and still, rms {r:.2e}");
        assert!(
            r < 1.0e-6,
            "{id}: a stopped cold engine makes sound (rms {r:.2e})"
        );

        // hot pipes that have just been running, then the engine stops
        snd.preheat(850.0);
        let run = input(&cfg, &Tuning::default(), 2500.0, 0.6);
        snd.set_input(&run);
        for _ in 0..22_050 {
            snd.next_sample();
        }
        let loud = rms_of(&(0..11_025).map(|_| snd.next_sample()).collect::<Vec<_>>());
        snd.set_input(&SoundInput {
            serial: 3,
            ..inp.clone()
        });
        let y: Vec<f32> = (0..(4.0 * 44_100.0) as usize)
            .map(|_| snd.next_sample())
            .collect();
        let tail = rms_of(&y[3 * 44_100..]);
        println!("{id}: running rms {loud:.3}, three seconds after stopping {tail:.2e}");
        assert!(
            tail < loud * 1.0e-3,
            "{id}: still audible 3 s after stopping ({tail:.2e} vs {loud:.3})"
        );
    }
}

#[test]
fn intake_and_exhaust_share_one_ambient_pressure() {
    // The engine model's atmosphere depends on its air temperature (~105 kPa at 33 C) while the pipe model sits at
    // 101.3 kPa. Giving the intake the first and the exhaust the second blew a 3 m/s draught through whichever
    // cylinder rested in valve overlap.
    for id in ["i4", "v8", "i6"] {
        let cfg = engine_by_id(id).unwrap();
        let mut snd = EngineSound::new(
            &cfg,
            &Tuning::default(),
            &default_banks(&cfg),
            Quality::FAST,
        );
        let inp = SoundInput {
            atm_pa: 105_500.0,
            map_pa: 105_500.0,
            ..resting_input(&cfg)
        };
        snd.set_input(&inp);
        let mut peak = 0.0f32;
        for i in 0..(2.0 * 44_100.0) as usize {
            snd.next_sample();
            if i > 44_100 {
                peak = peak.max(snd.gas.mouths.iter().map(|m| m.q.abs()).fold(0.0, f32::max));
            }
        }
        println!("{id}: largest flow at a tailpipe at rest {peak:.2e} m3/s");
        assert!(
            peak < 2.0e-4,
            "{id}: a stopped engine blows {peak:.2e} m3/s out of the tailpipe"
        );
    }
}

/// Drives the real engine model through a key turn and returns the sound input of every 60 Hz frame and the engine
/// model's crank speed extremes between 0.9 s and 1.4 s.
fn cranking_inputs(cfg: &EngineConfig, secs: f32) -> (Vec<SoundInput>, (f64, f64)) {
    let fuel = fuels()
        .into_iter()
        .find(|f| f.ignition_type == cfg.ignition_type)
        .unwrap();
    let mut e = CombustionEngine::new(cfg.clone(), &transmissions()[1], &brakes()[0], fuel);
    e.set_ignition_holdoff(1.1);
    e.warm_up();
    let n = cfg.cylinder_count;
    let (mut lo, mut hi) = (f64::MAX, 0.0f64);
    let mut burn = vec![0.0f32; n];
    let mut frames = Vec::new();
    for f in 0..(secs * 60.0) as usize {
        let t = f as f32 / 60.0;
        if (0.4..2.4).contains(&t) {
            e.engage_starter();
        } else {
            e.release_starter();
        }
        let (mut dt, mut acc, mut tacc) = (1.0 / 60.0, 0.0f64, 0.0f64);
        while dt > 1e-9 {
            let h = (1.2 / (e.rpm() * 6.0).max(1.0))
                .clamp(1.0 / 20_000.0, 1.0 / 240.0)
                .min(dt);
            e.update(h);
            acc += e.rpm() * h;
            tacc += h;
            dt -= h;
            if (0.9..1.4).contains(&t) {
                lo = lo.min(e.rpm());
                hi = hi.max(e.rpm());
            }
            for l in 0..n {
                let s = e.cylinder_sample(l);
                if (520.0..530.0).contains(&s.local_angle_deg) {
                    burn[(s.label - 1) as usize] = s.burn_fraction as f32;
                }
            }
        }
        let tel = e.telemetry();
        frames.push(SoundInput {
            rpm: (acc / tacc) as f32,
            throttle: tel.throttle as f32,
            load: (tel.display_torque_nm / (75.0 * cfg.displacement_l()).max(60.0)).clamp(0.0, 1.3)
                as f32,
            map_pa: tel.manifold_pressure_pa as f32,
            atm_pa: tel.atmospheric_pressure_pa as f32,
            air_k: tel.air_temperature_k as f32,
            charge_k: tel.charge_temp_k as f32,
            afr: tel.air_fuel_ratio as f32,
            egt_k: tel.exhaust_gas_temp_k as f32,
            running: tel.is_running,
            lit: !e.is_cranking_unlit(),
            cranking: tel.is_cranking,
            burn: burn.clone(),
            master: 1.0,
            serial: f as u64 + 1,
            burble: 0.3,
            ..Default::default()
        });
    }
    (frames, (lo, hi))
}

#[test]
fn cranking_sags_and_surges_like_the_engine_model() {
    // The starter's pitch follows the crank, which slows on every compression stroke. The sound model rebuilds that
    // speed ripple from its own cylinder pressures; it must agree with the engine model's to within a factor of two.
    for id in ["i4", "v6", "v8"] {
        let cfg = engine_by_id(id).unwrap();
        let (frames, (lo, hi)) = cranking_inputs(&cfg, 1.5);
        let mut snd = EngineSound::new(
            &cfg,
            &Tuning::default(),
            &default_banks(&cfg),
            Quality::FAST,
        );
        let (mut dmin, mut dmax) = (f32::MAX, f32::MIN);
        for (f, inp) in frames.iter().enumerate() {
            snd.set_input(inp);
            for _ in 0..735 {
                snd.next_sample();
                if (54..84).contains(&f) {
                    let d = snd.crank_speed_deviation();
                    dmin = dmin.min(d);
                    dmax = dmax.max(d);
                }
            }
        }
        let mean = 0.5 * (lo + hi);
        let (sim_pp, snd_pp) = ((hi - lo) / mean, (dmax - dmin) as f64);
        println!(
            "{id}: speed ripple peak to peak, engine model {:.1} %, sound model {:.1} %",
            sim_pp * 100.0,
            snd_pp * 100.0
        );
        assert!(sim_pp > 0.01 && snd_pp > 0.01, "{id}: no ripple at all");
        let ratio = snd_pp / sim_pp;
        assert!(
            (0.5..2.0).contains(&ratio),
            "{id}: ripple off by {ratio:.2}x"
        );
    }
}

#[test]
fn cranking_is_a_rich_buzz_with_energy_through_the_kilohertz_range() {
    use engine_core::acoustics::analysis::spectrum;
    // the old starter was a swept sine: nothing between its two lines
    let cfg = engine_by_id("i4_16").unwrap();
    let (frames, _) = cranking_inputs(&cfg, 1.4);
    let mut snd = EngineSound::new(
        &cfg,
        &Tuning::default(),
        &default_banks(&cfg),
        Quality::FAST,
    );
    let mut x = Vec::new();
    for (f, inp) in frames.iter().enumerate() {
        snd.set_input(inp);
        for _ in 0..735 {
            let s = snd.next_sample();
            if f >= 36 {
                x.push(s);
            }
        }
    }
    assert!(x.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
    let psd = spectrum(&x, 16384);
    let (lo, hi) = (112usize, 1490usize);
    let n = (hi - lo) as f64;
    let geo = psd[lo..hi].iter().map(|p| p.max(1e-30).ln()).sum::<f64>() / n;
    let ari = psd[lo..hi].iter().sum::<f64>() / n;
    let flat = 10.0 * (geo.exp() / ari).log10();
    let level = rms_of(&x);
    println!("cranking: output rms {level:.3}, spectral flatness 300 Hz - 4 kHz {flat:.1} dB");
    assert!(level > 0.005, "the starter is inaudible ({level})");
    assert!(
        flat > -30.0,
        "cranking is a pure tone (flatness {flat:.1} dB)"
    );
}

#[test]
fn the_quality_ladder_gets_finer_and_stays_stable_at_every_tier() {
    let l = Quality::LADDER;
    for w in l.windows(2) {
        assert!(w[0].dx < w[1].dx, "the ladder must run from fine to coarse");
    }
    // every tier takes CFL-limited steps without blowing up, from the very first one, on an engine that fires at once
    let cfg = engine_by_id("v8").unwrap();
    for q in l {
        let mut snd = EngineSound::new(&cfg, &Tuning::default(), &default_banks(&cfg), q);
        let inp = input(&cfg, &Tuning::default(), 5500.0, 1.0);
        snd.set_input(&inp);
        for i in 0..22_050 {
            let s = snd.next_sample();
            assert!(
                s.is_finite() && s.abs() <= 1.0,
                "tier {:.0} mm: bad sample {s} at {i}",
                q.dx * 1e3
            );
        }
        let g = &snd.gas.exhaust.net.grid;
        for k in 0..g.m {
            assert!(
                g.p[k].is_finite() && g.p[k] > 5.0e3 && g.p[k] < 3.0e6,
                "tier {:.0} mm: pressure {} Pa in cell {k}",
                q.dx * 1e3,
                g.p[k]
            );
        }
    }
}

fn rms_pa(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

/// Level (dB, arbitrary reference) of the 1/3-octave-wide band around `c` Hz of `x`.
fn band_level(x: &[f32], lo: f64, hi: f64) -> f64 {
    use engine_core::acoustics::analysis::{band_power, spectrum};
    10.0 * band_power(&spectrum(x, 8192), 44_100.0, lo, hi)
        .max(1e-30)
        .log10()
}

#[test]
fn an_engine_that_is_switched_off_but_still_turning_is_nearly_silent() {
    // the key goes off in a car that rolls on in gear: the crank is still driven by the wheels at speed, nothing
    // fires, and what is heard is the engine turning over - far under what it made running
    for id in ["v8", "i4", "v12"] {
        let cfg = engine_by_id(id).unwrap();
        let mut inp = input(&cfg, &Tuning::default(), 3500.0, 0.6);
        let mut snd = EngineSound::new(
            &cfg,
            &Tuning::default(),
            &default_banks(&cfg),
            Quality::FAST,
        );
        snd.preheat(inp.egt_k * 0.9);
        snd.set_input(&inp);
        snd.soak(1.0, 60.0);
        let running: Vec<f32> = (0..22_050).map(|_| snd.next_sample()).collect();
        inp.running = false;
        inp.lit = false;
        inp.serial += 1;
        snd.set_input(&inp);
        let _ = (0..44_100).map(|_| snd.next_sample()).count();
        let off: Vec<f32> = (0..22_050).map(|_| snd.next_sample()).collect();
        let drop_db = 20.0 * (rms_pa(&off) / rms_pa(&running)).log10();
        println!("{id}: switched off at 3500 rpm, 1 s later {drop_db:.1} dB re running");
        assert!(
            drop_db < -30.0,
            "{id}: the engine is only {drop_db:.1} dB down a second after the key went off"
        );
    }
}

#[test]
fn the_tiers_of_the_solver_sound_alike() {
    // the same engine at the same speed on the finest tier and on a coarse one: the radiation from the tailpipes has
    // about the same level from the low end up to the body of the note (the coarse tier smooths the pulses and used
    // to lose 3-4 dB of low end and 8 dB of the body; the per-tier correction puts them back)
    for id in ["v8", "i6"] {
        let cfg = engine_by_id(id).unwrap();
        let inp = input(&cfg, &Tuning::default(), 3000.0, 0.8);
        let render = |q: Quality| -> Vec<f32> {
            let mut snd = EngineSound::new(&cfg, &Tuning::default(), &default_banks(&cfg), q);
            snd.preheat(inp.egt_k * 0.9);
            snd.set_input(&inp);
            snd.soak(1.0, 60.0);
            let mut sink = vec![0.0f32; 11_025];
            snd.render(&mut sink);
            (0..44_100)
                .map(|_| {
                    snd.next_sample();
                    snd.p_tail
                })
                .collect()
        };
        let fine = render(Quality::HIGH);
        for q in [Quality::BALANCED, Quality::FAST, Quality::ECO] {
            let coarse = render(q);
            for (lo, hi, name) in [
                (60.0, 150.0, "low end"),
                (150.0, 400.0, "firing note"),
                (400.0, 900.0, "body"),
            ] {
                let d = band_level(&coarse, lo, hi) - band_level(&fine, lo, hi);
                println!("{id} {:.0} mm vs 28 mm, {name}: {d:+.1} dB", q.dx * 1e3);
                assert!(
                    d.abs() < 4.0,
                    "{id} at {:.0} mm: the {name} is {d:+.1} dB off the finer tier",
                    q.dx * 1e3
                );
            }
        }
    }
}

#[test]
fn engines_with_a_high_firing_frequency_still_have_a_low_end() {
    // a fast six or twelve is one bright line and its overtones; the low orders of the crank bring the rumble back,
    // and every engine at speed keeps its band under 150 Hz within about 12 dB of the one above 260 Hz
    for id in ["v6", "i6", "v12"] {
        let cfg = engine_by_id(id).unwrap();
        for rpm in [2500.0, 4500.0] {
            let inp = input(&cfg, &Tuning::default(), rpm, 0.8);
            let mut snd = EngineSound::new(
                &cfg,
                &Tuning::default(),
                &default_banks(&cfg),
                Quality::FAST,
            );
            snd.preheat(inp.egt_k * 0.9);
            snd.set_input(&inp);
            snd.soak(1.5, 60.0);
            let mut sink = vec![0.0f32; 22_050];
            snd.render(&mut sink);
            let x: Vec<f32> = (0..88_200)
                .map(|_| {
                    snd.next_sample();
                    snd.p_tail + snd.p_jet + snd.p_shell + snd.p_mech + snd.p_intake + snd.p_low
                })
                .collect();
            let low = band_level(&x, 30.0, 150.0);
            let body = band_level(&x, 260.0, 1500.0);
            println!(
                "{id} {rpm:.0} rpm: 30-150 Hz is {:+.1} dB re 260-1500 Hz",
                low - body
            );
            assert!(
                low - body > -13.5,
                "{id} at {rpm:.0} rpm has no low end ({:+.1} dB)",
                low - body
            );
        }
    }
}

/// 99.9th percentile over the median of the 2 ms envelope of the second difference of `x` (a crude high-pass: what the
/// ear takes for crackle).
fn crackle_db(x: &[f32]) -> f64 {
    let hp: Vec<f32> = x.windows(3).map(|w| w[2] - 2.0 * w[1] + w[0]).collect();
    let mut env: Vec<f64> = hp
        .chunks(88)
        .map(|c| (c.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / c.len() as f64).sqrt())
        .collect();
    env.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| env[((env.len() - 1) as f64 * p) as usize];
    20.0 * (q(0.995) / q(0.5).max(1e-12)).log10()
}

fn overrun(cfg: &EngineConfig, tuning: &Tuning, secs: f32) -> Vec<f32> {
    let wot = input(cfg, tuning, 5500.0, 1.0);
    let off = input(cfg, tuning, 2500.0, 0.0);
    let mut snd = EngineSound::new(cfg, tuning, &default_banks(cfg), Quality::FAST);
    snd.preheat(wot.egt_k * 0.9);
    snd.set_input(&wot);
    snd.soak(1.0, 60.0);
    let mut out = Vec::new();
    for b in 0..(secs * 60.0) as usize {
        let t = b as f32 / 60.0;
        let mut cur = if t < 0.5 { wot.clone() } else { off.clone() };
        cur.burble = 1.0;
        if t >= 0.5 {
            cur.rpm = 5500.0 - 3000.0 * (1.0 - (-(t - 0.5) * 1.1).exp());
        }
        cur.serial = b as u64 + 2;
        snd.set_input(&cur);
        for _ in 0..735 {
            out.push(snd.next_sample());
        }
    }
    out
}

#[test]
fn burble_is_low_and_round_not_a_click() {
    // The pops are real blow-downs through the pipes: whatever is fitted, the high-passed envelope of an overrun must not
    // have the click train a separate HF layer once made (that sounded like a cap pistol; the 99.5th percentile over the
    // median of rally recordings is 17-32 dB, a steady engine is at 8-10, and the overrun here must stay under 18).
    let cfg = engine_by_id("v8").unwrap();
    for spec in ["", "cat=delete,muffler=delete"] {
        let x = overrun(&cfg, &Tuning::parse_spec(spec).unwrap(), 4.0);
        assert!(x.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        let c = crackle_db(&x[44_100..]);
        println!("overrun [{spec}]: crackle {c:.1} dB");
        assert!(
            c < 18.0,
            "[{spec}] the burble is a train of clicks: {c:.1} dB"
        );
    }
}

#[test]
fn a_running_engine_has_a_bounded_crank_speed_ripple() {
    // the within-cycle ripple and the slow wander of the combustion torque: there, and nowhere near the cranking surge
    for id in ["i4", "v8", "w16", "dieseli6"] {
        let cfg = engine_by_id(id).unwrap();
        let inp = input(
            &cfg,
            &Tuning::default(),
            (cfg.redline_rpm * 0.13).max(750.0),
            0.0,
        );
        let mut snd = EngineSound::new(
            &cfg,
            &Tuning::default(),
            &default_banks(&cfg),
            Quality::FAST,
        );
        snd.preheat(inp.egt_k * 0.9);
        snd.set_input(&inp);
        snd.soak(1.0, 60.0);
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for _ in 0..(3.0 * 44_100.0) as usize {
            snd.next_sample();
            let d = snd.crank_speed_deviation();
            lo = lo.min(d);
            hi = hi.max(d);
        }
        println!(
            "{id}: idle crank speed {:+.2} %..{:+.2} %",
            lo * 100.0,
            hi * 100.0
        );
        assert!(
            hi - lo > 0.003,
            "{id}: a dead-steady crank ({:.4})",
            hi - lo
        );
        assert!(
            hi - lo < 0.15 && hi < 0.12 && lo > -0.12,
            "{id}: ripple out of bounds {lo}..{hi}"
        );
    }
}
