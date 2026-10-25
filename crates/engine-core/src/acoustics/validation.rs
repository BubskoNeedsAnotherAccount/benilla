//! Physics validation of the acoustic solver against closed-form acoustics: pipe resonances with end
//! corrections, Helmholtz resonators, area-change reflection, expansion-chamber transmission loss,
//! shock tubes and numerical damping.

use super::analysis::*;
use super::*;

const P_AMB: f32 = 101_325.0;
const FS: f32 = 44_100.0;

fn spec(name: &str, len: f32, dia: f32, dx: f32) -> DuctSpec {
    let wall = Wall::pipe(PipeMaterial::MildSteel, 0.0015, false, dia);
    DuctSpec::pipe(name, len, dia, wall, dx)
}

fn net_for(gas: Gas, t_amb: f32) -> Network {
    Network::new(Env::new(P_AMB, t_amb), gas)
}

/// Raises the pressure of cells `i0..i1` of a duct by `amp` Pa, isentropically (a pulse at rest).
fn bump(net: &mut Network, d: usize, i0: usize, i1: usize, amp: f32) {
    net.grid.update_primitives();
    for i in i0..i1 {
        let (p0, rho0, t0) = (net.grid.p_at(d, i), net.grid.rho_at(d, i), net.grid.t_at(d, i));
        let p = p0 + amp;
        let rho = rho0 * (p / p0).powf(1.0 / net.grid.gas.gamma(t0));
        net.grid.set_cell(d, i, rho, 0.0, p);
    }
    net.grid.update_primitives();
}

/// A right-going simple wave: raised-cosine pulse of `amp` Pa, `half` cells either side of cell `c`.
fn right_pulse(net: &mut Network, d: usize, c: usize, half: usize, amp: f32) {
    net.grid.update_primitives();
    for k in 0..=half {
        let a = amp * 0.5 * (1.0 + (std::f32::consts::PI * k as f32 / (half as f32 + 1.0)).cos());
        for i in [c + k, c.saturating_sub(k)] {
            let (p0, rho0, t0) = (net.grid.p_at(d, i), net.grid.rho_at(d, i), net.grid.t_at(d, i));
            let g = net.grid.gas;
            let p = p0 + a;
            let rho = rho0 * (p / p0).powf(1.0 / g.gamma(t0));
            let u = a / (rho0 * g.sound_speed(t0));
            net.grid.set_cell(d, i, rho, u, p);
        }
    }
    net.grid.update_primitives();
}

fn run(net: &mut Network, steps: usize, probe: impl FnMut(&Network) -> f32) -> Vec<f32> {
    run_res(net, &mut [], steps, probe)
}

fn run_res(net: &mut Network, res: &mut [Reservoir], steps: usize, mut probe: impl FnMut(&Network) -> f32) -> Vec<f32> {
    let dt = 1.0 / FS;
    let mut out = Vec::with_capacity(steps);
    for _ in 0..steps {
        net.step(dt, res);
        for r in res.iter_mut() {
            r.commit();
        }
        out.push(probe(net));
    }
    out
}

fn closest(peaks: &[(f64, f64)], f: f64) -> f64 {
    peaks.iter().map(|p| p.0).min_by(|a, b| (a - f).abs().partial_cmp(&(b - f).abs()).unwrap()).unwrap_or(0.0)
}

// ------------------------------------------------------------------ the numerics themselves

#[test]
fn uniform_gas_at_rest_stays_at_rest_even_in_a_cone() {
    let mut net = net_for(Gas::AIR, 293.0);
    let wall = Wall::pipe(PipeMaterial::MildSteel, 0.0015, false, 0.05);
    let d = net.add_duct(&DuctSpec::pipe("cone", 1.0, 0.03, wall, 0.025).taper(0.09), P_AMB, 293.0);
    net.add_node(Node::closed("l", PipeLeg::new(d, End::Left)));
    net.add_node(Node::closed("r", PipeLeg::new(d, End::Right)));
    run(&mut net, 4000, |_| 0.0);
    let worst = (0..net.grid.n(d)).fold(0.0f32, |a, i| a.max(net.grid.u_at(d, i).abs()));
    let dp = (0..net.grid.n(d)).fold(0.0f32, |a, i| a.max((net.grid.p_at(d, i) - P_AMB).abs()));
    assert!(worst < 0.05, "spurious velocity {worst} m/s");
    assert!(dp < 20.0, "spurious pressure {dp} Pa");
}

