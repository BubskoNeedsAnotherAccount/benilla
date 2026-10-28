//! Where ducts meet: junctions, valves, open ends and walls.
//!
//! A junction joins any number of duct ends (and, through orifices, any number of reservoirs) at one
//! stagnation pressure `P0`. Each duct end contributes the exact isentropic simple-wave relation between
//! the pressure and velocity at its face and its interior cell (a Riemann-invariant boundary), so waves
//! scatter off the junction exactly as plane-wave theory says (`R = (A1-A2)/(A1+A2)` and so on) and
//! stay nonlinear at blow-down amplitudes. Mass is conserved at the node and energy by enthalpy-weighted
//! mixing; a `K` loss coefficient per leg and direction models contraction / expansion / merging losses.
//!
//! With `s = 1 - (u - u0)/(n c0)` and `n = 2/(gamma-1)` an integer, the face state is
//! `rho = rho0 s^n`, `p = p0 s^(n+2)`, so a leg is a handful of multiplications - the whole network
//! costs little more than its cells.

use super::gas::{powi_fast, Gas};
use super::grid::{End, Grid};
use super::reservoir::{orifice_flow_d, Reservoir};

pub const MAX_LEGS: usize = 8;

/// 1/n for the isentropic exponent n (4..=11).
const INV_NP1: [f32; 12] = [
    0.0,
    0.5,
    1.0 / 3.0,
    0.25,
    0.2,
    1.0 / 6.0,
    1.0 / 7.0,
    0.125,
    1.0 / 9.0,
    0.1,
    1.0 / 11.0,
    1.0 / 12.0,
];
const INV_NM1: [f32; 12] = [
    0.0,
    0.0,
    1.0,
    0.5,
    1.0 / 3.0,
    0.25,
    0.2,
    1.0 / 6.0,
    1.0 / 7.0,
    0.125,
    1.0 / 9.0,
    0.1,
];
const INV_N: [f32; 12] = [
    0.0,
    1.0,
    0.5,
    1.0 / 3.0,
    0.25,
    0.2,
    1.0 / 6.0,
    1.0 / 7.0,
    0.125,
    1.0 / 9.0,
    0.1,
    1.0 / 11.0,
];

/// A duct end attached to a node.
#[derive(Clone, Debug)]
pub struct PipeLeg {
    pub duct: usize,
    pub end: End,
    /// Loss coefficients (referenced to this leg's velocity) for flow into the node and out of it.
    pub k_in: f32,
    pub k_out: f32,
    /// Series inertance of a hole or short tube between this leg's face and the node: the gas in it
    /// (length * A_face / A_hole, metres) resists acceleration. 0 = none.
    pub inert: f32,
    /// Warm start: face velocity toward the node.
    u: f32,
    /// Last solution, for read-out.
    pub mdot: f32,
    pub p_face: f32,
}

impl PipeLeg {
    pub fn new(duct: usize, end: End) -> Self {
        PipeLeg {
            duct,
            end,
            k_in: 0.0,
            k_out: 0.0,
            inert: 0.0,
            u: 0.0,
            mdot: 0.0,
            p_face: 0.0,
        }
    }
    pub fn with_loss(mut self, k_in: f32, k_out: f32) -> Self {
        self.k_in = k_in;
        self.k_out = k_out;
        self
    }
    /// Adds series inertance: `inert = l_hole * A_face / A_hole` (m).
    pub fn with_inertance(mut self, inert: f32) -> Self {
        self.inert = inert;
        self
    }
}

/// A reservoir joined to the node through a variable orifice (valve, throttle, filter, hole).
#[derive(Clone, Debug)]
pub struct ValveLeg {
    pub res: usize,
    /// Effective flow area (Cd * A), m^2. Set by the owner before every step.
    pub area: f32,
    /// Flow from the reservoir into the node this step, kg/s.
    pub mdot: f32,
}

impl ValveLeg {
    pub fn new(res: usize) -> Self {
        ValveLeg {
            res,
            area: 0.0,
            mdot: 0.0,
        }
    }
}

