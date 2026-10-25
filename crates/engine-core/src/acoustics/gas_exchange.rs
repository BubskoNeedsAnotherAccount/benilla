//! The gas path of a running engine: cylinders that breathe through valves into the exhaust network,
//! advanced together at audio rate.
//!
//! The thermodynamic engine model supplies what it knows - the cylinder pressure when each exhaust
//! valve opens, the trapped charge - and this module does the rest of the gas dynamics: the blow-down
//! through the valve, the piston pumping the residue out, valve overlap, and everything the pipes do to
//! the pulse on its way to the tailpipe.

use super::cylinder_gas::{valve_train, CylGeom, ValveTrain};
use super::exhaust::{ExhaustInputs, ExhaustSystem};
use super::intake::{IntakeInputs, IntakeSystem};
use super::node::Env;
use super::reservoir::Reservoir;
use crate::config::EngineConfig;
use crate::tuning::Tuning;

#[inline(always)]
fn tsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_rdtsc()
    }
    #[cfg(not(target_arch = "x86_64"))]
    0
}

/// What the sound model reads off a tailpipe mouth after every step.
#[derive(Clone, Copy, Debug, Default)]
pub struct MouthSample {
    /// Volume flow out of the pipe, m^3/s, and its rate of change, m^3/s^2.
    pub q: f32,
    pub dq_dt: f32,
    /// Face velocity (out positive), m/s.
    pub u: f32,
}

pub struct GasPath {
    pub cfg: EngineConfig,
    pub valves: ValveTrain,
    pub geom: CylGeom,
    /// Cylinder reservoirs first, then the exhaust chambers, then the intake chambers.
    pub res: Vec<Reservoir>,
    pub exhaust: ExhaustSystem,
    pub intake: IntakeSystem,
    /// Manifold pressure the intake is steered to, Pa.
    pub map_target: f32,
    /// Firing offset of each cylinder (degrees, indexed by label - 1).
    pub offsets: Vec<f32>,
    /// Crank angle, 0..720.
    pub crank: f32,
    prev_local: Vec<f32>,
    /// Burned-gas temperature (K) the cylinder takes when its exhaust valve opens, and the fuel mass that
    /// burned per unit of charge (burn / AFR). The cylinder keeps the mass the intake actually delivered, so
    /// the exhaust flow equals the engine's real flow; pressure follows from the gas law.
    pub t_evo: Vec<f32>,
    pub fuel_frac: Vec<f32>,
    /// Cylinder pressure at the last exhaust-valve opening, Pa (diagnostics).
    pub p_evo: Vec<f32>,
    pub mouths: Vec<MouthSample>,
    /// Intake mouths (induction noise).
    pub intake_mouths: Vec<MouthSample>,
    /// Mean pressure inside each radiating body, Pa.
    pub body_p: Vec<f32>,
    pub env: Env,
    /// Solver sub-steps used by the last call (diagnostics).
    pub substeps: u32,
    /// Cycle counters (diagnostics): cylinders and valves, network, post-step reads.
    pub prof: [u64; 3],
    /// Calls and solver sub-steps so far (diagnostics).
    pub calls: u64,
    pub sub_total: u64,
    /// A pop armed for a cylinder's next exhaust-valve opening: (pressure multiplier, temperature K).
    /// Unburnt charge lighting in the cylinder or the port is a real blow-down with more energy.
    pub pop: Vec<Option<(f32, f32)>>,
    /// Pops fired so far and the multiplier of the latest (the viewer shows them at the tailpipe).
    pub pops_fired: u64,
    pub last_pop_mult: f32,
    /// Cycle-to-cycle scatter of the charge a cylinder blows down (coefficient of variation; combustion is never
    /// the same twice, and worst at idle), and each cylinder's own slowly wandering offset (its breathing, its
    /// injector): without them every exhaust pulse is an exact copy of the last and the spectrum is a razor-sharp
    /// comb, which no engine has.
    pub cyc_cov: f32,
    bias: Vec<f32>,
    bias_sd: f32,
    rng: u32,
}

/// Standard deviation of a cylinder's own offset in blow-down mass, and how fast it wanders (per cycle).
const CYL_BIAS_SD: f32 = 0.03;
const CYL_BIAS_KEEP: f32 = 0.97;

