use engine_core::*;

#[test]
fn all_engines_idle_and_rev() {
    for cfg in engines() {
        let id = cfg.id.clone();
        let mut e = build_default(cfg);
        e.engage_starter();
        let mut t = 0.0;
        while t < 3.0 {
            e.update(1.0 / 120.0);
            if e.telemetry().rpm > 600.0 {
                e.release_starter();
            }
            t += 1.0 / 120.0;
        }
        let tel = e.telemetry();
        println!(
            "{id}: idle rpm {:.0} running={} coolant {:.0}K",
            tel.rpm, tel.is_running, tel.coolant_temp_k
        );
        assert!(tel.is_running, "{id} stalled");
        assert!(
            (500.0..1400.0).contains(&tel.rpm),
            "{id} idle rpm {}",
            tel.rpm
        );
    }
}

#[test]
fn dyno_matches_reference_figures() {
    // Reference figures documented by the original TS sim's dyno pass.
    let cases = [("v8", 422.0, 5860.0)];
    for (id, ref_hp, ref_rpm) in cases {
        let mut e = build_default(engine_by_id(id).unwrap());
        e.warm_up();
        let (mut best_hp, mut best_rpm) = (0.0, 0.0);
        let mut rpm = 1500.0;
        while rpm <= e.config().redline_rpm {
            let tq = e.dyno_point(rpm, 0.4);
            let hp = tq * rpm * std::f64::consts::PI / 30.0 / 745.7;
            if hp > best_hp {
                best_hp = hp;
                best_rpm = rpm;
            }
            rpm += 250.0;
        }
        println!("{id}: peak {best_hp:.0} hp @ {best_rpm:.0} rpm (reference {ref_hp} @ {ref_rpm})");
        assert!(
            (best_hp - ref_hp).abs() / ref_hp < 0.06,
            "{id} peak hp {best_hp}"
        );
    }
}

#[test]
fn starts_with_fine_microsteps() {
    // Same stepping pattern as the viewer: 1/60 frames split into <=1.2 deg crank steps.
    let mut e = build_default(engine_by_id("v8").unwrap());
    let mut t = 0.0;
    for frame in 0..400 {
        e.engage_starter();
        let mut left = 1.0 / 60.0;
        while left > 1e-9 {
            let w_deg = (e.rpm() * 6.0).max(1.0);
            let h = (1.2 / w_deg).clamp(1.0 / 20_000.0, 1.0 / 240.0).min(left);
            e.update(h);
            left -= h;
        }
        t += 1.0 / 60.0;
        if frame % 40 == 0 {
            println!(
                "t={t:.2} rpm={:.0} running={}",
                e.rpm(),
                e.telemetry().is_running
            );
        }
        if e.rpm() > 650.0 {
            e.release_starter();
        }
    }
    assert!(e.telemetry().is_running);
}

#[test]
fn ecu_lights_cel_on_a_dead_cylinder_and_flags_open_o2() {
    use engine_core::CelState;
    let mut e = build_default(engine_by_id("i4").unwrap());
    e.engage_starter();
    let step = |e: &mut engine_core::CombustionEngine, secs: f64| {
        let mut t = 0.0;
        while t < secs {
            if e.rpm() > 650.0 {
                e.release_starter();
            }
            e.update(1.0 / 120.0);
            t += 1.0 / 120.0;
        }
    };
    step(&mut e, 12.0);
    assert!(e.telemetry().is_running);
    assert_eq!(
        e.telemetry().cel_state,
        CelState::Off,
        "healthy engine should not light the CEL"
    );
    // pull one coil connector: a real misfire the ECU can only detect via crank speed
    e.cylinder_ignition_mut(2).unwrap().connector.connected = false;
    let mut lit = false;
    for _ in 0..30 {
        step(&mut e, 1.0);
        if e.telemetry().cel_state != CelState::Off {
            lit = true;
            break;
        }
    }
    println!("CEL after dead cylinder: {:?}", e.telemetry().cel_state);
    assert!(lit, "ECU never noticed a dead cylinder");
    // clearing codes while still broken must not hide the fault for long
    e.clear_cel();
    // an unplugged O2 sensor is a direct circuit fault
    e.cylinder_ignition_mut(2).unwrap().connector.connected = true;
    e.exhaust.o2.connected = false;
    step(&mut e, 3.0);
    assert_ne!(
        e.telemetry().cel_state,
        CelState::Off,
        "open O2 circuit should set a fault"
    );
}