#[test]
fn closed_network_conserves_mass_and_energy() {
    let mut net = net_for(Gas::AIR, 293.0);
    let a = net.add_duct(&spec("a", 0.6, 0.05, 0.02), P_AMB, 293.0);
    let b = net.add_duct(&spec("b", 0.6, 0.08, 0.02), P_AMB, 293.0);
    net.grid.dissipation = false;
    net.add_node(Node::closed("l", PipeLeg::new(a, End::Left)));
    net.add_node(Node::junction("j").with_leg(PipeLeg::new(a, End::Right)).with_leg(PipeLeg::new(b, End::Left)));
    net.add_node(Node::closed("r", PipeLeg::new(b, End::Right)));
    bump(&mut net, a, 0, 8, 30_000.0);
    let e0: f32 = (0..2).map(|d| net.grid.energy(d)).sum();
    let m0 = net.mass();
    run(&mut net, 6000, |_| 0.0);
    let e1: f32 = (0..2).map(|d| net.grid.energy(d)).sum();
    assert!((net.mass() / m0 - 1.0).abs() < 2e-4, "mass drift {}", net.mass() / m0 - 1.0);
    assert!((e1 / e0 - 1.0).abs() < 2e-3, "energy drift {}", e1 / e0 - 1.0);
    assert_eq!(net.fallbacks(), 0);
}

#[test]
fn small_pulse_travels_at_the_speed_of_sound() {
    for (gas, t) in [(Gas::AIR, 293.0f32), (Gas::EXHAUST, 500.0), (Gas::EXHAUST, 900.0)] {
        let mut net = net_for(gas, 293.0);
        let d = net.add_duct(&spec("p", 6.0, 0.05, 0.02), P_AMB, t);
        net.grid.dissipation = false;
        let ar = net.grid.end_area(d, End::Left);
        net.add_node(Node::open("l", PipeLeg::new(d, End::Left), OpenEnd::anechoic(ar)));
        net.add_node(Node::open("r", PipeLeg::new(d, End::Right), OpenEnd::anechoic(ar)));
        right_pulse(&mut net, d, 40, 20, 150.0);
        let probe = 200usize;
        let (mut tt, mut best, mut best_t) = (0.0f32, 0.0f32, 0.0f32);
        let dt = 1.0 / FS;
        for _ in 0..(0.012 * FS) as usize {
            net.step(dt, &mut []);
            tt += dt;
            let pp = net.grid.p_at(d, probe) - P_AMB;
            if pp > best {
                best = pp;
                best_t = tt;
            }
        }
        let c = gas.sound_speed(t);
        let meas = (probe as f32 - 40.0) * 0.02 / best_t;
        assert!((meas / c - 1.0).abs() < 0.01, "T={t}: measured {meas} vs {c}");
        assert!(best > 120.0, "pulse smeared away: {best} Pa");
    }
}

#[test]
fn sod_shock_tube_has_the_right_plateau_and_shock_position() {
    let mut net = net_for(Gas::AIR, 300.0);
    let d = net.add_duct(&spec("sod", 1.0, 0.05, 0.005), P_AMB, 300.0);
    net.grid.dissipation = false;
    net.add_node(Node::closed("l", PipeLeg::new(d, End::Left)));
    net.add_node(Node::closed("r", PipeLeg::new(d, End::Right)));
    for i in 0..net.grid.n(d) {
        let (p, rho) = if i < net.grid.n(d) / 2 { (1.0e5, 1.0) } else { (1.0e4, 0.125) };
        net.grid.set_cell(d, i, rho, 0.0, p);
    }
    net.grid.update_primitives();
    let dt = 4.0e-6;
    for _ in 0..150 {
        net.step(dt, &mut []);
    }
    // exact solution (gamma 1.4): plateau between rarefaction tail (x = 0.487) and contact (0.676)
    // has p* = 30 313 Pa and u* = 293 m/s; the caloric law differs a little from gamma = 1.4
    let i = ((0.5 + 0.12) / net.grid.dx(d)) as usize;
    assert!((net.grid.p_at(d, i) / 3.03e4 - 1.0).abs() < 0.06, "plateau pressure {}", net.grid.p_at(d, i));
    assert!((net.grid.u_at(d, i) / 293.0 - 1.0).abs() < 0.08, "plateau velocity {}", net.grid.u_at(d, i));
    let shock = (0..net.grid.n(d)).rev().find(|&k| net.grid.p_at(d, k) > 2.0e4).unwrap() as f32 * net.grid.dx(d);
    assert!((shock - 0.832).abs() < 0.03, "shock at {shock}");
}