/// How uneven the cylinders of an engine with many of them are, 0 (eight or fewer) .. 1 (sixteen or more): more
/// cylinders mean more runners, valves, injectors and combustion chambers that differ, and a longer, more irregular
/// manifold. Without it a perfectly symmetric sixteen is a bare firing line and its overtones, a whine.
pub fn irregularity(n: usize) -> f32 {
    ((n as f32 - 8.0) / 8.0).clamp(0.0, 1.0)
}

/// Standard deviation (degrees of crank) of the fixed timing error of a cylinder of a fully irregular engine (valve lash
/// and cam tolerances, ignition scatter).
const TIMING_ERR_DEG: f32 = 0.8;

/// Firing offsets (degrees) of each cylinder from the config's firing order.
pub fn firing_offsets(cfg: &EngineConfig) -> Vec<f32> {
    let n = cfg.cylinder_count;
    let mut off = vec![0.0f32; n];
    let mut cum = 0.0;
    for i in 0..n {
        off[cfg.firing_order[i] as usize - 1] = cum as f32;
        cum += cfg.firing_intervals_deg[i] as f32;
    }
    off
}

impl GasPath {
    pub fn new(cfg: &EngineConfig, tuning: &Tuning, banks: &[u8], dx: f32, env: Env) -> GasPath {
        let n = cfg.cylinder_count;
        let valves = valve_train(cfg);
        let geom = CylGeom::new(cfg);
        let exhaust = ExhaustSystem::build(&ExhaustInputs { cfg, tuning, banks, dx, env, res_base: n });
        let offsets = firing_offsets(cfg);
        // each cylinder starts as ambient air in the volume its piston sits at (crank angle 0)
        let mut res: Vec<Reservoir> = (0..n).map(|i| {
            let mut r = Reservoir::new(super::gas::Gas::EXHAUST, geom.volume((0.0f32 - offsets[i]).rem_euclid(720.0)), env.p_amb, env.t_amb);
            r.min_mass = 1.0e-8;
            r
        }).collect();
        res.extend(exhaust.chambers.iter().cloned());
        let intake = IntakeSystem::build(&IntakeInputs { cfg, tuning, dx: dx * 1.1, env, res_base: res.len() });
        res.extend(intake.chambers.iter().cloned());
        let nb = exhaust.bodies.len();
        let nt = exhaust.tails.len();
        let mut gp = GasPath {
            cfg: cfg.clone(),
            valves,
            geom,
            res,
            exhaust,
            map_target: env.p_amb,
            intake_mouths: vec![MouthSample::default(); intake.mouths.len()],
            intake,
            offsets,
            crank: 0.0,
            prev_local: vec![0.0; n],
            p_evo: vec![2.0e5; n],
            t_evo: vec![900.0; n],
            fuel_frac: vec![0.07; n],
            mouths: vec![MouthSample::default(); nt],
            body_p: vec![env.p_amb; nb],
            env,
            substeps: 1,
            prof: [0; 3],
            calls: 0,
            sub_total: 0,
            pop: vec![None; n],
            pops_fired: 0,
            last_pop_mult: 0.0,
            cyc_cov: 0.0,
            bias: vec![0.0; n],
            bias_sd: CYL_BIAS_SD * (1.0 + 1.5 * irregularity(n)),
            rng: 0x9E37_79B9,
        };
        for i in 0..n {
            gp.bias[i] = gp.bias_sd * gp.randn();
            let err = TIMING_ERR_DEG * irregularity(n) * gp.randn();
            gp.offsets[i] += err;
        }
        // the CFL rate of the pipes (which sets the first time step) is known from the start
        gp.exhaust.net.grid.update_primitives();
        gp.intake.net.grid.update_primitives();
        gp
    }

    /// A standard normal deviate (sum of twelve uniforms).
    fn randn(&mut self) -> f32 {
        let mut s = -6.0;
        for _ in 0..12 {
            let mut x = self.rng;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.rng = x;
            s += (x >> 8) as f32 / 16_777_216.0;
        }
        s
    }