#[test]
fn evap_test_passes_when_sealed_and_fails_with_a_leak() {
    use engine_core::EvapMonitorStatus;
    for (leak, expect_pass) in [(0.0, true), (0.02, false), (0.15, false)] {
        let mut e = build_default(engine_by_id("i6").unwrap());
        e.evap.set_leak_fraction(leak);
        e.set_fuel_level(0.5); // the monitor only runs with a mid-range fuel level
        e.engage_starter();
        let mut result = EvapMonitorStatus::NotReady;
        let mut t = 0.0;
        while t < 90.0 {
            if e.rpm() > 650.0 {
                e.release_starter();
            }
            e.warm_up(); // closed loop (and so the EVAP monitor) needs a warm engine
            e.update(1.0 / 120.0);
            t += 1.0 / 120.0;
            let r = e.telemetry().evap_monitor_status;
            if matches!(
                r,
                EvapMonitorStatus::Pass
                    | EvapMonitorStatus::SmallLeak
                    | EvapMonitorStatus::LargeLeak
            ) {
                result = r;
                break;
            }
        }
        println!("leak {leak}: monitor concluded {result:?} at t={t:.0}s");
        assert_eq!(
            result == EvapMonitorStatus::Pass,
            expect_pass,
            "unexpected EVAP result {result:?}"
        );
    }
}

#[test]
fn healthy_engines_never_light_the_cel_at_idle_or_cruise() {
    use engine_core::CelState;
    for id in ["i4_16", "v8", "i6", "v6"] {
        let mut e = build_default(engine_by_id(id).unwrap());
        e.engage_starter();
        let mut t = 0.0f64;
        let mut first_lit: Option<f64> = None;
        while t < 60.0 {
            if e.rpm() > 650.0 {
                e.release_starter();
            }
            e.update(1.0 / 120.0);
            t += 1.0 / 120.0;
            if first_lit.is_none() && e.telemetry().cel_state != CelState::Off {
                first_lit = Some(t);
            }
        }
        println!("{id}: idle 60s -> first CEL at {first_lit:?}");
        assert!(
            first_lit.is_none(),
            "{id}: CEL lit at {first_lit:?} on a healthy idling engine"
        );
    }
}

#[test]
fn clutch_dump_and_wot_launch_does_not_look_like_a_misfire() {
    use engine_core::CelState;
    for id in ["i4_16", "v8", "i6"] {
        let mut e = build_default(engine_by_id(id).unwrap());
        let gears = engine_core::transmissions()
            .into_iter()
            .find(|t| t.id == "6speed")
            .unwrap()
            .gears;
        e.engage_starter();
        let mut t = 0.0f64;
        let mut engaged = false;
        let mut gear_idx = 1usize;
        let mut first_lit: Option<f64> = None;
        while t < 30.0 {
            if e.rpm() > 650.0 {
                e.release_starter();
            }
            if t > 4.0 && !engaged {
                engaged = true;
                gear_idx = 2;
                e.set_gear(gears[2].label, gears[2].ratio);
                e.set_throttle(1.0);
            }
            // simple auto-shift like the viewer's
            let tel = e.telemetry();
            if engaged
                && !tel.is_shifting
                && gear_idx + 1 < gears.len()
                && tel.rpm > tel.redline_rpm * 0.9
            {
                gear_idx += 1;
                e.set_gear(gears[gear_idx].label, gears[gear_idx].ratio);
            }
            e.update(1.0 / 120.0);
            t += 1.0 / 120.0;
            if first_lit.is_none() && e.telemetry().cel_state != CelState::Off {
                first_lit = Some(t);
                let tl = e.telemetry();
                println!("{id}: CEL monitors (misfire,o2,trim,evap) = {:?}, state {:?} rpm {:.0} gear {} knock {:.2} shifting {} revlim {} thr {:.2} boost {:.0} afr {:.1}", e.ecu_fault_flags(), tl.cel_state, tl.rpm, tl.gear_label, tl.knock_intensity, tl.is_shifting, tl.is_rev_limiting, tl.throttle, tl.boost_pressure_pa, tl.air_fuel_ratio);
            }
        }
        println!("{id}: launch -> first CEL at {first_lit:?}, final gear {gear_idx}");
        assert!(
            first_lit.is_none(),
            "{id}: CEL lit at {first_lit:?} during a normal WOT launch"
        );
    }
}

// ------------------------------------------------------------------ bolt-on tuning