#[test]
fn travelling_waves_keep_their_amplitude_at_audio_resolution() {
    // a 4-cycle packet through 6 m of pipe at 21 and 43 cells per wavelength
    for (f, max_loss_db) in [(400.0f32, 0.5f32), (800.0, 2.0)] {
        let mut net = net_for(Gas::AIR, 293.0);
        let dx = 0.02f32;
        let d = net.add_duct(&spec("p", 16.0, 0.05, dx), P_AMB, 293.0);
        net.grid.dissipation = false;
        let ar = net.grid.end_area(d, End::Left);
        net.add_node(Node::open("l", PipeLeg::new(d, End::Left), OpenEnd::anechoic(ar)));
        net.add_node(Node::open("r", PipeLeg::new(d, End::Right), OpenEnd::anechoic(ar)));
        let c = Gas::AIR.sound_speed(293.0);
        let lam = c / f;
        let n_cells = ((4.0 * lam) / dx) as usize;
        net.grid.update_primitives();
        let c0 = ((0.5 + 4.0 * lam) / dx) as usize;
        for i in 0..n_cells {
            let idx = c0 + i - n_cells;
            let x = i as f32 / n_cells as f32;
            let env = 0.5 - 0.5 * (std::f32::consts::TAU * x).cos();
            let a_p = 100.0 * env * (std::f32::consts::TAU * 4.0 * x).sin();
            let (p0, rho0) = (net.grid.p_at(d, idx), net.grid.rho_at(d, idx));
            let p = p0 + a_p;
            net.grid.set_cell(d, idx, rho0 * (p / p0).powf(1.0 / 1.4), a_p / (rho0 * c), p);
        }
        net.grid.update_primitives();
        let (near, far) = ((6.0 / dx) as usize, (12.0 / dx) as usize);
        let (mut pk_near, mut pk_far) = (0.0f32, 0.0f32);
        for _ in 0..(0.04 * FS) as usize {
            net.step(1.0 / FS, &mut []);
            pk_near = pk_near.max((net.grid.p_at(d, near) - P_AMB).abs());
            pk_far = pk_far.max((net.grid.p_at(d, far) - P_AMB).abs());
        }
        let loss = -20.0 * (pk_far / pk_near).log10();
        println!("{f:.0} Hz ({:.0} cells/wave): {loss:.2} dB lost over 6 m", lam / dx);
        assert!(loss < max_loss_db, "{f} Hz packet lost {loss} dB");
    }
}

// ------------------------------------------------------------------ resonances

#[test]
fn closed_open_pipe_rings_at_odd_quarter_waves() {
    let (l, dia) = (1.2f32, 0.05f32);
    let mut net = net_for(Gas::AIR, 293.0);
    let d = net.add_duct(&spec("p", l, dia, 0.02), P_AMB, 293.0);
    net.add_node(Node::closed("closed", PipeLeg::new(d, End::Left)));
    let area = net.grid.end_area(d, End::Right);
    net.add_node(Node::open("mouth", PipeLeg::new(d, End::Right), OpenEnd::radiating(area)));
    bump(&mut net, d, 0, 3, 1500.0);
    let sig = run(&mut net, 32_000, |n| n.grid.p_at(0, 0) - P_AMB);
    let psd = spectrum(&sig[2000..], 16384);
    let pk = peaks(&psd, FS as f64, 30.0, 700.0, 12.0);
    let c = Gas::AIR.sound_speed(293.0) as f64;
    let delta = 0.6133 * dia as f64 / 2.0;
    for k in 1..=4 {
        let want = (2 * k - 1) as f64 * c / (4.0 * (l as f64 + delta));
        let got = closest(&pk, want);
        println!("closed-open mode {k}: want {want:.1} Hz, got {got:.1} Hz");
        assert!((got / want - 1.0).abs() < 0.015, "mode {k}: want {want:.1} got {got:.1}");
    }
}

#[test]
fn open_open_pipe_rings_at_half_waves_with_both_end_corrections() {
    let (l, dia) = (1.0f32, 0.05f32);
    let mut net = net_for(Gas::AIR, 293.0);
    let d = net.add_duct(&spec("p", l, dia, 0.02), P_AMB, 293.0);
    let area = net.grid.end_area(d, End::Right);
    net.add_node(Node::open("a", PipeLeg::new(d, End::Left), OpenEnd::radiating(area)));
    net.add_node(Node::open("b", PipeLeg::new(d, End::Right), OpenEnd::radiating(area)));
    let off = (0.3 * l / net.grid.dx(d)) as usize;
    bump(&mut net, d, off, off + 3, 1500.0);
    let sig = run(&mut net, 32_000, move |n| n.grid.p_at(0, off + 6) - P_AMB);
    let psd = spectrum(&sig[1500..], 16384);
    let pk = peaks(&psd, FS as f64, 60.0, 900.0, 10.0);
    let c = Gas::AIR.sound_speed(293.0) as f64;
    let leff = l as f64 + 2.0 * 0.6133 * dia as f64 / 2.0;
    for k in 1..=4 {
        let want = k as f64 * c / (2.0 * leff);
        let got = closest(&pk, want);
        println!("open-open mode {k}: want {want:.1} Hz, got {got:.1} Hz");
        assert!((got / want - 1.0).abs() < 0.015, "mode {k}: want {want:.1} got {got:.1}");
    }
}

