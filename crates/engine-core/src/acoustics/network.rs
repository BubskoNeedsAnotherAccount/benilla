//! A network of ducts and nodes stepped together.

use super::gas::Gas;
use super::grid::{DuctSpec, Grid};
use super::node::{Env, Node};
use super::reservoir::Reservoir;

/// The most a single step may take of a cell's CFL limit before it is split (the schemes are stable to 1).
const CFL_HARD: f32 = 0.97;

#[derive(Clone, Debug)]
pub struct Network {
    pub grid: Grid,
    pub nodes: Vec<Node>,
    pub env: Env,
    /// Cycle counters (diagnostics): primitives, valve nodes, other nodes, advance.
    pub prof: [u64; 4],
}

#[inline(always)]
fn tsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_rdtsc()
    }
    #[cfg(not(target_arch = "x86_64"))]
    0
}

impl Network {
    pub fn new(env: Env, gas: Gas) -> Self {
        Network { grid: Grid::new(gas, env.t_amb), nodes: Vec::new(), env, prof: [0; 4] }
    }

    pub fn add_duct(&mut self, spec: &DuctSpec, p: f32, t: f32) -> usize {
        self.grid.add_duct(spec, p, t)
    }

    pub fn add_node(&mut self, n: Node) -> usize {
        self.nodes.push(n);
        self.nodes.len() - 1
    }

    /// How many sub-steps a step of `dt` needs to stay under `cfl`.
    pub fn substeps_for(&self, dt: f32, cfl: f32) -> u32 {
        ((dt * self.grid.max_rate / cfl).ceil() as u32).clamp(1, 8)
    }

    /// Advances every duct and node by `dt`. Reservoir flows accumulate in `res` (the caller commits
    /// them, because a cylinder is shared between the intake and exhaust networks).
    ///
    /// The caller picks `dt` from the CFL rate of the *previous* step; a blow-down can speed the gas up faster
    /// than that, so the step checks itself against the state it is about to advance and splits if it must.
    pub fn step(&mut self, dt: f32, res: &mut [Reservoir]) {
        let t0 = tsc();
        self.grid.update_primitives();
        self.prof[0] += tsc() - t0;
        let parts = ((dt * self.grid.max_rate / CFL_HARD).ceil() as u32).clamp(1, 6);
        if parts == 1 {
            self.step_primed(dt, res);
        } else {
            let h = dt / parts as f32;
            self.step_primed(h, res);
            for _ in 1..parts {
                self.grid.update_primitives();
                self.step_primed(h, res);
            }
        }
    }

    /// One step whose primitives are already up to date.
    fn step_primed(&mut self, dt: f32, res: &mut [Reservoir]) {
        let (mut tv, mut tn) = (0u64, 0u64);
        for n in self.nodes.iter_mut() {
            let a = tsc();
            n.solve(&mut self.grid, res, dt, &self.env);
            let d = tsc() - a;
            n.cycles += d;
            if n.valves.is_empty() {
                tn += d;
            } else {
                tv += d;
            }
        }
        let t2 = tsc();
        self.grid.advance(dt);
        let t3 = tsc();
        self.prof[1] += tv;
        self.prof[2] += tn;
        self.prof[3] += t3 - t2;
    }

    pub fn mass(&self) -> f32 {
        self.grid.total_mass()
    }

    /// Mean Newton iterations per junction solve.
    pub fn mean_iterations(&self) -> f32 {
        let (i, s) = self.nodes.iter().fold((0u64, 0u64), |a, n| (a.0 + n.iters, a.1 + n.solves));
        i as f32 / s.max(1) as f32
    }

    pub fn fallbacks(&self) -> u32 {
        self.nodes.iter().map(|n| n.fallbacks).sum()
    }
}
