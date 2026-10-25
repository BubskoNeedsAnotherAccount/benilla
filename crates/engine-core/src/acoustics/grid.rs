//! The cells of every pipe in a network, stored as one flat structure-of-arrays so the numerics run as
//! a few long, branch-free passes the compiler can vectorise (AVX2/FMA when the CPU has it).
//!
//! Each duct is a run of cells with a ghost cell on either side; the ghosts carry the face state the
//! junction / valve / open-end nodes computed, which lets the end cells keep a second-order slope.
//!
//! The model is quasi-1-D compressible Euler flow with area variation, wall friction and wall heat
//! transfer, solved with a second-order MUSCL-Hancock / HLL finite-volume scheme:
//!
//! ```text
//!   d(A rho)/dt   + d(A rho u)/dx          = 0
//!   d(A rho u)/dt + d(A (rho u^2 + p))/dx  = p dA/dx - F_friction A
//!   d(A E)/dt     + d(A u (E + p))/dx      = q_wall A
//! ```
//!
//! Smooth regions use unlimited central slopes (no numerical damping of sound), a pressure-curvature
//! sensor hands over to a van Leer limiter at fronts, so blow-down shocks stay monotone while a 1 kHz
//! wave survives a 6 m pipe with ~1 dB of numerical loss. The scheme is nonlinear, so pulses steepen
//! and sound speed follows `c + u` the way it does in a real header. Wall temperature is a state: a
//! long pipe really does cool the gas (lower speed of sound), and a cold exhaust warms up with time.

use super::gas::Gas;
use super::material::Wall;

macro_rules! simd_kernel {
    (fn $name:ident / $avx:ident ( $($a:ident : $t:ty),* ) $(-> $r:ty)? $body:block) => {
        #[inline(always)]
        #[allow(clippy::too_many_arguments)]
        fn $name($($a: $t),*) $(-> $r)? $body
        #[cfg(target_arch = "x86_64")]
        #[target_feature(enable = "avx2,fma")]
        #[allow(clippy::too_many_arguments)]
        unsafe fn $avx($($a: $t),*) $(-> $r)? {
            $name($($a),*)
        }
    };
}

/// Calls a kernel or its AVX2 twin depending on what the CPU supports.
macro_rules! call {
    ($self:ident, $k:ident, $kavx:ident, $($arg:expr),* $(,)?) => {{
        #[cfg(target_arch = "x86_64")]
        {
            if $self.simd {
                // SAFETY: the CPU features were detected when the grid was built.
                unsafe { $kavx($($arg),*) }
            } else {
                $k($($arg),*)
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            $k($($arg),*)
        }
    }};
}


pub const SIGMA_SB: f32 = 5.670_37e-8;
const RHO_MIN: f32 = 0.02;
const P_MIN: f32 = 2_000.0;
/// Pressure-curvature sensor band over which central slopes blend into limited ones.
const SENSOR_LO: f32 = 0.004;
const SENSOR_HI: f32 = 0.03;
/// The slow thermal / friction-coefficient update runs once per this many steps.
pub const SLOW_EVERY: u32 = 128;

/// Which end of a duct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Left,
    Right,
}

/// Geometry and physics of a duct to be built.
#[derive(Clone, Debug)]
pub struct DuctSpec {
    pub name: String,
    pub length: f32,
    /// Bore at the left / right end (linear taper between them), m.
    pub d_in: f32,
    pub d_out: f32,
    pub wall: Wall,
    /// Target cell size, m (the actual cell size divides the length evenly).
    pub dx_target: f32,
    /// Open-area fraction: scales the flow area (a catalyst monolith is ~75 % open).
    pub area_scale: f32,
    /// Hydraulic diameter of the channels, if not the bore (monolith channels are ~1 mm).
    pub hyd_dia: Option<f32>,
    /// Multiplier on wall friction (1 = smooth-pipe correlations).
    pub friction_scale: f32,
    /// In the engine bay: still, hot air around the pipe instead of road airflow.
    pub engine_bay: bool,
}

impl DuctSpec {
    pub fn pipe(name: &str, length: f32, dia: f32, wall: Wall, dx_target: f32) -> Self {
        DuctSpec {
            name: name.to_string(),
            length,
            d_in: dia,
            d_out: dia,
            wall,
            dx_target,
            area_scale: 1.0,
            hyd_dia: None,
            friction_scale: 1.0,
            engine_bay: false,
        }
    }
    pub fn taper(mut self, d_out: f32) -> Self {
        self.d_out = d_out;
        self
    }
}

/// Bookkeeping for one duct inside the grid.
#[derive(Clone, Debug)]
pub struct DuctInfo {
    pub name: String,
    /// Index of the first real cell (the ghost cell is `base - 1`).
    pub base: usize,
    pub n: usize,
    pub dx: f32,
    pub length: f32,
    pub wall: Wall,
    /// Boundary fluxes per unit area in +x at the left and right face: (mass, momentum, energy).
    pub bl: [f32; 3],
    pub br: [f32; 3],
    /// Face states (rho, u along +x, p) the nodes computed at the two ends.
    pub gl: [f32; 3],
    pub gr: [f32; 3],
    /// 1 / end-face area at the left and right ends.
    pub inv_al: f32,
    pub inv_ar: f32,
}