#[test]
fn hot_gas_raises_every_resonance_with_the_speed_of_sound() {
    let (l, dia) = (1.0f32, 0.05f32);
    for t in [293.0f32, 500.0, 900.0] {
        // Ambient at the pipe temperature: the textbook end correction applies. (With cold air outside a
        // hot pipe the slug is denser than the gas it pushes, which lengthens the pipe acoustically -
        // the solver reproduces that too, see `cold_air_outside_a_hot_pipe_lowers_its_resonance`.)
        let mut net = net_for(Gas::EXHAUST, t);
        net.env.gas = Gas::EXHAUST;
        net.env.rho_amb = P_AMB / (Gas::EXHAUST.r * t);
        net.env.c_amb = Gas::EXHAUST.sound_speed(t);
        let d = net.add_duct(&spec("p", l, dia, 0.02), P_AMB, t);
        net.grid.dissipation = false;
        net.add_node(Node::closed("closed", PipeLeg::new(d, End::Left)));
        let area = net.grid.end_area(d, End::Right);
        net.add_node(Node::open("mouth", PipeLeg::new(d, End::Right), OpenEnd::radiating(area)));
        bump(&mut net, d, 0, 3, 1500.0);
        let sig = run(&mut net, 32_000, |n| n.grid.p_at(0, 0) - P_AMB);
        let psd = spectrum(&sig[2000..], 16384);
        let f1 = strongest_peak(&psd, FS as f64, 30.0, 400.0).unwrap();
        let c = Gas::EXHAUST.sound_speed(t) as f64;
        let want = c / (4.0 * (l as f64 + 0.6133 * dia as f64 / 2.0));
        println!("T = {t} K: c = {c:.0} m/s, fundamental {f1:.1} Hz (want {want:.1})");
        assert!((f1 / want - 1.0).abs() < 0.015, "T={t}: {f1} vs {want}");
    }
}

#[test]
fn cold_air_outside_a_hot_pipe_lowers_its_resonance() {
    // same hot pipe, once with ambient air at the pipe temperature and once with 293 K air outside
    let f1_for = |ambient_hot: bool| {
        let t = 700.0;
        let mut net = net_for(Gas::EXHAUST, if ambient_hot { t } else { 293.0 });
        if ambient_hot {
            net.env.gas = Gas::EXHAUST;
            net.env.rho_amb = P_AMB / (Gas::EXHAUST.r * t);
            net.env.c_amb = Gas::EXHAUST.sound_speed(t);
        }
        let d = net.add_duct(&spec("p", 1.0, 0.05, 0.02), P_AMB, t);
        net.grid.dissipation = false;
        net.add_node(Node::closed("closed", PipeLeg::new(d, End::Left)));
        let area = net.grid.end_area(d, End::Right);
        net.add_node(Node::open("mouth", PipeLeg::new(d, End::Right), OpenEnd::radiating(area)));
        bump(&mut net, d, 0, 3, 300.0);
        let sig = run(&mut net, 32_000, |n| n.grid.p_at(0, 0) - P_AMB);
        strongest_peak(&spectrum(&sig[2000..], 16384), FS as f64, 30.0, 400.0).unwrap()
    };
    let (hot, cold) = (f1_for(true), f1_for(false));
    println!("hot-pipe fundamental: {hot:.1} Hz with hot ambient, {cold:.1} Hz with 293 K air outside");
    assert!(cold < hot * 0.99, "cold outside air must lengthen the pipe acoustically");
    assert!(cold > hot * 0.92);
}