    /// Takes over the thermal state and the cylinders' personalities of `other`, a gas path of the same engine and parts
    /// at another cell size: gas and wall temperature of every duct, the muffler chambers, each cylinder's own
    /// offset. A model that replaces another mid-run then sounds like it (resonances follow the sound speed).
    pub fn copy_state_from(&mut self, other: &GasPath) {
        self.exhaust.copy_thermal_from(&other.exhaust);
        if self.intake.net.grid.ducts.len() == other.intake.net.grid.ducts.len() {
            for d in 0..self.intake.net.grid.ducts.len() {
                let (g, w) = other.intake.net.grid.thermal_profile(d);
                self.intake.net.grid.set_thermal_profile(d, &g, &w);
            }
        }
        let n = self.cfg.cylinder_count;
        let nc = self.exhaust.chambers.len().min(other.exhaust.chambers.len());
        for k in 0..nc {
            let t = other.res[n + k].t();
            let r = &mut self.res[n + k];
            r.energy = r.mass * r.gas.e(t);
        }
        if self.bias.len() == other.bias.len() {
            self.bias.copy_from_slice(&other.bias);
        }
        self.pops_fired = other.pops_fired;
    }

    /// Local crank angle of cylinder `i`.
    #[inline]
    pub fn local(&self, i: usize) -> f32 {
        (self.crank - self.offsets[i]).rem_euclid(720.0)
    }

    /// Advances the gas path by `dt` seconds at `rpm`, splitting the step if the CFL limit needs it.
    pub fn step(&mut self, dt: f32, rpm: f32) {
        let sub = self.exhaust.net.substeps_for(dt, 0.95).max(self.intake.net.substeps_for(dt, 0.95));
        self.substeps = sub;
        self.calls += 1;
        self.sub_total += sub as u64;
        let h = dt / sub as f32;
        for _ in 0..sub {
            self.substep(h, rpm);
        }
        let t3 = tsc();
        // sound sources read once per call
        for (k, t) in self.exhaust.tails.iter().enumerate() {
            if let Some(oe) = self.exhaust.net.nodes[t.node].open_end() {
                self.mouths[k] = MouthSample { q: oe.q, dq_dt: (oe.q - oe.q_prev) / h, u: oe.u_face };
            }
        }
        for (k, &(node, _)) in self.intake.mouths.iter().enumerate() {
            if let Some(oe) = self.intake.net.nodes[node].open_end() {
                self.intake_mouths[k] = MouthSample { q: -oe.q, dq_dt: -(oe.q - oe.q_prev) / h, u: oe.u_face };
            }
        }
        for b in 0..self.body_p.len() {
            self.body_p[b] = self.exhaust.body_pressure(b);
        }
        self.prof[2] += tsc() - t3;
    }

    fn substep(&mut self, h: f32, rpm: f32) {
        let t0 = tsc();
        let n = self.cfg.cylinder_count;
        self.crank = (self.crank + rpm * 6.0 * h).rem_euclid(720.0);
        let evo = self.valves.exhaust.open;
        for i in 0..n {
            let local = self.local(i);
            let prev = self.prev_local[i];
            // the exhaust valve just opened: the cylinder holds the burned charge the engine model says
            let crossed = if prev <= local { prev < evo && local >= evo } else { prev < evo || local >= evo };
            self.res[i].set_volume(self.geom.volume(local));
            if crossed {
                // the charge burned: its fuel joins the gas and its temperature jumps; the mass the intake
                // delivered stays, so pressure is whatever the gas law then says
                let wander = self.bias_sd * (1.0 - CYL_BIAS_KEEP * CYL_BIAS_KEEP).sqrt();
                self.bias[i] = CYL_BIAS_KEEP * self.bias[i] + wander * self.randn();
                let scatter = (1.0 + self.cyc_cov * self.randn() + self.bias[i]).clamp(0.6, 1.4);
                let mut mass = self.res[i].mass * (1.0 + self.fuel_frac[i]) * scatter;
                let mut t = self.t_evo[i];
                if let Some((mult, tp)) = self.pop[i].take() {
                    // unburnt charge lighting at the valve: more energy and mass than a normal blow-down
                    self.pops_fired += 1;
                    self.last_pop_mult = mult;
                    mass *= mult;
                    t = tp;
                }
                let r = &mut self.res[i];
                r.mass = mass.max(r.min_mass);
                r.energy = r.mass * r.gas.e(t);
                self.p_evo[i] = r.p();
            }
            self.prev_local[i] = local;
            let area = self.valves.exhaust.area(local);
            self.exhaust.set_valve_area(i, area);
            self.intake.set_valve_area(i, self.valves.intake.area(local));
        }
        // a stopped engine has nothing to hold the manifold below ambient: the pump would otherwise drive a
        // steady draught through whichever cylinder rests in valve overlap, and the solver noise riding on
        // it is an airy hiss
        if rpm > 1.0 {
            self.intake.control(&self.res, self.map_target, h);
        }
        let t1 = tsc();
        self.intake.net.step(h, &mut self.res);
        self.exhaust.net.step(h, &mut self.res);
        let t2 = tsc();
        for r in self.res.iter_mut() {
            r.commit();
        }
        self.prof[0] += t1 - t0;
        self.prof[1] += t2 - t1;
    }