#[derive(Clone, Debug)]
pub struct Grid {
    pub gas: Gas,
    pub ducts: Vec<DuctInfo>,
    /// Total array length including ghost cells.
    pub m: usize,
    /// Switch off friction, boundary-layer damping and wall heat transfer (clean-physics tests).
    pub dissipation: bool,
    /// Environment for the walls: ambient temperature and external convection, W/(m^2 K).
    pub t_amb: f32,
    pub h_conv: f32,
    /// Speeds the wall's thermal evolution up (1 = real time) so a cold exhaust can be soaked quickly.
    pub thermal_accel: f32,
    /// Largest `(|u| + c)/dx` at the last primitive update: the CFL time step is `cfl / max_rate`.
    pub max_rate: f32,
    // conserved
    pub rho: Vec<f32>,
    pub mom: Vec<f32>,
    pub en: Vec<f32>,
    // primitives
    pub u: Vec<f32>,
    pub p: Vec<f32>,
    pub t: Vec<f32>,
    pub c: Vec<f32>,
    /// Wall temperature per cell, K.
    pub tw: Vec<f32>,
    // static geometry / wall data per cell
    a_c: Vec<f32>,
    a_lo: Vec<f32>,
    a_hi: Vec<f32>,
    alog: Vec<f32>,
    inv_dx: Vec<f32>,
    inv_acdx: Vec<f32>,
    dh: Vec<f32>,
    fr4: Vec<f32>,
    fscale: Vec<f32>,
    cap: Vec<f32>,
    eps: Vec<f32>,
    rins: Vec<f32>,
    outer: Vec<f32>,
    /// Outside-air model per cell: multiplier on the convection coefficient and temperature offset (K).
    bay_k: Vec<f32>,
    bay_dt: Vec<f32>,
    // slowly updated
    hv: Vec<f32>,
    k_ac: Vec<f32>,
    k_lam: Vec<f32>,
    c_tur: Vec<f32>,
    u_avg: Vec<f32>,
    // scratch
    rl: Vec<f32>,
    ul: Vec<f32>,
    pl: Vec<f32>,
    el: Vec<f32>,
    rr: Vec<f32>,
    ur: Vec<f32>,
    pr: Vec<f32>,
    er: Vec<f32>,
    f0: Vec<f32>,
    f1: Vec<f32>,
    f2: Vec<f32>,
    step_count: u32,
    slow_dt: f32,
    simd: bool,
}

fn detect_simd() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Plain compare-select max / min / clamp: these lower to vmaxps / vminps and let the loops vectorise
/// (`f32::max` and `clamp` carry NaN handling that blocks it).
#[inline(always)]
fn mx(a: f32, b: f32) -> f32 {
    if a > b {
        a
    } else {
        b
    }
}

#[inline(always)]
fn mn(a: f32, b: f32) -> f32 {
    if a < b {
        a
    } else {
        b
    }
}

#[inline(always)]
fn cl(x: f32, lo: f32, hi: f32) -> f32 {
    mn(mx(x, lo), hi)
}

#[inline(always)]
fn vanleer(a: f32, b: f32) -> f32 {
    (a * b.abs() + a.abs() * b) / (a.abs() + b.abs() + 1.0e-20)
}

impl Grid {
    pub fn new(gas: Gas, t_amb: f32) -> Grid {
        Grid {
            gas,
            ducts: Vec::new(),
            m: 0,
            dissipation: true,
            t_amb,
            h_conv: 12.0,
            thermal_accel: 1.0,
            max_rate: 0.0,
            rho: vec![],
            mom: vec![],
            en: vec![],
            u: vec![],
            p: vec![],
            t: vec![],
            c: vec![],
            tw: vec![],
            a_c: vec![],
            a_lo: vec![],
            a_hi: vec![],
            alog: vec![],
            inv_dx: vec![],
            inv_acdx: vec![],
            dh: vec![],
            fr4: vec![],
            fscale: vec![],
            cap: vec![],
            eps: vec![],
            rins: vec![],
            outer: vec![],
            bay_k: vec![],
            bay_dt: vec![],
            hv: vec![],
            k_ac: vec![],
            k_lam: vec![],
            c_tur: vec![],
            u_avg: vec![],
            rl: vec![],
            ul: vec![],
            pl: vec![],
            el: vec![],
            rr: vec![],
            ur: vec![],
            pr: vec![],
            er: vec![],
            f0: vec![],
            f1: vec![],
            f2: vec![],
            step_count: 0,
            slow_dt: 0.0,
            simd: detect_simd(),
        }
    }