#[test]
fn helmholtz_resonator_between_two_volumes_rings_at_its_formula() {
    // a stub closed at the far end with a resonator on it. The neck's mass swings between the stub's
    // compliance and the chamber's, which are in series: w^2 = c^2 A / L_eff (1/V_stub + 1/V).
    let gas = Gas::AIR;
    let mut net = net_for(gas, 293.0);
    let d = net.add_duct(&spec("stub", 0.06, 0.1, 0.02), P_AMB, 293.0);
    net.grid.dissipation = false;
    net.add_node(Node::closed("c0", PipeLeg::new(d, End::Left)));
    let (a_n, l_eff, v) = (1.5e-4f32, 0.05f32, 4.0e-3f32);
    let mut neck = SlugLeg::new(0, a_n, l_eff);
    neck.k = 0.0;
    net.add_node(Node::junction("j").with_leg(PipeLeg::new(d, End::Right)).with_slug(neck));
    let mut res = vec![Reservoir::new(gas, v, P_AMB, 293.0)];
    let c = gas.sound_speed(293.0);
    let v_pipe = net.grid.volume(d);
    let want = (c / std::f32::consts::TAU * (a_n / l_eff * (1.0 / v + 1.0 / v_pipe)).sqrt()) as f64;
    bump(&mut net, d, 0, 3, 3000.0);
    let sig = run_res(&mut net, &mut res, 44_100, |n| n.grid.p_at(0, 1) - P_AMB);
    let psd = spectrum(&sig[1000..], 32768);
    let f = strongest_peak(&psd, FS as f64, 40.0, 1200.0).unwrap();
    println!("Helmholtz (series volumes): formula {want:.1} Hz, simulated {f:.1} Hz");
    assert!((f / want - 1.0).abs() < 0.02, "{f} vs {want}");
}

/// Transmission |P_t|^2/|P_i|^2 of a pulse through a network built by `build`, which returns the
/// (inlet duct, outlet duct, reservoirs). Anechoic ends on both sides.
pub(super) fn transmission(build: impl FnOnce(&mut Network) -> (usize, usize, Vec<Reservoir>), t_end: f32, window_s: f32, n: usize) -> Vec<(f64, f64)> {
    let mut net = net_for(Gas::AIR, 293.0);
    let (a, b, mut res) = build(&mut net);
    let c0 = (0.8 / net.grid.dx(a)) as usize;
    right_pulse(&mut net, a, c0, 8, 200.0);
    let probe_a = (1.6 / net.grid.dx(a)) as usize;
    let probe_b = (1.0 / net.grid.dx(b)) as usize;
    let dt = 1.0 / FS;
    let (mut pin, mut pout) = (Vec::new(), Vec::new());
    for s in 0..(t_end * FS) as usize {
        net.step(dt, &mut res);
        for r in res.iter_mut() {
            r.commit();
        }
        let t = s as f32 * dt;
        pin.push(if t < window_s { net.grid.p_at(a, probe_a) - P_AMB } else { 0.0 });
        pout.push(net.grid.p_at(b, probe_b) - P_AMB);
    }
    let spec_of = |x: &[f32]| {
        let mut re: Vec<f64> = (0..n).map(|i| x.get(i).copied().unwrap_or(0.0) as f64).collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);
        (0..n / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect::<Vec<f64>>()
    };
    let (si, so) = (spec_of(&pin), spec_of(&pout));
    let df = FS as f64 / n as f64;
    (1..n / 2).map(|k| (k as f64 * df, so[k] / si[k].max(1e-30))).collect()
}

#[test]
fn side_branch_resonator_notches_the_transmission_at_its_helmholtz_frequency() {
    let gas = Gas::AIR;
    let (a_n, l_eff, v) = (5.0e-4f32, 0.04f32, 3.0e-3f32);
    let f_h = SlugLeg::new(0, a_n, l_eff).resonance_hz(gas.sound_speed(293.0), v) as f64;
    let curve = transmission(
        |net| {
            let a = net.add_duct(&spec("in", 3.0, 0.05, 0.02), P_AMB, 293.0);
            let b = net.add_duct(&spec("out", 3.0, 0.05, 0.02), P_AMB, 293.0);
            net.grid.dissipation = false;
            let (ar1, ar2) = (net.grid.end_area(a, End::Left), net.grid.end_area(b, End::Right));
            net.add_node(Node::open("src", PipeLeg::new(a, End::Left), OpenEnd::anechoic(ar1)));
            let mut neck = SlugLeg::new(0, a_n, l_eff);
            neck.k = 0.0;
            net.add_node(Node::junction("j").with_leg(PipeLeg::new(a, End::Right)).with_leg(PipeLeg::new(b, End::Left)).with_slug(neck));
            net.add_node(Node::open("end", PipeLeg::new(b, End::Right), OpenEnd::anechoic(ar2)));
            (a, b, vec![Reservoir::new(gas, v, P_AMB, 293.0)])
        },
        0.12,
        0.0075,
        16384,
    );
    let (mut best_f, mut best) = (0.0, f64::MAX);
    for (f, r) in &curve {
        if *f > 0.6 * f_h && *f < 1.6 * f_h && *r < best {
            best = *r;
            best_f = *f;
        }
    }
    let at = |f0: f64| curve.iter().min_by(|a, b| (a.0 - f0).abs().partial_cmp(&(b.0 - f0).abs()).unwrap()).unwrap().1;
    println!("side branch: Helmholtz {f_h:.1} Hz, deepest notch {best_f:.1} Hz ({:.1} dB), at 2 f_H {:.1} dB", 10.0 * best.log10(), 10.0 * at(2.0 * f_h).log10());
    assert!((best_f / f_h - 1.0).abs() < 0.04, "notch at {best_f} vs {f_h}");
    assert!(10.0 * best.log10() < -15.0, "notch too shallow: {} dB", 10.0 * best.log10());
    assert!(10.0 * at(2.0 * f_h).log10() > -4.0, "far from resonance sound should pass");
}