/// A neck - a slug of gas with inertia, a resistance and a quadratic loss - joining the node to a
/// chamber (a reservoir). The Helmholtz resonators, quarter-wave stubs and perforated-pipe
/// absorbers of a muffler are all necks like this one. Because the neck is a leg of the junction
/// solve it couples to the pipe through the waves, not through a single (stiff) cell.
#[derive(Clone, Debug)]
pub struct SlugLeg {
    pub res: usize,
    /// Neck area (total over all holes), m^2, and effective length including end corrections, m.
    pub area: f32,
    pub l_eff: f32,
    /// Series resistance per unit area, Pa s/m, and quadratic loss coefficient.
    pub r: f32,
    pub k: f32,
    /// Velocity into the branch, m/s.
    pub u: f32,
    /// Flow into the branch this step, kg/s.
    pub mdot: f32,
}

impl SlugLeg {
    pub fn new(res: usize, area: f32, l_eff: f32) -> Self {
        SlugLeg {
            res,
            area,
            l_eff,
            r: 0.0,
            k: 1.0,
            u: 0.0,
            mdot: 0.0,
        }
    }
    pub fn with_resistance(mut self, r: f32) -> Self {
        self.r = r;
        self
    }
    /// Helmholtz resonance against a chamber of `volume` with an open (infinite) other side.
    pub fn resonance_hz(&self, c: f32, volume: f32) -> f32 {
        c / std::f32::consts::TAU * (self.area / (volume * self.l_eff)).sqrt()
    }
}

/// How an open end radiates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OpenMode {
    /// Real radiation: an inertial slug of air of the end-correction length in parallel with the
    /// radiation resistance (the Levine-Schwinger low-frequency impedance `rho c [(ka)^2/4 + j 0.6133 ka]`).
    Radiating,
    /// A perfectly matched load (test fixture): all incident sound is absorbed.
    Anechoic,
}

/// An opening to the atmosphere.
#[derive(Clone, Debug)]
pub struct OpenEnd {
    pub mode: OpenMode,
    /// End-correction length, m (0.61 a unflanged, 0.85 a flanged).
    pub delta: f32,
    /// Extra loss for flow *into* the pipe (vena contracta), dimensionless.
    pub k_in: f32,
    /// Velocity of the air slug at the mouth, m/s (outflow positive).
    pub u_slug: f32,
    /// Outputs: volume flow out of the pipe, m^3/s, and its value on the previous step.
    pub q: f32,
    pub q_prev: f32,
    /// Face velocity (outflow positive) and static pressure at the mouth.
    pub u_face: f32,
    pub p_face: f32,
    /// Face area, m^2.
    pub area: f32,
}

impl OpenEnd {
    pub fn radiating(area: f32) -> Self {
        let a = (area / std::f32::consts::PI).sqrt();
        OpenEnd {
            mode: OpenMode::Radiating,
            delta: 0.6133 * a,
            k_in: 0.5,
            u_slug: 0.0,
            q: 0.0,
            q_prev: 0.0,
            u_face: 0.0,
            p_face: 0.0,
            area,
        }
    }
    pub fn anechoic(area: f32) -> Self {
        let mut o = OpenEnd::radiating(area);
        o.mode = OpenMode::Anechoic;
        o.k_in = 0.0;
        o
    }
}

#[derive(Clone, Debug)]
pub enum Terminal {
    /// Several legs meeting at one stagnation pressure.
    Junction,
    /// A rigid closed end (single leg).
    Wall,
    /// An opening to the atmosphere (single leg).
    Open(OpenEnd),
    /// A single duct end opening into a large volume (reservoir `res`): a plenum or airbox, treated as at rest.
    Tank(usize),
}

/// Ambient conditions the nodes need.
#[derive(Clone, Copy, Debug)]
pub struct Env {
    pub p_amb: f32,
    pub t_amb: f32,
    pub rho_amb: f32,
    pub c_amb: f32,
    pub gas: Gas,
    /// Scales the Newton convergence tolerances of the junctions (1 = real-time default; tests use less).
    pub tol: f32,
}