    /// Appends a duct (two ghost cells plus its real cells) at uniform state `p0`, `t0`; returns its id.
    pub fn add_duct(&mut self, spec: &DuctSpec, p0: f32, t0: f32) -> usize {
        // Two cells at least, except in a duct shorter than about one and a half target cells: halving a short
        // duct would make its cells the smallest in the network, and the CFL limit of those few cells sets the
        // sub-stepping (and so the cost) of every other duct and junction.
        let n = ((spec.length / spec.dx_target).round() as usize).max(if spec.length < 1.6 * spec.dx_target { 1 } else { 2 });
        let dx = spec.length / n as f32;
        let area = |x: f32| {
            let d = spec.d_in + (spec.d_out - spec.d_in) * (x / spec.length);
            spec.area_scale * std::f32::consts::FRAC_PI_4 * d * d
        };
        let gas = self.gas;
        let rho0 = p0 / (gas.r * t0);
        let e0 = rho0 * gas.e(t0);
        let base = self.m + 1;
        for k in 0..n + 2 {
            // k = 0 and n + 1 are ghosts and take the neighbouring cell's geometry
            let i = k.clamp(1, n) - 1;
            let a_lo = area(i as f32 * dx);
            let a_hi = area((i + 1) as f32 * dx);
            let a_c = area((i as f32 + 0.5) * dx);
            let dh = spec.hyd_dia.unwrap_or_else(|| spec.d_in + (spec.d_out - spec.d_in) * ((i as f32 + 0.5) * dx / spec.length));
            let rr = (spec.wall.roughness_m / dh).clamp(1e-7, 0.05);
            let f = 1.0 / (2.0 * (rr / 3.7).log10()).powi(2);
            self.rho.push(rho0);
            self.mom.push(0.0);
            self.en.push(e0);
            self.u.push(0.0);
            self.p.push(p0);
            self.t.push(t0);
            self.c.push(gas.sound_speed(t0));
            self.tw.push(t0);
            self.a_c.push(a_c);
            self.a_lo.push(a_lo);
            self.a_hi.push(a_hi);
            self.alog.push((a_hi - a_lo) / (dx * a_c));
            self.inv_dx.push(1.0 / dx);
            self.inv_acdx.push(1.0 / (a_c * dx));
            self.dh.push(dh);
            self.fr4.push(f * f * f * f);
            self.fscale.push(spec.friction_scale);
            self.cap.push(spec.wall.cap);
            self.eps.push(spec.wall.eps);
            self.rins.push(spec.wall.r_ins);
            self.outer.push(spec.wall.outer_ratio);
            self.bay_k.push(if spec.engine_bay { 0.3 } else { 1.0 });
            self.bay_dt.push(if spec.engine_bay { 60.0 } else { 0.0 });
            for v in [&mut self.hv, &mut self.k_ac, &mut self.k_lam, &mut self.c_tur, &mut self.u_avg] {
                v.push(0.0);
            }
            for v in [&mut self.rl, &mut self.ul, &mut self.pl, &mut self.el, &mut self.rr, &mut self.ur, &mut self.pr, &mut self.er] {
                v.push(0.0);
            }
        }
        for v in [&mut self.f0, &mut self.f1, &mut self.f2] {
            v.extend(std::iter::repeat(0.0).take(n + 2));
        }
        self.m += n + 2;
        self.ducts.push(DuctInfo {
            name: spec.name.clone(),
            base,
            n,
            dx,
            length: spec.length,
            wall: spec.wall,
            bl: [0.0, p0, 0.0],
            br: [0.0, p0, 0.0],
            gl: [rho0, 0.0, p0],
            gr: [rho0, 0.0, p0],
            inv_al: 1.0 / area(0.0),
            inv_ar: 1.0 / area(spec.length),
        });
        let id = self.ducts.len() - 1;
        self.slow_update_range(base, base + n, 0.0);
        id
    }

    /// Forces the vector path on or off (benchmarks and tests); the default is what the CPU supports.
    pub fn set_simd(&mut self, on: bool) {
        self.simd = on && detect_simd();
    }

    // ------------------------------------------------------------------ accessors

    pub fn n(&self, d: usize) -> usize {
        self.ducts[d].n
    }
    /// 1/dx of flat cell `k` (diagnostics).
    pub fn inv_dx_of(&self, k: usize) -> f32 {
        self.inv_dx[k]
    }
    /// Gas temperature of flat cell `k` (diagnostics).
    pub fn mean_temperature_of(&self, k: usize) -> f32 {
        self.t[k]
    }
    pub fn dx(&self, d: usize) -> f32 {
        self.ducts[d].dx
    }
    /// Index into the flat arrays of cell `i` of duct `d`.
    #[inline]
    pub fn idx(&self, d: usize, i: usize) -> usize {
        self.ducts[d].base + i
    }
    pub fn p_at(&self, d: usize, i: usize) -> f32 {
        self.p[self.idx(d, i)]
    }
    pub fn u_at(&self, d: usize, i: usize) -> f32 {
        self.u[self.idx(d, i)]
    }
    pub fn t_at(&self, d: usize, i: usize) -> f32 {
        self.t[self.idx(d, i)]
    }
    pub fn rho_at(&self, d: usize, i: usize) -> f32 {
        self.rho[self.idx(d, i)]
    }
    pub fn tw_at(&self, d: usize, i: usize) -> f32 {
        self.tw[self.idx(d, i)]
    }
    pub fn area_at(&self, d: usize, i: usize) -> f32 {
        self.a_c[self.idx(d, i)]
    }

    /// Sets one cell from primitive variables.
    pub fn set_cell(&mut self, d: usize, i: usize, rho: f32, u: f32, p: f32) {
        let k = self.idx(d, i);
        let t = p / (rho * self.gas.r);
        self.rho[k] = rho;
        self.mom[k] = rho * u;
        self.en[k] = rho * (self.gas.e(t) + 0.5 * u * u);
    }