// ------------------------------------------------------------------ area changes and chambers

/// Two ducts joined by a node; anechoic far ends. Returns (incident, reflected, transmitted) peak pressures.
fn step_scattering(d1: f32, d2: f32, k_in1: f32, k_out2: f32) -> (f32, f32, f32) {
    let mut net = net_for(Gas::AIR, 293.0);
    let a = net.add_duct(&spec("in", 3.0, d1, 0.02), P_AMB, 293.0);
    let b = net.add_duct(&spec("out", 3.0, d2, 0.02), P_AMB, 293.0);
    net.grid.dissipation = false;
    let (ar1, ar2) = (net.grid.end_area(a, End::Left), net.grid.end_area(b, End::Right));
    net.add_node(Node::open("src", PipeLeg::new(a, End::Left), OpenEnd::anechoic(ar1)));
    net.add_node(Node::junction("step").with_leg(PipeLeg::new(a, End::Right).with_loss(k_in1, 0.0)).with_leg(PipeLeg::new(b, End::Left).with_loss(0.0, k_out2)));
    net.add_node(Node::open("end", PipeLeg::new(b, End::Right), OpenEnd::anechoic(ar2)));
    right_pulse(&mut net, a, (0.8 / 0.02) as usize, 20, 150.0);
    let (probe_a, probe_b) = ((1.5 / 0.02) as usize, (1.0 / 0.02) as usize);
    let (mut inc, mut refl, mut trans) = (0.0f32, 0.0f32, 0.0f32);
    let dt = 1.0 / FS;
    for s in 0..(0.02 * FS) as usize {
        net.step(dt, &mut []);
        let t = s as f32 * dt;
        let pa = net.grid.p_at(a, probe_a) - P_AMB;
        if t < 0.006 {
            inc = inc.max(pa);
        } else {
            refl = if pa.abs() > refl.abs() { pa } else { refl };
        }
        trans = trans.max(net.grid.p_at(b, probe_b) - P_AMB);
    }
    (inc, refl, trans)
}

#[test]
fn area_step_reflects_and_transmits_like_plane_wave_theory() {
    for (d1, d2) in [(0.04f32, 0.08f32), (0.08, 0.04), (0.05, 0.05)] {
        let (a1, a2) = (d1 * d1, d2 * d2);
        let (inc, refl, trans) = step_scattering(d1, d2, 0.0, 0.0);
        let r_theory = (a1 - a2) / (a1 + a2);
        let t_theory = 2.0 * a1 / (a1 + a2);
        println!("A2/A1 = {:.1}: R {:.3} (theory {r_theory:.3}), T {:.3} (theory {t_theory:.3})", a2 / a1, refl / inc, trans / inc);
        assert!((refl / inc - r_theory).abs() < 0.02, "R {} vs {}", refl / inc, r_theory);
        assert!((trans / inc - t_theory).abs() < 0.03, "T {} vs {}", trans / inc, t_theory);
    }
}

#[test]
fn sudden_expansion_loss_coefficient_reduces_transmission() {
    let (_, _, t0) = step_scattering(0.04, 0.08, 0.0, 0.0);
    let (_, _, t1) = step_scattering(0.04, 0.08, 40.0, 0.0);
    assert!(t1 < t0, "{t1} vs {t0}");
}