fn peak(id: &str, tuning: &Tuning) -> (f64, f64) {
    let cfg = engine_by_id(id).unwrap();
    let fuel = fuels()
        .into_iter()
        .find(|f| f.ignition_type == cfg.ignition_type)
        .unwrap();
    let curve = dyno_curve(&cfg, &fuel, tuning, 250.0);
    let best = curve
        .iter()
        .max_by(|a, b| a.hp.partial_cmp(&b.hp).unwrap())
        .unwrap();
    (best.hp, best.rpm)
}

#[test]
fn stock_tuning_is_exactly_the_stock_engine() {
    // a default Tuning must not move any figure the reference dyno pins down
    for id in ["v8", "i4_16", "v12", "dieseli6"] {
        let cfg = engine_by_id(id).unwrap();
        let fuel = fuels()
            .into_iter()
            .find(|f| f.ignition_type == cfg.ignition_type)
            .unwrap();
        let a = dyno_curve(&cfg, &fuel, &Tuning::default(), 500.0);
        let mut e = build_default(cfg.clone());
        e.warm_up();
        for p in &a {
            let tq = e.dyno_point(p.rpm, 0.3);
            assert!(
                (tq - p.torque_nm).abs() < 1e-6,
                "{id} @ {}: {tq} vs {}",
                p.rpm,
                p.torque_nm
            );
        }
    }
}

#[test]
fn free_flowing_exhaust_and_intake_make_power() {
    let stock = peak("v8", &Tuning::default()).0;
    let mut t = Tuning::default();
    t.header = Header::LongTube;
    t.cat = Cat::HighFlow;
    t.muffler = Muffler::Straight;
    t.air_filter = AirFilter::ColdAir;
    let (hp, _) = peak("v8", &t);
    println!("v8 stock {stock:.0} hp, bolt-ons {hp:.0} hp");
    assert!(
        hp > stock * 1.02 && hp < stock * 1.15,
        "bolt-ons gave {hp} vs {stock}"
    );
}

#[test]
fn runner_length_moves_the_power_peak() {
    let (_, long_rpm) = peak(
        "v8",
        &Tuning {
            runners: Runners::LongRunner,
            ..Tuning::default()
        },
    );
    let (_, short_rpm) = peak(
        "v8",
        &Tuning {
            runners: Runners::ShortRunner,
            ..Tuning::default()
        },
    );
    println!("v8 long runners peak {long_rpm}, short {short_rpm}");
    assert!(
        short_rpm >= long_rpm,
        "short runners should peak no lower than long ones"
    );
}

#[test]
fn clogged_cat_costs_power() {
    let cfg = engine_by_id("v8").unwrap();
    let fuel = fuels()
        .into_iter()
        .find(|f| f.ignition_type == cfg.ignition_type)
        .unwrap();
    let mut e = CombustionEngine::new(cfg.clone(), &transmissions()[1], &brakes()[0], fuel);
    e.warm_up();
    let healthy = e.dyno_point(5500.0, 0.3);
    e.set_catalyst_clog(1.0);
    let clogged = e.dyno_point(5500.0, 0.3);
    println!("v8 @5500: healthy {healthy:.0} Nm, clogged cat {clogged:.0} Nm");
    assert!(clogged < healthy * 0.97);
}

#[test]
fn more_turbos_spool_faster_and_hold_boost_at_the_top() {
    let cfg = engine_by_id("v8").unwrap();
    let one = Tuning {
        turbo_count: Some(1),
        boost_bar: Some(1.0),
        ..Tuning::default()
    };
    let two = Tuning {
        turbo_count: Some(2),
        boost_bar: Some(1.0),
        ..Tuning::default()
    };
    let s1 = one.turbo_setup(&cfg).unwrap();
    let s2 = two.turbo_setup(&cfg).unwrap();
    assert!(s2.spool_tau_s < s1.spool_tau_s);
    assert!(s2.full_spool_rpm < s1.full_spool_rpm);
    assert!(s2.choke_rpm > s1.choke_rpm);
    let (hp1, _) = peak("v8", &one);
    let (hp2, _) = peak("v8", &two);
    let stock = peak("v8", &Tuning::default()).0;
    println!("v8: NA {stock:.0} hp, 1 turbo @1bar {hp1:.0} hp, 2 turbos {hp2:.0} hp");
    assert!(hp1 > stock * 1.2, "a turbo should add a lot of power");
    assert!(hp2 >= hp1, "a second turbo must not lose power");
}