    /// Resets every cell of a duct to a uniform state at rest (wall temperature follows `t`).
    pub fn fill(&mut self, d: usize, p: f32, t: f32) {
        let rho = p / (self.gas.r * t);
        let e = rho * self.gas.e(t);
        let (b, n) = (self.ducts[d].base, self.ducts[d].n);
        for k in b - 1..b + n + 1 {
            self.rho[k] = rho;
            self.mom[k] = 0.0;
            self.en[k] = e;
            self.tw[k] = t;
        }
    }

    pub fn set_wall_temperature(&mut self, d: usize, t: f32) {
        let (b, n) = (self.ducts[d].base, self.ducts[d].n);
        for k in b - 1..b + n + 1 {
            self.tw[k] = t;
        }
    }

    /// Gas and wall temperature of every cell of duct `d`, in order along the duct.
    pub fn thermal_profile(&self, d: usize) -> (Vec<f32>, Vec<f32>) {
        let (b, n) = (self.ducts[d].base, self.ducts[d].n);
        (self.t[b..b + n].to_vec(), self.tw[b..b + n].to_vec())
    }

    /// Sets the temperatures of duct `d` from a profile taken on another grid (another cell size): the profile is
    /// laid along the duct and read at each of this duct's cells (linearly in the position along it), keeping
    /// pressure and velocity. A model that takes over from another is then as warm as it, cell for cell.
    pub fn set_thermal_profile(&mut self, d: usize, gas: &[f32], wall: &[f32]) {
        if gas.is_empty() || wall.is_empty() {
            return;
        }
        let (b, n) = (self.ducts[d].base, self.ducts[d].n);
        let at = |v: &[f32], i: usize| -> f32 {
            let x = (i as f32 + 0.5) / n as f32 * v.len() as f32 - 0.5;
            let i0 = x.floor().clamp(0.0, (v.len() - 1) as f32) as usize;
            let i1 = (i0 + 1).min(v.len() - 1);
            let f = (x - i0 as f32).clamp(0.0, 1.0);
            v[i0] * (1.0 - f) + v[i1] * f
        };
        for i in 0..n {
            let k = b + i;
            let t = at(gas, i);
            let rho = self.p[k] / (self.gas.r * t);
            let u = self.u[k];
            self.rho[k] = rho;
            self.mom[k] = rho * u;
            self.en[k] = rho * (self.gas.e(t) + 0.5 * u * u);
            self.tw[k] = at(wall, i);
        }
        self.tw[b - 1] = self.tw[b];
        self.tw[b + n] = self.tw[b + n - 1];
    }

    /// Gas temperature of a duct's cells, keeping pressure and velocity.
    pub fn set_gas_temperature(&mut self, d: usize, t: f32) {
        let (b, n) = (self.ducts[d].base, self.ducts[d].n);
        for k in b..b + n {
            let rho = self.p[k] / (self.gas.r * t);
            let u = self.u[k];
            self.rho[k] = rho;
            self.mom[k] = rho * u;
            self.en[k] = rho * (self.gas.e(t) + 0.5 * u * u);
        }
    }

    pub fn volume(&self, d: usize) -> f32 {
        let (b, n, dx) = (self.ducts[d].base, self.ducts[d].n, self.ducts[d].dx);
        self.a_c[b..b + n].iter().sum::<f32>() * dx
    }

    pub fn mass(&self, d: usize) -> f32 {
        let (b, n, dx) = (self.ducts[d].base, self.ducts[d].n, self.ducts[d].dx);
        (b..b + n).map(|k| self.rho[k] * self.a_c[k]).sum::<f32>() * dx
    }

    pub fn energy(&self, d: usize) -> f32 {
        let (b, n, dx) = (self.ducts[d].base, self.ducts[d].n, self.ducts[d].dx);
        (b..b + n).map(|k| self.en[k] * self.a_c[k]).sum::<f32>() * dx
    }

    pub fn total_mass(&self) -> f32 {
        (0..self.ducts.len()).map(|d| self.mass(d)).sum()
    }

    /// Mean gas temperature of a duct weighted by mass.
    pub fn mean_temperature(&self, d: usize) -> f32 {
        let (b, n) = (self.ducts[d].base, self.ducts[d].n);
        let (mut s, mut w) = (0.0, 0.0);
        for k in b..b + n {
            let m = self.rho[k] * self.a_c[k];
            s += self.t[k] * m;
            w += m;
        }
        s / w.max(1e-12)
    }

    /// Primitive state (rho, u, p, c, T) of the cell next to `end`, with velocity signed *toward* the
    /// end (out of the duct is positive).
    #[inline]
    pub fn end_state(&self, d: usize, end: End) -> (f32, f32, f32, f32, f32) {
        let di = &self.ducts[d];
        match end {
            End::Left => {
                let k = di.base;
                (self.rho[k], -self.u[k], self.p[k], self.c[k], self.t[k])
            }
            End::Right => {
                let k = di.base + di.n - 1;
                (self.rho[k], self.u[k], self.p[k], self.c[k], self.t[k])
            }
        }
    }