    /// Acoustic power (W) of the turbulent jets of all exhaust valves and of all intake valves at this moment, from
    /// the pressure ratio across each open valve and the mass flow the network gave it (see `sound::flow_noise`).
    pub fn jet_powers(&self) -> (f32, f32) {
        use crate::sound::flow_noise::jet_power;
        let (mut wx, mut wi) = (0.0f32, 0.0f32);
        for c in 0..self.cfg.cylinder_count {
            let r = &self.res[c];
            let nd = &self.exhaust.net.nodes[self.exhaust.port_node[c]];
            let v = &nd.valves[0];
            if v.area > 1.0e-7 && v.mdot.abs() > 1.0e-6 {
                let (pc, pp) = (r.p(), nd.legs[0].p_face);
                // out of the cylinder the jet is burned gas at the cylinder's temperature; back into it, port gas
                wx += if v.mdot > 0.0 { jet_power(pc, pp, r.t(), v.mdot, 1.33, 288.7) } else { jet_power(pp, pc, 800.0, v.mdot, 1.33, 288.7) };
            }
            let nd = &self.intake.net.nodes[self.intake.port_node[c]];
            let v = &nd.valves[0];
            if v.area > 1.0e-7 && v.mdot.abs() > 1.0e-6 {
                let (pc, pp) = (r.p(), nd.legs[0].p_face);
                // the reservoir is the cylinder: flow into it is negative
                wi += if v.mdot < 0.0 { jet_power(pp, pc, self.env.t_amb + 10.0, v.mdot, 1.4, 287.0) } else { jet_power(pc, pp, r.t(), v.mdot, 1.33, 288.7) };
            }
        }
        (wx, wi)
    }

    /// Cylinder `i`'s slowly drifting offset in burn strength (the personality the cycle-to-cycle scatter rides on).
    pub fn cylinder_bias(&self, i: usize) -> f32 {
        self.bias.get(i).copied().unwrap_or(0.0)
    }

    /// Total mass in the cylinders and chambers plus the pipes, kg (for conservation tests).
    pub fn total_mass(&self) -> f32 {
        self.res.iter().map(|r| r.mass).sum::<f32>() + self.exhaust.net.mass()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::engine_by_id;

    fn banks_for(cfg: &EngineConfig) -> Vec<u8> {
        let nb = crate::config::bank_tilts_deg(&cfg.id).len();
        (0..cfg.cylinder_count).map(|l| (l % nb) as u8).collect()
    }

    #[test]
    fn an_idling_v8_pumps_gas_out_of_the_tailpipes_in_pulses() {
        let cfg = engine_by_id("v8").unwrap();
        let env = Env::new(101_325.0, 293.0);
        let mut gp = GasPath::new(&cfg, &Tuning::default(), &banks_for(&cfg), 0.035, env);
        gp.exhaust.preheat(750.0);
        for t in gp.t_evo.iter_mut() {
            *t = 1050.0;
        }
        let dt = 1.0 / 44_100.0;
        let mut q = Vec::new();
        for _ in 0..44_100 {
            gp.step(dt, 800.0);
            q.push(gp.mouths.iter().map(|m| m.q).sum::<f32>());
        }
        assert!(q.iter().all(|v| v.is_finite()));
        for n in &gp.exhaust.net.nodes {
            if n.fallbacks > 0 {
                println!("  fallbacks in node {}: {} of {} solves; first: {}", n.name, n.fallbacks, n.solves, n.last_fail);
            }
        }
        let tail = &q[22_050..];
        let mean = tail.iter().sum::<f32>() / tail.len() as f32;
        let peak = tail.iter().cloned().fold(f32::MIN, f32::max);
        let trough = tail.iter().cloned().fold(f32::MAX, f32::min);
        println!("tail flow: mean {:.4} m3/s, peak {:.4}, trough {:.4}; substeps {}; fallbacks {}", mean, peak, trough, gp.substeps, gp.exhaust.net.fallbacks());
        // gas leaves on average, and the flow pulses at the firing rate
        assert!(mean > 0.0, "net flow must be out of the pipe");
        assert!(peak > 2.0 * mean, "flow should be pulsed: peak {peak} vs mean {mean}");
        let solves: u64 = gp.exhaust.net.nodes.iter().map(|n| n.solves).sum();
        assert!((gp.exhaust.net.fallbacks() as f64) < 1.0e-3 * solves as f64, "Newton fallbacks {} of {solves}", gp.exhaust.net.fallbacks());
    }
}

#[cfg(test)]
mod header_tuning {
    use crate::config::engine_by_id;
    use crate::sound::{default_banks, EngineSound, Quality, SoundInput};
    use crate::tuning::{Header, Tuning};