#[test]
fn boost_slider_and_intercooler_do_what_they_say() {
    let low = Tuning {
        turbo_count: Some(1),
        boost_bar: Some(0.4),
        ..Tuning::default()
    };
    let high = Tuning {
        turbo_count: Some(2),
        boost_bar: Some(1.0),
        ..Tuning::default()
    };
    // keep the fuel out of the picture: premium has knock headroom for this test
    let cfg = engine_by_id("v8").unwrap();
    let fuel = fuels().into_iter().find(|f| f.id == "race100").unwrap();
    let pk = |t: &Tuning| {
        dyno_curve(&cfg, &fuel, t, 500.0)
            .iter()
            .map(|p| p.hp)
            .fold(0.0, f64::max)
    };
    assert!(pk(&high) > pk(&low));
    let hot = Tuning {
        intercooler: Intercooler::None,
        ..high.clone()
    };
    let cool = Tuning {
        intercooler: Intercooler::Large,
        ..high.clone()
    };
    assert!(
        pk(&cool) > pk(&hot) * 1.05,
        "intercooler should matter: {} vs {}",
        pk(&cool),
        pk(&hot)
    );
}

#[test]
fn no_engine_ships_with_a_turbo_but_every_one_can_take_one() {
    for cfg in engines() {
        let id = &cfg.id;
        assert!(
            cfg.turbo.is_none(),
            "{id}: turbos are fitted in the tuning panel, not baked into an engine"
        );
        assert!(Tuning::default().turbo_setup(&cfg).is_none(), "{id}");
        let kit = Tuning {
            turbo_count: Some(1),
            ..Tuning::default()
        };
        let setup = kit.turbo_setup(&cfg).unwrap();
        assert_eq!(setup.count, 1, "{id}");
        assert!(
            setup.max_boost_pa > 0.0 && setup.full_spool_rpm < cfg.redline_rpm,
            "{id}"
        );
    }
}

#[test]
fn modded_engines_still_idle_and_start() {
    let t = Tuning {
        air_filter: AirFilter::OpenStack,
        runners: Runners::Itb,
        header: Header::LongTube,
        cat: Cat::Delete,
        muffler: Muffler::Delete,
        pipe_scale: 1.3,
        turbo_count: Some(4),
        boost_bar: Some(1.5),
        ..Tuning::default()
    };
    for id in ["v8", "i4", "w16", "dieseli6"] {
        let mut e = build_default(engine_by_id(id).unwrap());
        e.set_tuning(t.clone());
        e.engage_starter();
        let mut s = 0.0;
        while s < 4.0 {
            e.update(1.0 / 120.0);
            if e.telemetry().rpm > 600.0 {
                e.release_starter();
            }
            s += 1.0 / 120.0;
        }
        let tel = e.telemetry();
        println!("{id}: modded idle {:.0} rpm", tel.rpm);
        assert!(
            tel.is_running && (400.0..1600.0).contains(&tel.rpm),
            "{id} idle {}",
            tel.rpm
        );
    }
}

#[test]
fn throttle_blips_and_lifts_do_not_light_the_cel() {
    use engine_core::CelState;
    for id in [
        "i4", "i4_25", "v6", "v8", "i6", "v12", "vtwin", "v16", "w16",
    ] {
        let mut e = build_default(engine_by_id(id).unwrap());
        e.engage_starter();
        let mut t = 0.0f64;
        let mut first_lit: Option<f64> = None;
        while t < 80.0 {
            if e.rpm() > 650.0 {
                e.release_starter();
            }
            // blip / lift pattern at neutral
            let ph = (t % 6.0) / 6.0;
            if t > 8.0 {
                e.set_throttle(if ph < 0.3 {
                    0.0
                } else if ph < 0.5 {
                    1.0
                } else if ph < 0.6 {
                    0.0
                } else {
                    0.4
                });
            }
            e.update(1.0 / 120.0);
            t += 1.0 / 120.0;
            if first_lit.is_none() && e.telemetry().cel_state != CelState::Off {
                first_lit = Some(t);
                let tl = e.telemetry();
                println!(
                    "{id}: t={t:.1} monitors {:?} rpm {:.0} thr {:.2} afr {:.1}",
                    e.ecu_fault_flags(),
                    tl.rpm,
                    tl.throttle,
                    tl.air_fuel_ratio
                );
            }
        }
        println!("{id}: blips -> first CEL {first_lit:?}");
        assert!(
            first_lit.is_none(),
            "{id}: CEL lit at {first_lit:?} from throttle blips on a healthy engine"
        );
    }
}