    #[inline]
    pub fn end_inv_area(&self, d: usize, end: End) -> f32 {
        match end {
            End::Left => self.ducts[d].inv_al,
            End::Right => self.ducts[d].inv_ar,
        }
    }

    #[inline]
    pub fn end_area(&self, d: usize, end: End) -> f32 {
        let di = &self.ducts[d];
        match end {
            End::Left => self.a_lo[di.base],
            End::Right => self.a_hi[di.base + di.n - 1],
        }
    }

    /// Hands the grid the flux through one end face (per unit area, +x convention) and the face state
    /// (`u_x` along +x) a node computed there.
    #[inline]
    pub fn set_end(&mut self, d: usize, end: End, flux: [f32; 3], rho: f32, u_x: f32, p: f32) {
        let di = &mut self.ducts[d];
        match end {
            End::Left => {
                di.bl = flux;
                di.gl = [rho, u_x, p];
            }
            End::Right => {
                di.br = flux;
                di.gr = [rho, u_x, p];
            }
        }
    }

    /// Deposits `joules` of heat and `kg` of burned gas into one cell (a pop or backfire).
    pub fn inject(&mut self, d: usize, cell: usize, joules: f32, kg: f32) {
        let k = self.idx(d, cell);
        let vol = 1.0 / self.inv_acdx[k];
        self.rho[k] += kg / vol;
        self.en[k] += joules / vol;
    }

    // ------------------------------------------------------------------ numerics

    /// Refreshes u, p, T, c from the conserved variables and the CFL rate. Call once per step before the nodes.
    pub fn update_primitives(&mut self) {
        self.sanitize_ghosts();
        let m = self.m;
        let (ga, gb, gr) = (self.gas.a, self.gas.b, self.gas.r);
        call!(
            self,
            k_prim,
            k_prim_avx2,
            ga,
            gb,
            gr,
            &mut self.rho[..m],
            &self.mom[..m],
            &self.en[..m],
            &mut self.u[..m],
            &mut self.p[..m],
            &mut self.t[..m],
            &mut self.c[..m]
        );
        self.max_rate = call!(self, k_rate, k_rate_avx2, &self.u[..m], &self.c[..m], &self.inv_dx[..m]);
    }

    /// Ghost cells hold garbage after an update; copy the neighbouring real cell so they stay finite.
    fn sanitize_ghosts(&mut self) {
        for di in &self.ducts {
            let (l, r) = (di.base - 1, di.base + di.n);
            for (g, s) in [(l, di.base), (r, di.base + di.n - 1)] {
                self.rho[g] = self.rho[s];
                self.mom[g] = self.mom[s];
                self.en[g] = self.en[s];
            }
        }
    }

    /// One time step of every duct: reconstruction, fluxes, update, friction and wall heat. The end
    /// fluxes and face states must already be set by the nodes, and [`Grid::update_primitives`] called.
    pub fn advance(&mut self, dt: f32) {
        // ghost primitives from the node face states (mirrored about the face)
        for di in &self.ducts {
            let (l, r) = (di.base - 1, di.base + di.n);
            let (c0, c1) = (di.base, di.base + di.n - 1);
            self.rho[l] = mx(2.0 * di.gl[0] - self.rho[c0], RHO_MIN);
            self.u[l] = 2.0 * di.gl[1] - self.u[c0];
            self.p[l] = mx(2.0 * di.gl[2] - self.p[c0], P_MIN);
            self.rho[r] = mx(2.0 * di.gr[0] - self.rho[c1], RHO_MIN);
            self.u[r] = 2.0 * di.gr[1] - self.u[c1];
            self.p[r] = mx(2.0 * di.gr[2] - self.p[c1], P_MIN);
        }
        let m = self.m;
        let (ga, gb, gr) = (self.gas.a, self.gas.b, self.gas.r);
        let inv_band = 1.0 / (SENSOR_HI - SENSOR_LO);

        // pass 1: reconstruct and evolve face states by half a step (cells 1..m-1)
        call!(
            self,
            k_recon,
            k_recon_avx2,
            0.5 * dt,
            inv_band,
            ga,
            gb,
            gr,
            &self.rho[0..m - 2],
            &self.rho[1..m - 1],
            &self.rho[2..m],
            &self.u[0..m - 2],
            &self.u[1..m - 1],
            &self.u[2..m],
            &self.p[0..m - 2],
            &self.p[1..m - 1],
            &self.p[2..m],
            &self.c[1..m - 1],
            &self.alog[1..m - 1],
            &self.inv_dx[1..m - 1],
            &mut self.rl[1..m - 1],
            &mut self.ul[1..m - 1],
            &mut self.pl[1..m - 1],
            &mut self.el[1..m - 1],
            &mut self.rr[1..m - 1],
            &mut self.ur[1..m - 1],
            &mut self.pr[1..m - 1],
            &mut self.er[1..m - 1]
        );
        // pass 2: HLL flux at every interface j = 2..m-2 (between cell j-1 and cell j)
        call!(
            self,
            k_flux,
            k_flux_avx2,
            &self.rr[1..m - 2],
            &self.ur[1..m - 2],
            &self.pr[1..m - 2],
            &self.er[1..m - 2],
            &self.rl[2..m - 1],
            &self.ul[2..m - 1],
            &self.pl[2..m - 1],
            &self.el[2..m - 1],
            &self.c[1..m - 2],
            &self.c[2..m - 1],
            &mut self.f0[2..m - 1],
            &mut self.f1[2..m - 1],
            &mut self.f2[2..m - 1]
        );
        // boundary fluxes from the nodes
        for di in &self.ducts {
            let (l, r) = (di.base, di.base + di.n);
            self.f0[l] = di.bl[0];
            self.f1[l] = di.bl[1];
            self.f2[l] = di.bl[2];
            self.f0[r] = di.br[0];
            self.f1[r] = di.br[1];
            self.f2[r] = di.br[2];
        }
        // pass 3: update the cells, friction, wall heat
        let dflag = if self.dissipation { 1.0f32 } else { 0.0 };
        call!(
            self,
            k_cells,
            k_cells_avx2,
            dt,
            dflag,
            self.gas.a,
            2.0 * self.gas.b,
            &self.f0[1..m - 1],
            &self.f0[2..m],
            &self.f1[1..m - 1],
            &self.f1[2..m],
            &self.f2[1..m - 1],
            &self.f2[2..m],
            &self.a_lo[1..m - 1],
            &self.a_hi[1..m - 1],
            &self.inv_acdx[1..m - 1],
            &self.pl[1..m - 1],
            &self.pr[1..m - 1],
            &self.u[1..m - 1],
            &self.t[1..m - 1],
            &self.tw[1..m - 1],
            &self.hv[1..m - 1],
            &self.k_ac[1..m - 1],
            &self.k_lam[1..m - 1],
            &self.c_tur[1..m - 1],
            &mut self.rho[1..m - 1],
            &mut self.mom[1..m - 1],
            &mut self.en[1..m - 1]
        );

        self.slow_dt += dt;
        let due = self.step_count % SLOW_EVERY == 0;
        self.step_count = self.step_count.wrapping_add(1);
        if due {
            let sd = self.slow_dt;
            self.slow_dt = 0.0;
            for d in 0..self.ducts.len() {
                let (b, n) = (self.ducts[d].base, self.ducts[d].n);
                self.slow_update_range(b, b + n, sd);
            }
        }
    }