    /// Mean pressure at the exhaust valve of cylinder 1 during the valve overlap, at WOT, in bar.
    fn overlap_pressure(id: &str, plen: f32, rpm: f32) -> f32 {
        let cfg = engine_by_id(id).unwrap();
        let tuning = Tuning { header: Header::LongTube, primary_len_scale: plen as f64, crossover: crate::tuning::Crossover::None, ..Tuning::default() };
        let mut snd = EngineSound::new(&cfg, &tuning, &default_banks(&cfg), Quality::BALANCED);
        let inp = SoundInput {
            rpm, throttle: 1.0, load: 1.0, map_pa: 95_000.0, atm_pa: 101_325.0, air_k: 293.0, charge_k: 305.0, afr: 12.8, egt_k: 950.0,
            running: true, lit: true, burn: vec![1.0; cfg.cylinder_count], master: 1.0, serial: 1, ..Default::default()
        };
        snd.preheat(900.0);
        snd.set_input(&inp);
        let dt = 1.0 / 44_100.0;
        let cycle_s = 120.0 / rpm;
        let warm = (8.0 * cycle_s / dt) as usize;
        let meas = (14.0 * cycle_s / dt) as usize;
        let ov = snd.gas.valves;
        let (mut acc, mut n) = (0.0f64, 0usize);
        for i in 0..warm + meas {
            snd.next_sample();
            if i >= warm {
                for c in 0..cfg.cylinder_count {
                    let local = snd.gas.local(c);
                    let open = ov.intake.open;
                    if local >= open - 12.0 || local < ov.exhaust.close() + 4.0 {
                        let d = snd.gas.exhaust.net.nodes[snd.gas.exhaust.port_node[c]].legs[0].duct;
                        acc += snd.gas.exhaust.net.grid.p_at(d, 0) as f64;
                        n += 1;
                    }
                }
            }
        }
        (acc / n.max(1) as f64 / 1e5) as f32
    }

    #[test]
    #[ignore]
    fn overlap_pressure_vs_rpm_and_primary_length() {
        let cfg = engine_by_id("i4").unwrap();
        let evo = crate::acoustics::valve_train(&cfg).exhaust.open;
        let ed = 180.0 + (540.0 - evo as f64);
        for plen in [0.6f32, 0.8, 1.0, 1.3] {
            let sys = EngineSound::new(&cfg, &Tuning { header: Header::LongTube, primary_len_scale: plen as f64, ..Tuning::default() }, &default_banks(&cfg), Quality::BALANCED);
            let l = sys.gas.exhaust.report.primary_length as f64;
            let formula = 850.0 * ed / (l / 0.0254 + 3.0);
            let mut line = String::new();
            let (mut best, mut best_rpm) = (f32::MAX, 0.0);
            for rpm in (0..14).map(|k| 2000.0f32 + 500.0 * k as f32) {
                let p = overlap_pressure("i4", plen, rpm);
                line += &format!(" {:.0}:{:.0}", rpm / 100.0, (p - 1.0) * 100.0);
                if p < best {
                    best = p;
                    best_rpm = rpm;
                }
            }
            println!("TUNING primary {l:.2} m (ED {ed:.0}): formula tunes to {formula:.0} rpm; lowest overlap pressure at {best_rpm:.0} rpm |{line}");
        }
    }
}