#[test]
fn expansion_chamber_transmission_loss_matches_the_plane_wave_formula() {
    let (dp, dc, lc) = (0.05f32, 0.15f32, 0.3f32);
    let curve = transmission(
        |net| {
            let a = net.add_duct(&spec("in", 3.0, dp, 0.02), P_AMB, 293.0);
            let m = net.add_duct(&spec("chamber", lc, dc, 0.02), P_AMB, 293.0);
            let b = net.add_duct(&spec("out", 3.0, dp, 0.02), P_AMB, 293.0);
            net.grid.dissipation = false;
            let (ar1, ar2) = (net.grid.end_area(a, End::Left), net.grid.end_area(b, End::Right));
            net.add_node(Node::open("src", PipeLeg::new(a, End::Left), OpenEnd::anechoic(ar1)));
            net.add_node(Node::junction("j1").with_leg(PipeLeg::new(a, End::Right)).with_leg(PipeLeg::new(m, End::Left)));
            net.add_node(Node::junction("j2").with_leg(PipeLeg::new(m, End::Right)).with_leg(PipeLeg::new(b, End::Left)));
            net.add_node(Node::open("end", PipeLeg::new(b, End::Right), OpenEnd::anechoic(ar2)));
            (a, b, vec![])
        },
        0.05,
        0.0085,
        8192,
    );
    let m = (dc as f64 / dp as f64).powi(2);
    let c = Gas::AIR.sound_speed(293.0) as f64;
    let tl_theory = |f: f64| {
        let kl = std::f64::consts::TAU * f / c * lc as f64;
        10.0 * (1.0 + 0.25 * (m - 1.0 / m).powi(2) * kl.sin().powi(2)).log10()
    };
    let mut worst = 0.0f64;
    for (f, ratio) in curve.iter().filter(|(f, _)| (120.0..1000.0).contains(f)) {
        worst = worst.max((-10.0 * ratio.log10() - tl_theory(*f)).abs());
    }
    let at = |f0: f64| curve.iter().min_by(|a, b| (a.0 - f0).abs().partial_cmp(&(b.0 - f0).abs()).unwrap()).unwrap();
    let f_peak = c / (4.0 * lc as f64);
    let (_, r_peak) = at(f_peak);
    println!("expansion chamber: TL at {f_peak:.0} Hz = {:.1} dB (theory {:.1}); worst deviation {worst:.2} dB", -10.0 * r_peak.log10(), tl_theory(f_peak));
    assert!((-10.0 * r_peak.log10() - tl_theory(f_peak)).abs() < 1.0);
    // the null is flat (TL ~ (k dL)^2): look for the transparent band near the half-wave frequency
    let f_null = c / (2.0 * lc as f64);
    let best = curve.iter().filter(|(f, _)| (*f - f_null).abs() < 0.12 * f_null).map(|(f, r)| (*f, -10.0 * r.log10())).fold((0.0, f64::MAX), |a, b| if b.1 < a.1 { b } else { a });
    println!("most transparent point near the null: {:.0} Hz, TL {:.2} dB", best.0, best.1);
    assert!(best.1 < 1.0, "chamber must be transparent near the half-wave frequency");
    assert!((best.0 / f_null - 1.0).abs() < 0.06);
    assert!(worst < 2.0, "worst TL deviation {worst} dB");
}

// ------------------------------------------------------------------ throughput

/// Release-mode throughput of the solver on a chain of ducts and junctions.
#[test]
#[ignore]
fn bench_cells_per_second() {
    for (segs, len, simd) in [(1usize, 10.08f32, true), (1, 10.08, false), (24, 0.42, true), (24, 0.42, false)] {
        let mut net = net_for(Gas::EXHAUST, 293.0);
        net.grid.set_simd(simd);
        let mut prev: Option<usize> = None;
        let mut first = 0;
        for _ in 0..segs {
            let d = net.add_duct(&spec("seg", len, 0.06, 0.028), P_AMB, 700.0);
            if let Some(p) = prev {
                net.add_node(Node::junction("j").with_leg(PipeLeg::new(p, End::Right)).with_leg(PipeLeg::new(d, End::Left)));
            } else {
                first = d;
            }
            prev = Some(d);
        }
        let last = prev.unwrap();
        net.add_node(Node::closed("c", PipeLeg::new(first, End::Left)));
        let ar = net.grid.end_area(last, End::Right);
        net.add_node(Node::open("m", PipeLeg::new(last, End::Right), OpenEnd::radiating(ar)));
        bump(&mut net, first, 0, 3, 20_000.0);
        let cells: usize = (0..net.grid.ducts.len()).map(|d| net.grid.n(d)).sum();
        let dt = 1.0 / FS;
        for _ in 0..2000 {
            net.step(dt, &mut []);
        }
        let t0 = std::time::Instant::now();
        let steps = 20_000;
        for _ in 0..steps {
            net.step(dt, &mut []);
        }
        let el = t0.elapsed().as_secs_f64();
        // stage breakdown
        let (mut ta, mut tb, mut tc) = (0.0f64, 0.0f64, 0.0f64);
        for _ in 0..steps {
            let a = std::time::Instant::now();
            net.grid.update_primitives();
            let b = std::time::Instant::now();
            for n in net.nodes.iter_mut() {
                n.solve(&mut net.grid, &mut [], dt, &net.env);
            }
            let c = std::time::Instant::now();
            net.grid.advance(dt);
            let d = std::time::Instant::now();
            ta += (b - a).as_secs_f64();
            tb += (c - b).as_secs_f64();
            tc += (d - c).as_secs_f64();
        }
        println!("  stages per step: primitives {:.2} us, nodes {:.2} us, advance {:.2} us", ta / steps as f64 * 1e6, tb / steps as f64 * 1e6, tc / steps as f64 * 1e6);
        println!(
            "BENCH simd={simd} {segs} segments, {cells} cells: {:.1} ns per cell-step, {:.1}% of one core at 44.1 kHz, fallbacks {}, {:.2} Newton iterations per junction",
            el / (steps * cells) as f64 * 1e9,
            100.0 * el / steps as f64 * 44100.0,
            net.fallbacks(),
            net.mean_iterations()
        );
    }
}