    /// Slow thermal bookkeeping for cells `lo..hi`: heat-transfer coefficient, friction coefficients,
    /// acoustic boundary-layer damping and the wall temperature itself.
    fn slow_update_range(&mut self, lo: usize, hi: usize, dt: f32) {
        let g = self.gas;
        let omega_ref = std::f32::consts::TAU * 250.0;
        let alpha = if dt > 0.0 { 1.0 - (-dt / 0.01).exp() } else { 1.0 };
        for k in lo..hi {
            let t = self.t[k].max(200.0);
            let rho = self.rho[k].max(RHO_MIN);
            let d = self.dh[k];
            let mu = g.viscosity(t);
            self.u_avg[k] += (self.u[k].abs() - self.u_avg[k]) * alpha;
            let ua = self.u_avg[k].max(0.05);
            let re = rho * ua * d / mu;
            let kcond = g.conductivity(t);
            // Dittus-Boelter for turbulent flow, fully developed laminar Nu = 3.66 otherwise
            let nu = if re > 3000.0 { 0.023 * re.powf(0.8) * g.pr.powf(0.4) } else { 3.66 };
            // pulsating flow transfers ~1.5x the steady-flow heat, and chambers keep mixing even when the mean
            // flow through them is slow, so there is a floor
            let h = (1.5 * nu * kcond / d).max(25.0);
            self.hv[k] = h * 4.0 / d;
            // friction: laminar damping rate 32 mu/(rho d^2); turbulent f/(2 d) per unit |u| with f from a
            // 4-norm blend of Blasius and the fully rough limit (within ~5 % of Colebrook)
            let f_tur = (0.01 / re.max(1.0) + self.fr4[k]).sqrt().sqrt();
            self.k_lam[k] = 32.0 * mu / (rho * d * d) * self.fscale[k];
            self.c_tur[k] = f_tur / (2.0 * d) * self.fscale[k];
            // Kirchhoff attenuation of plane waves (viscous + thermal boundary layers) as a damping rate
            let gam = g.gamma(t);
            let att = (2.0 / d) / self.c[k] * (omega_ref * (mu / rho) * 0.5).sqrt() * (1.0 + (gam - 1.0) / g.pr.sqrt());
            self.k_ac[k] = self.c[k] * att * self.fscale[k].min(1.0);
            if dt > 0.0 {
                // wall: gas-side h, outer natural / forced convection + radiation, optional wrap
                let tw = self.tw[k];
                let ta = self.t_amb + self.bay_dt[k];
                let h_rad = self.eps[k] * SIGMA_SB * (tw * tw + ta * ta) * (tw + ta);
                let u_out = 1.0 / (self.rins[k] + 1.0 / (self.h_conv * self.bay_k[k] + h_rad)) * self.outer[k];
                let dtw = (h * (t - tw) - u_out * (tw - ta)) / self.cap[k];
                self.tw[k] = (tw + dt * self.thermal_accel * dtw).clamp(200.0, 1700.0);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Kernels. Free functions over plain slices so every array is a `noalias` argument and the loops
// vectorise; each has an AVX2/FMA twin compiled with the features enabled.

simd_kernel! {
    fn k_prim / k_prim_avx2 (ga: f32, gb: f32, gr: f32, rho: &mut [f32], mom: &[f32], en: &[f32], u: &mut [f32], p: &mut [f32], t: &mut [f32], c: &mut [f32]) {
        let m = rho.len();
        let (mom, en, u, p, t, c) = (&mom[..m], &en[..m], &mut u[..m], &mut p[..m], &mut t[..m], &mut c[..m]);
        for k in 0..m {
            let r = mx(rho[k], RHO_MIN);
            let inv_r = 1.0 / r;
            let uu = mom[k] * inv_r;
            let e = mx(en[k] * inv_r - 0.5 * uu * uu, 1.0e4);
            let tt = 2.0 * e / (ga + (ga * ga + 4.0 * gb * e).sqrt());
            let cv = ga + 2.0 * gb * tt;
            rho[k] = r;
            u[k] = uu;
            t[k] = tt;
            p[k] = mx(r * gr * tt, P_MIN);
            c[k] = (gr * tt * (cv + gr) / cv).sqrt();
        }
    }
}

simd_kernel! {
    fn k_rate / k_rate_avx2 (u: &[f32], c: &[f32], inv_dx: &[f32]) -> f32 {
        let m = u.len();
        let (c, inv_dx) = (&c[..m], &inv_dx[..m]);
        let mut acc = [0.0f32; 8];
        let mut k = 0;
        while k + 8 <= m {
            for l in 0..8 {
                acc[l] = mx(acc[l], (u[k + l].abs() + c[k + l]) * inv_dx[k + l]);
            }
            k += 8;
        }
        let mut rate = acc.iter().fold(0.0f32, |a, &b| mx(a, b));
        while k < m {
            rate = mx(rate, (u[k].abs() + c[k]) * inv_dx[k]);
            k += 1;
        }
        rate
    }
}

simd_kernel! {
    fn k_recon / k_recon_avx2 (
        hdt: f32, inv_band: f32, ga: f32, gb: f32, gr: f32,
        rm: &[f32], rc: &[f32], rf: &[f32], um: &[f32], uc: &[f32], uf: &[f32], pm: &[f32], pc: &[f32], pf: &[f32],
        cs: &[f32], alog: &[f32], inv_dx: &[f32],
        rl: &mut [f32], ul: &mut [f32], pl: &mut [f32], el: &mut [f32], rr: &mut [f32], ur: &mut [f32], pr: &mut [f32], er: &mut [f32]
    ) {
        let n = rc.len();
        let (rm, rf, um, uc, uf, pm, pc, pf) = (&rm[..n], &rf[..n], &um[..n], &uc[..n], &uf[..n], &pm[..n], &pc[..n], &pf[..n]);
        let (cs, alog, inv_dx) = (&cs[..n], &alog[..n], &inv_dx[..n]);
        let (rl, ul, pl, el) = (&mut rl[..n], &mut ul[..n], &mut pl[..n], &mut el[..n]);
        let (rr, ur, pr, er) = (&mut rr[..n], &mut ur[..n], &mut pr[..n], &mut er[..n]);
        for k in 0..n {
            let (r, u, p) = (rc[k], uc[k], pc[k]);
            // smooth regions (sound waves) get the unlimited central slope, which has no numerical damping;
            // a pressure-curvature sensor hands over to the van Leer limiter at fronts
            let sens = (pm[k] - 2.0 * p + pf[k]).abs() / (pm[k] + 2.0 * p + pf[k]);
            let phi = cl((SENSOR_HI - sens) * inv_band, 0.0, 1.0);
            let lim_r = vanleer(r - rm[k], rf[k] - r);
            let sr = lim_r + phi * (0.5 * (rf[k] - rm[k]) - lim_r);
            let lim_u = vanleer(u - um[k], uf[k] - u);
            let su = lim_u + phi * (0.5 * (uf[k] - um[k]) - lim_u);
            let lim_p = vanleer(p - pm[k], pf[k] - p);
            let sp = lim_p + phi * (0.5 * (pf[k] - pm[k]) - lim_p);
            let id = inv_dx[k];
            let (g_r, g_u, g_p) = (sr * id, su * id, sp * id);
            let al = alog[k];
            let inv_r = 1.0 / r;
            let rt = -u * g_r - r * g_u - r * u * al;
            let ut = -u * g_u - g_p * inv_r;
            let pt = -u * g_p - r * cs[k] * cs[k] * (g_u + u * al);
            let (rcen, ucen, pcen) = (r + hdt * rt, u + hdt * ut, p + hdt * pt);
            let (rl_, ul_, pl_) = (mx(rcen - 0.5 * sr, RHO_MIN), ucen - 0.5 * su, mx(pcen - 0.5 * sp, P_MIN));
            let (rr_, ur_, pr_) = (mx(rcen + 0.5 * sr, RHO_MIN), ucen + 0.5 * su, mx(pcen + 0.5 * sp, P_MIN));
            rl[k] = rl_;
            ul[k] = ul_;
            pl[k] = pl_;
            rr[k] = rr_;
            ur[k] = ur_;
            pr[k] = pr_;
            let tl = pl_ / (rl_ * gr);
            let tr = pr_ / (rr_ * gr);
            el[k] = rl_ * tl * (ga + gb * tl) + 0.5 * rl_ * ul_ * ul_;
            er[k] = rr_ * tr * (ga + gb * tr) + 0.5 * rr_ * ur_ * ur_;
        }
    }
}

simd_kernel! {
    fn k_flux / k_flux_avx2 (
        rl_: &[f32], ul_: &[f32], pl_: &[f32], el_: &[f32], rr_: &[f32], ur_: &[f32], pr_: &[f32], er_: &[f32],
        c_left: &[f32], c_right: &[f32],
        f0: &mut [f32], f1: &mut [f32], f2: &mut [f32]
    ) {
        // "l" states are the right faces of cell j-1, "r" states the left faces of cell j
        let n = f0.len();
        let (rl_, ul_, pl_, el_, rr_, ur_, pr_, er_) = (&rl_[..n], &ul_[..n], &pl_[..n], &el_[..n], &rr_[..n], &ur_[..n], &pr_[..n], &er_[..n]);
        let (c_left, c_right) = (&c_left[..n], &c_right[..n]);
        let (f1, f2) = (&mut f1[..n], &mut f2[..n]);
        for j in 0..n {
            let (r_l, u_l, p_l, e_l) = (rl_[j], ul_[j], pl_[j], el_[j]);
            let (r_r, u_r, p_r, e_r) = (rr_[j], ur_[j], pr_[j], er_[j]);
            let cm = mx(c_left[j], c_right[j]) * 1.06;
            let sl = mn(mn(u_l, u_r) - cm, 0.0);
            let sr = mx(mx(u_l, u_r) + cm, 0.0);
            let inv = 1.0 / (sr - sl);
            let k = sl * sr;
            let (fl0, fl1, fl2) = (r_l * u_l, r_l * u_l * u_l + p_l, u_l * (e_l + p_l));
            let (fr0, fr1, fr2) = (r_r * u_r, r_r * u_r * u_r + p_r, u_r * (e_r + p_r));
            f0[j] = (sr * fl0 - sl * fr0 + k * (r_r - r_l)) * inv;
            f1[j] = (sr * fl1 - sl * fr1 + k * (r_r * u_r - r_l * u_l)) * inv;
            f2[j] = (sr * fl2 - sl * fr2 + k * (e_r - e_l)) * inv;
        }
    }
}

simd_kernel! {
    fn k_cells / k_cells_avx2 (
        dt: f32, dflag: f32, ga: f32, gb2: f32,
        f0l: &[f32], f0r: &[f32], f1l: &[f32], f1r: &[f32], f2l: &[f32], f2r: &[f32],
        a_lo: &[f32], a_hi: &[f32], inv_acdx: &[f32], pl: &[f32], pr: &[f32], u: &[f32], t: &[f32],
        tw: &[f32], hv: &[f32], k_ac: &[f32], k_lam: &[f32], c_tur: &[f32],
        rho: &mut [f32], mom: &mut [f32], en: &mut [f32]
    ) {
        let n = rho.len();
        let (f0l, f0r, f1l, f1r, f2l, f2r) = (&f0l[..n], &f0r[..n], &f1l[..n], &f1r[..n], &f2l[..n], &f2r[..n]);
        let (a_lo, a_hi, inv_acdx, pl, pr, u, t) = (&a_lo[..n], &a_hi[..n], &inv_acdx[..n], &pl[..n], &pr[..n], &u[..n], &t[..n]);
        let (tw, hv, k_ac, k_lam, c_tur) = (&tw[..n], &hv[..n], &k_ac[..n], &k_lam[..n], &c_tur[..n]);
        let (mom, en) = (&mut mom[..n], &mut en[..n]);
        for k in 0..n {
            let (al, ar) = (a_lo[k], a_hi[k]);
            let kk = dt * inv_acdx[k];
            let p_star = 0.5 * (pl[k] + pr[k]);
            let r = mx(rho[k] - kk * (ar * f0r[k] - al * f0l[k]), RHO_MIN);
            let mut mo = mom[k] - kk * (ar * f1r[k] - al * f1l[k]) + kk * p_star * (ar - al);
            let mut e = en[k] - kk * (ar * f2r[k] - al * f2l[k]);
            // friction (implicit damping of the momentum; the kinetic energy turns into heat)
            let kf = mx(k_lam[k], c_tur[k] * u[k].abs()) + k_ac[k];
            let x = mn(dt * kf * dflag, 1.0);
            mo *= 1.0 / (1.0 + x);
            // wall heat transfer, limited so a stiff channel cannot overshoot the wall temperature
            let dtemp = tw[k] - t[k];
            let cap = r * (ga + gb2 * t[k]);
            let lim = 0.5 * cap * dtemp.abs();
            e += cl(dt * hv[k] * dtemp * dflag, -lim, lim);
            rho[k] = r;
            mom[k] = mo;
            en[k] = e;
        }
    }
}