impl Env {
    pub fn new(p_amb: f32, t_amb: f32) -> Env {
        let gas = Gas::AIR;
        Env {
            p_amb,
            t_amb,
            rho_amb: p_amb / (gas.r * t_amb),
            c_amb: gas.sound_speed(t_amb),
            gas,
            tol: 1.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub legs: Vec<PipeLeg>,
    pub valves: Vec<ValveLeg>,
    pub slugs: Vec<SlugLeg>,
    pub terminal: Terminal,
    /// Fraction of the through-flow enthalpy (above ambient) removed by a turbine at this node.
    pub extract: f32,
    p0: f32,
    /// Newton failures that needed the bisection fallback, and iteration statistics (diagnostics).
    pub fallbacks: u32,
    pub iters: u64,
    pub solves: u64,
    /// Time spent solving this node, TSC cycles (diagnostics).
    pub cycles: u64,
    /// Description of the last solve that needed the fallback (diagnostics).
    pub last_fail: String,
}

#[derive(Clone, Copy, Default)]
struct LegW {
    rho0: f32,
    u0: f32,
    p0: f32,
    c0: f32,
    a: f32,
    n: i32,
    kin: f32,
    kout: f32,
    u: f32,
    u_lo: f32,
    u_hi: f32,
    /// Series inertance per step (rho * inert / dt) and the previous face velocity it acts against.
    iner_dt: f32,
    u_prev: f32,
    /// Reciprocals precomputed once per solve (divisions are the slow part of a leg).
    inv_nc: f32,
    inv_c0: f32,
    /// Largest Newton step in velocity.
    dmax: f32,
    inv_a: f32,
    /// Slug legs: `slug` marks them; `rho0` is the neck gas density, `p0` the chamber pressure,
    /// `kin` the quadratic loss, `kout` the series resistance, `c0` the inertia per step (M/dt),
    /// `u0` the velocity into the branch on the previous step. `u` is the flow velocity *toward the node*.
    slug: bool,
}

/// Face state of a leg for an assumed velocity `u` (toward the node).
#[derive(Clone, Copy, Default)]
struct Eval {
    /// Mass flux rho*u, d/du.
    q: f32,
    dq: f32,
    /// Stagnation-referenced pressure and its derivative.
    pt: f32,
    dpt: f32,
    rho: f32,
    p: f32,
    /// d(p)/du of the plain face pressure.
    dp: f32,
}

fn area_guard(a: f32) -> f32 {
    a.max(1.0e-9)
}

impl LegW {
    fn new(d: &Grid, leg: &PipeLeg, dt: f32) -> LegW {
        let (rho0, u0, p0, c0, t0) = d.end_state(leg.duct, leg.end);
        let n = d.gas.isentropic_n(t0);
        let nf = n as f32;
        let inv_c0 = 1.0 / c0;
        // sonic limits of the isentropic simple-wave branch
        let ni = (n as usize).min(11);
        let u_hi = 0.97 * (nf * c0 + u0) * INV_NP1[ni];
        let u_lo = -0.97 * (nf * c0 + u0) * INV_NM1[ni];
        LegW {
            rho0,
            u0,
            p0,
            c0,
            a: d.end_area(leg.duct, leg.end),
            n,
            kin: leg.k_in,
            kout: leg.k_out,
            u: leg.u.clamp(u_lo, u_hi),
            u_lo,
            u_hi,
            iner_dt: rho0 * leg.inert / dt,
            u_prev: leg.u,
            inv_nc: inv_c0 * INV_N[ni],
            inv_c0,
            dmax: 0.0,
            inv_a: d.end_inv_area(leg.duct, leg.end),
            slug: false,
        }
    }

    fn neck(sl: &SlugLeg, rho_neck: f32, p_chamber: f32, dt: f32) -> LegW {
        let m = rho_neck * sl.l_eff;
        LegW {
            rho0: rho_neck,
            u0: sl.u,
            p0: p_chamber,
            c0: m / dt,
            a: sl.area,
            n: 0,
            kin: sl.k,
            kout: sl.r,
            u: -sl.u,
            u_lo: -1.0e3,
            u_hi: 1.0e3,
            iner_dt: 0.0,
            u_prev: 0.0,
            inv_nc: 0.0,
            inv_c0: 0.0,
            dmax: 150.0,
            inv_a: 1.0 / area_guard(sl.area),
            slug: true,
        }
    }

    #[inline]
    fn eval(&self, u: f32) -> Eval {
        if self.slug {
            // u is the flow velocity toward the node (out of the branch); the neck obeys
            // P0 = p_chamber + (M/dt)(u_branch - u_old) + R u_branch + 1/2 K rho |u_branch| u_branch
            let ub = -u;
            let au = u.abs();
            let pt = self.p0
                + self.c0 * (ub - self.u0)
                + self.kout * ub
                + 0.5 * self.kin * self.rho0 * au * ub;
            let dpt = -(self.c0 + self.kout + self.kin * self.rho0 * au);
            return Eval {
                q: self.rho0 * u,
                dq: self.rho0,
                pt,
                dpt,
                rho: self.rho0,
                p: pt,
                dp: dpt,
            };
        }
        let nf = self.n as f32;
        let s = (1.0 - (u - self.u0) * self.inv_nc).clamp(0.2, 2.0);
        let inv_s = 1.0 / s;
        let sn = powi_fast(s, self.n);
        let rho = self.rho0 * sn;
        let p = self.p0 * sn * s * s;
        let sig = if u >= 0.0 {
            1.0 - self.kin
        } else {
            -(1.0 + self.kout)
        };
        let au = u.abs();
        let drho = -rho * self.inv_c0 * inv_s;
        let dp = -(nf + 2.0) * p * self.inv_nc * inv_s;
        Eval {
            q: rho * u,
            dq: rho + u * drho,
            pt: p + 0.5 * sig * rho * u * au - self.iner_dt * (u - self.u_prev),
            dpt: dp + 0.5 * sig * (drho * u * au + 2.0 * rho * au) - self.iner_dt,
            rho,
            p,
            dp,
        }
    }
}

impl Node {
    pub fn junction(name: &str) -> Node {
        Node {
            name: name.to_string(),
            legs: Vec::new(),
            valves: Vec::new(),
            slugs: Vec::new(),
            terminal: Terminal::Junction,
            extract: 0.0,
            p0: 0.0,
            fallbacks: 0,
            iters: 0,
            solves: 0,
            cycles: 0,
            last_fail: String::new(),
        }
    }

    pub fn closed(name: &str, leg: PipeLeg) -> Node {
        let mut n = Node::junction(name);
        n.legs.push(leg);
        n.terminal = Terminal::Wall;
        n
    }

    /// A duct end that opens into a big volume `res` (a plenum): pressure at rest on the far side.
    pub fn tank(name: &str, leg: PipeLeg, res: usize) -> Node {
        let mut n = Node::junction(name);
        n.legs.push(leg);
        n.terminal = Terminal::Tank(res);
        n
    }

    pub fn open(name: &str, leg: PipeLeg, end: OpenEnd) -> Node {
        let mut n = Node::junction(name);
        n.legs.push(leg);
        n.terminal = Terminal::Open(end);
        n
    }

    pub fn with_leg(mut self, leg: PipeLeg) -> Node {
        assert!(
            self.legs.len() < MAX_LEGS,
            "node {} has too many legs",
            self.name
        );
        self.legs.push(leg);
        self
    }

    pub fn with_valve(mut self, v: ValveLeg) -> Node {
        self.valves.push(v);
        self
    }

    pub fn with_slug(mut self, v: SlugLeg) -> Node {
        self.slugs.push(v);
        self
    }

    pub fn open_end(&self) -> Option<&OpenEnd> {
        match &self.terminal {
            Terminal::Open(o) => Some(o),
            _ => None,
        }
    }

    /// Solves the node from the current duct states and writes the end fluxes into the ducts.
    pub fn solve(&mut self, grid: &mut Grid, res: &mut [Reservoir], dt: f32, env: &Env) {
        match &self.terminal {
            Terminal::Wall => self.solve_wall(grid),
            Terminal::Open(_) => self.solve_open(grid, dt, env),
            Terminal::Tank(r) => {
                let r = *r;
                self.solve_tank(grid, res, r, dt)
            }
            Terminal::Junction => self.solve_junction(grid, res, dt, env),
        }
    }

    fn solve_wall(&mut self, grid: &mut Grid) {
        let leg = &mut self.legs[0];
        let lw = LegW::new(grid, leg, 1.0);
        let e = lw.eval(0.0);
        leg.mdot = 0.0;
        leg.p_face = e.p;
        grid.set_end(leg.duct, leg.end, [0.0, e.p, 0.0], e.rho, 0.0, e.p);
    }

    fn solve_tank(&mut self, grid: &mut Grid, res: &mut [Reservoir], ri: usize, dt: f32) {
        let leg = &mut self.legs[0];
        let lw = LegW::new(grid, leg, dt);
        let (p_tank, h_tank) = {
            let r = &res[ri];
            (r.p(), r.gas.h(r.t()))
        };
        let mut u = lw.u;
        for _ in 0..10 {
            let e = lw.eval(u);
            let f = e.pt - p_tank;
            let du = (-f / e.dpt).clamp(-0.4 * lw.c0, 0.4 * lw.c0);
            u = (u + du).clamp(lw.u_lo, lw.u_hi);
            if f.abs() < 0.3 + 2.0e-6 * p_tank {
                break;
            }
        }
        let e = lw.eval(u);
        leg.u = u;
        let m = e.rho * u * lw.a;
        let h0_out = grid.gas.h(e.p / (e.rho * grid.gas.r)) + 0.5 * u * u;
        let ef = m * if m > 0.0 { h0_out } else { h_tank };
        let inv_a = lw.inv_a;
        let flux = [m * inv_a, m * inv_a * u + e.p, ef * inv_a];
        leg.mdot = m;
        leg.p_face = e.p;
        let r = &mut res[ri];
        if !r.infinite {
            r.dm += m * dt;
            r.dh += ef * dt;
        }
        let (fx, ux) = if leg.end == End::Right {
            (flux, u)
        } else {
            ([-flux[0], flux[1], -flux[2]], -u)
        };
        grid.set_end(leg.duct, leg.end, fx, e.rho, ux, e.p);
    }

    fn solve_open(&mut self, grid: &mut Grid, dt: f32, env: &Env) {
        let Terminal::Open(oe) = &mut self.terminal else {
            return;
        };
        let leg = &mut self.legs[0];
        let mut lw = LegW::new(grid, leg, dt);
        lw.iner_dt = 0.0;
        lw.kin = 0.0;
        lw.kout = 0.0;
        let a_eq = (oe.area / std::f32::consts::PI).sqrt();
        // series inertance (per unit area) and the parallel radiation resistance
        let (m_in, z_r) = match oe.mode {
            OpenMode::Radiating => {
                let m = env.rho_amb * oe.delta.max(1e-4);
                let z = env.rho_amb * env.c_amb * (2.0 * oe.delta / a_eq).powi(2);
                (m, z)
            }
            OpenMode::Anechoic => (f32::INFINITY, env.rho_amb * env.c_amb),
        };
        let cinv = if m_in.is_finite() {
            dt / m_in + 1.0 / z_r
        } else {
            1.0 / z_r
        };
        let kin = oe.k_in;
        let mut u = leg.u.clamp(lw.u_lo, lw.u_hi);
        for _ in 0..8 {
            let e = lw.eval(u);
            let inflow = u < 0.0;
            let loss = if inflow {
                0.5 * e.rho * (1.0 + kin) * u * u
            } else {
                0.0
            };
            let r = e.p - env.p_amb - (u - oe.u_slug) / cinv + loss;
            let dloss = if inflow { e.rho * (1.0 + kin) * u } else { 0.0 };
            let dr = e.dp - 1.0 / cinv + dloss;
            let du = (-r / dr).clamp(-0.3 * lw.c0, 0.3 * lw.c0);
            u = (u + du).clamp(lw.u_lo, lw.u_hi);
            if r.abs() < 0.05 {
                break;
            }
        }
        let e = lw.eval(u);
        leg.u = u;
        // advance the slug with the pressure that drove it
        let drive = {
            let inflow = u < 0.0;
            e.p - env.p_amb
                + if inflow {
                    0.5 * e.rho * (1.0 + kin) * u * u
                } else {
                    0.0
                }
        };
        if m_in.is_finite() {
            oe.u_slug += dt * drive / m_in;
        }
        let m = e.rho * u * lw.a;
        let a = lw.a;
        let h_out = grid.gas.h(e.p / (e.rho * grid.gas.r)) + 0.5 * u * u;
        let h_in = env.gas.h(env.t_amb);
        let ef = m * if u >= 0.0 { h_out } else { h_in };
        let flux = [m / a, m / a * u + e.p, ef / a];
        oe.q_prev = oe.q;
        oe.q = u * a;
        oe.u_face = u;
        oe.p_face = e.p;
        leg.mdot = m;
        leg.p_face = e.p;
        let (fx, ux) = if leg.end == End::Right {
            (flux, u)
        } else {
            ([-flux[0], flux[1], -flux[2]], -u)
        };
        grid.set_end(leg.duct, leg.end, fx, e.rho, ux, e.p);
    }

    fn solve_junction(&mut self, grid: &mut Grid, res: &mut [Reservoir], dt: f32, env: &Env) {
        let np = self.legs.len();
        let ns = self.slugs.len();
        let nl = np + ns;
        let mut lw = [LegW::default(); MAX_LEGS];
        for (k, leg) in self.legs.iter().enumerate() {
            lw[k] = LegW::new(grid, leg, dt);
            lw[k].dmax = 0.5 * lw[k].c0;
        }
        let rho_pipe = if np > 0 { lw[0].rho0 } else { env.rho_amb };
        for (j, sl) in self.slugs.iter().enumerate() {
            let ch = &res[sl.res];
            let rho_neck = if sl.u >= 0.0 { rho_pipe } else { ch.rho() };
            lw[np + j] = LegW::neck(sl, rho_neck, ch.p(), dt);
            lw[np + j].dmax = 150.0;
        }
        // starting stagnation pressure: keep the previous one if it is sensible, else the mean face pressure
        let mean_p = lw[..np].iter().map(|l| l.p0).sum::<f32>() / np.max(1) as f32;
        let mut p0 = if self.p0 > 1.0e4 { self.p0 } else { mean_p };

        // valve legs: gas for each reservoir, with an upstream temperature estimate on the pipe side
        let t_pipe = if np > 0 {
            grid.end_state(self.legs[0].duct, self.legs[0].end).4
        } else {
            env.t_amb
        };
        let gas_pipe = if np > 0 { grid.gas } else { env.gas };
        // flow from the reservoir into the node and its derivative with respect to the node pressure
        let valve_eval = |res: &[Reservoir], v: &ValveLeg, p0: f32| -> (f32, f32) {
            if v.area <= 2.0e-6 {
                return (0.0, 0.0);
            }
            let r = &res[v.res];
            let pr = r.p();
            if p0 < pr {
                let (m, _, d_dn) = orifice_flow_d(&r.gas, v.area, pr, r.t(), p0);
                (m, d_dn)
            } else {
                let (m, d_up, _) = orifice_flow_d(&gas_pipe, v.area, p0, t_pipe, pr);
                (-m, -d_up)
            }
        };
        let valve_flow =
            |res: &[Reservoir], v: &ValveLeg, p0: f32| -> f32 { valve_eval(res, v, p0).0 };

        let mut ev = [Eval::default(); MAX_LEGS];
        let mut converged = false;
        self.solves += 1;
        for _ in 0..14 {
            self.iters += 1;
            let mut g = 0.0f32;
            let mut scale = 0.0f32;
            let mut denom = 0.0f32;
            for v in &self.valves {
                let (m, d) = valve_eval(res, v, p0);
                g += m;
                scale += m.abs();
                denom += d.min(-1.0e-12) * (v.area > 2.0e-6) as u8 as f32;
            }
            let mut numer = 0.0f32;
            let mut fk = [0.0f32; MAX_LEGS];
            let mut dpk = [0.0f32; MAX_LEGS];
            let mut worst = 0.0f32;
            for k in 0..nl {
                let e = lw[k].eval(lw[k].u);
                let f = e.pt - p0;
                let m = lw[k].a * e.q;
                g += m;
                scale += m.abs();
                fk[k] = f;
                dpk[k] = e.dpt;
                let r = lw[k].a * e.dq / e.dpt;
                denom += r;
                numer += r * f;
                worst = worst.max(f.abs());
                ev[k] = e;
            }
            if g.abs() < (5.0e-4 * scale + 4.0e-6) * env.tol
                && worst < (0.3 + 2.0e-6 * p0) * env.tol
            {
                converged = true;
                break;
            }
            let dp0 = ((-g + numer) / denom).clamp(-0.5 * p0, 0.5 * p0);
            p0 = (p0 + dp0).max(5.0e3);
            for k in 0..nl {
                let du = (dp0 - fk[k]) / dpk[k];
                lw[k].u =
                    (lw[k].u + du.clamp(-lw[k].dmax, lw[k].dmax)).clamp(lw[k].u_lo, lw[k].u_hi);
            }
        }
        if !converged {
            self.fallbacks += 1;
            if self.fallbacks == 1 {
                self.last_fail = format!(
                    "p0 {:.0} legs {:?} valves {:?} slugs {}",
                    p0,
                    (0..nl)
                        .map(|k| (lw[k].u, ev[k].pt - p0, lw[k].a * ev[k].q))
                        .collect::<Vec<_>>(),
                    self.valves
                        .iter()
                        .map(|v| (v.area, res[v.res].p(), res[v.res].t()))
                        .collect::<Vec<_>>(),
                    ns
                );
            }
            // robust fallback: bisection on P0 with each leg solved for its own velocity
            let (mut lo, mut hi) = (0.25 * mean_p, 4.0 * mean_p);
            let flow_at = |lw: &mut [LegW; MAX_LEGS], p: f32| -> f32 {
                let mut g = self
                    .valves
                    .iter()
                    .map(|v| valve_flow(res, v, p))
                    .sum::<f32>();
                for k in 0..nl {
                    let mut u = lw[k].u;
                    for _ in 0..14 {
                        let e = lw[k].eval(u);
                        let du = ((p - e.pt) / e.dpt).clamp(-lw[k].dmax, lw[k].dmax);
                        u = (u + du).clamp(lw[k].u_lo, lw[k].u_hi);
                        if du.abs() < 2.0e-3 {
                            break;
                        }
                    }
                    lw[k].u = u;
                    g += lw[k].a * lw[k].eval(u).q;
                }
                g
            };
            for _ in 0..24 {
                let mid = 0.5 * (lo + hi);
                if flow_at(&mut lw, mid) > 0.0 {
                    lo = mid; // net flow toward the node: pressure too low
                } else {
                    hi = mid;
                }
            }
            p0 = 0.5 * (lo + hi);
            flow_at(&mut lw, p0);
        }
        self.p0 = p0;

        // ---- converged: mixed enthalpy of everything flowing into the node
        if !converged {
            for k in 0..nl {
                ev[k] = lw[k].eval(lw[k].u);
            }
        }
        let (mut m_out, mut h_sum) = (0.0f32, 0.0f32);
        for k in 0..nl {
            let e = ev[k];
            let m = lw[k].a * e.q;
            if m > 0.0 {
                let h0 = if k < np {
                    let gas = &grid.gas;
                    gas.h(e.p / (e.rho * gas.r)) + 0.5 * lw[k].u * lw[k].u
                } else {
                    let r = &res[self.slugs[k - np].res];
                    r.gas.h(r.t())
                };
                m_out += m;
                h_sum += m * h0;
            }
        }
        for v in self.valves.iter_mut() {
            let m = valve_flow(res, v, p0);
            v.mdot = m;
            if m > 0.0 {
                let r = &res[v.res];
                m_out += m;
                h_sum += m * r.gas.h(r.t());
            }
        }
        let mut h_mix = if m_out > 1.0e-12 {
            h_sum / m_out
        } else {
            // nothing is flowing: any value works, take the mean pipe enthalpy
            let mut s = 0.0;
            for k in 0..np {
                let g = &grid.gas;
                s += g.h(grid.end_state(self.legs[k].duct, self.legs[k].end).4);
            }
            s / np.max(1) as f32
        };
        if self.extract > 0.0 {
            let h_ref = env.gas.h(env.t_amb);
            h_mix -= self.extract * (h_mix - h_ref).max(0.0);
        }

        // ---- fluxes into the ducts
        for k in 0..np {
            let e = ev[k];
            let m = lw[k].a * e.q;
            let u = lw[k].u;
            let gas = grid.gas;
            let ef = if m > 0.0 {
                m * (gas.h(e.p / (e.rho * gas.r)) + 0.5 * u * u)
            } else {
                m * h_mix
            };
            let inv_a = lw[k].inv_a;
            let flux = [m * inv_a, m * inv_a * u + e.p, ef * inv_a];
            let leg = &mut self.legs[k];
            leg.u = u;
            leg.mdot = m;
            leg.p_face = e.p;
            let (fx, ux) = if leg.end == End::Right {
                (flux, u)
            } else {
                ([-flux[0], flux[1], -flux[2]], -u)
            };
            grid.set_end(leg.duct, leg.end, fx, e.rho, ux, e.p);
        }
        // ---- necks and reservoirs
        for j in 0..ns {
            let w = lw[np + j].u;
            let m = lw[np + j].a * lw[np + j].rho0 * w;
            let sl = &mut self.slugs[j];
            sl.u = -w;
            sl.mdot = -m;
            let r = &mut res[sl.res];
            if !r.infinite {
                r.dm -= m * dt;
                r.dh -= m * dt * if m > 0.0 { r.gas.h(r.t()) } else { h_mix };
            }
        }
        for v in &self.valves {
            let r = &mut res[v.res];
            if r.infinite {
                continue;
            }
            r.dm -= v.mdot * dt;
            r.dh -= v.mdot * dt * if v.mdot >= 0.0 { r.gas.h(r.t()) } else { h_mix };
        }
    }
}