/// Cost of one two-leg junction solve in isolation.
#[test]
#[ignore]
fn bench_junction_solve() {
    let mut net = net_for(Gas::EXHAUST, 293.0);
    let a = net.add_duct(&spec("a", 0.4, 0.05, 0.028), P_AMB, 700.0);
    let b = net.add_duct(&spec("b", 0.4, 0.07, 0.028), P_AMB, 700.0);
    net.add_node(Node::junction("j").with_leg(PipeLeg::new(a, End::Right)).with_leg(PipeLeg::new(b, End::Left)));
    bump(&mut net, a, 0, 4, 5000.0);
    for _ in 0..200 {
        net.step(1.0 / FS, &mut []);
    }
    let n = 2_000_000;
    let mut env = net.env;
    env.p_amb = P_AMB;
    let t0 = std::time::Instant::now();
    for _ in 0..n {
        net.nodes[0].solve(&mut net.grid, &mut [], 1.0 / FS, &env);
    }
    let el = t0.elapsed().as_secs_f64();
    println!("JUNCTION {:.1} ns per solve, {:.2} iterations", el / n as f64 * 1e9, net.mean_iterations());
}

/// Amplitude lost by a travelling packet over 6 m of hot pipe, by frequency, cell size and amplitude. Prints a table
/// (run with `--ignored --nocapture`); the lab's way of comparing numerical schemes.
#[test]
#[ignore]
fn scheme_loss_table() {
    let temp = 850.0f32;
    let c = Gas::AIR.sound_speed(temp);
    for amp in [100.0f32, 15_000.0] {
        println!("packet amplitude {amp:.0} Pa in {temp:.0} K air (c = {c:.0} m/s): loss over 6 m, dB");
        for dx in [0.014f32, 0.028, 0.05] {
            let mut line = format!("  dx {:.0} mm:", dx * 1e3);
            for f in [400.0f32, 800.0, 1200.0, 1600.0, 2400.0] {
                let mut net = net_for(Gas::AIR, temp);
                let d = net.add_duct(&spec("p", 16.0, 0.05, dx), P_AMB, temp);
                net.grid.dissipation = false;
                let ar = net.grid.end_area(d, End::Left);
                net.add_node(Node::open("l", PipeLeg::new(d, End::Left), OpenEnd::anechoic(ar)));
                net.add_node(Node::open("r", PipeLeg::new(d, End::Right), OpenEnd::anechoic(ar)));
                let lam = c / f;
                let n_cells = ((4.0 * lam) / dx) as usize;
                net.grid.update_primitives();
                let c0 = ((0.5 + 4.0 * lam) / dx) as usize;
                for i in 0..n_cells {
                    let idx = c0 + i - n_cells;
                    let x = i as f32 / n_cells as f32;
                    let env = 0.5 - 0.5 * (std::f32::consts::TAU * x).cos();
                    let a_p = amp * env * (std::f32::consts::TAU * 4.0 * x).sin();
                    let (p0, rho0) = (net.grid.p_at(d, idx), net.grid.rho_at(d, idx));
                    let p = p0 + a_p;
                    let rho = rho0 * (p / p0).powf(1.0 / 1.35);
                    net.grid.set_cell(d, idx, rho, a_p / (rho0 * c), p);
                }
                net.grid.update_primitives();
                let (near, far) = ((6.0 / dx) as usize, (12.0 / dx) as usize);
                let (mut pk_near, mut pk_far) = (0.0f32, 0.0f32);
                let dt = 0.7 * dx / (c + amp / (1.2 * c));
                let steps = (0.05 / dt) as usize;
                for _ in 0..steps {
                    net.step(dt, &mut []);
                    pk_near = pk_near.max((net.grid.p_at(d, near) - P_AMB).abs());
                    pk_far = pk_far.max((net.grid.p_at(d, far) - P_AMB).abs());
                }
                line += &format!(" {f:.0} Hz ({:.0}/wave): {:.2}", lam / dx, -20.0 * (pk_far / pk_near).log10());
            }
            println!("{line}");
        }
    }
}
