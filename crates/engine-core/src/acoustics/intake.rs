//! The intake as an acoustic network: intake valve, port and runner, plenum, throttle, tube, airbox
//! and snorkel (or an open velocity stack per cylinder on throttle bodies) out to the mouth that
//! radiates the induction noise. Sized from the engine and the player's air filter and manifold.

use super::gas::Gas;
use super::grid::{DuctSpec, End};
use super::material::{PipeMaterial, Wall};
use super::network::Network;
use super::node::{Env, Node, OpenEnd, PipeLeg, SlugLeg, ValveLeg};
use super::reservoir::Reservoir;
use crate::config::EngineConfig;
use crate::tuning::{AirFilter, Runners, Tuning};

pub struct IntakeInputs<'a> {
    pub cfg: &'a EngineConfig,
    pub tuning: &'a Tuning,
    pub dx: f32,
    pub env: Env,
    /// Index of the first chamber reservoir this system owns.
    pub res_base: usize,
}

#[derive(Clone, Debug, Default)]
pub struct IntakeReport {
    pub runner_length: f32,
    pub runner_diameter: f32,
    pub plenum_litres: f32,
    pub cells: usize,
    pub nodes: usize,
    /// Helmholtz resonance of the airbox + snorkel, if there is one.
    pub airbox_hz: Option<f32>,
    pub mouths: usize,
}

pub struct IntakeSystem {
    pub net: Network,
    /// Node of each cylinder's intake port (its valve leg is `valves[0]`).
    pub port_node: Vec<usize>,
    /// Nodes that are open mouths, and their diameters.
    pub mouths: Vec<(usize, f32)>,
    pub chambers: Vec<Reservoir>,
    pub res_base: usize,
    /// Reservoir index of the plenum (None on throttle bodies) and the throttle node.
    pub plenum: Option<usize>,
    throttle_node: Option<usize>,
    /// Throttle bodies: (node, leg index of the runner side, leg index of the stack side, runner/stack area).
    itb: Vec<(usize, f32)>,
    /// Current throttle effective area, m^2, and its limits.
    pub throttle_area: f32,
    a_min: f32,
    a_max: f32,
    plenum_avg: f32,
    pub report: IntakeReport,
}

fn pipe(net: &mut Network, env: Env, name: &str, len: f32, d_in: f32, d_out: f32, wall: Wall, dx: f32) -> usize {
    let spec = DuctSpec::pipe(name, len.max(0.04), d_in, wall, dx).taper(d_out);
    net.add_duct(&spec, env.p_amb, env.t_amb)
}

impl IntakeSystem {
    pub fn build(inp: &IntakeInputs) -> IntakeSystem {
        let cfg = inp.cfg;
        let tun = inp.tuning;
        let n = cfg.cylinder_count;
        let env = inp.env;
        let bore = cfg.bore_meters as f32;
        let disp_l = cfg.displacement_l() as f32;
        let mut net = Network::new(env, Gas::AIR);
        let mut chambers: Vec<Reservoir> = Vec::new();
        let res_base = inp.res_base;
        let chamber = |chambers: &mut Vec<Reservoir>, v: f32| -> usize {
            chambers.push(Reservoir::new(Gas::AIR, v, env.p_amb, env.t_amb));
            res_base + chambers.len() - 1
        };
        let wall = |d: f32, mat: PipeMaterial, t_mm: f32| Wall::pipe(mat, t_mm * 1e-3, false, d);

        let itb = tun.runners == Runners::Itb;
        let (run_len, vol_k) = match tun.runners {
            Runners::LongRunner => (0.44, 1.0),
            Runners::Stock => (0.30, 1.2),
            Runners::ShortRunner => (0.17, 1.7),
            Runners::Itb => (0.14, 0.0),
        };
        let l_port = 0.035 + 0.30 * bore;
        let d_run = 0.46 * bore * if itb { 1.1 } else { 1.0 };
        let d_port = 0.52 * bore;
        let mut report = IntakeReport { runner_length: l_port + run_len, runner_diameter: d_run, ..Default::default() };
        let mut port_node = Vec::with_capacity(n);
        let mut mouths = Vec::new();
        let mut itb_nodes = Vec::new();
        let mut plenum = None;
        let mut throttle_node = None;
        let a_max;
        let plastic = PipeMaterial::Stainless409; // wall properties of an alloy / composite manifold are close enough

        let runners: Vec<usize> = (0..n)
            .map(|i| {
                let w = wall(d_run, plastic, 2.5);
                let d = pipe(&mut net, env, &format!("runner{}", i + 1), l_port + run_len, d_port, d_run, w, inp.dx);
                port_node.push(net.add_node(Node::junction(&format!("iport{}", i + 1)).with_leg(PipeLeg::new(d, End::Left)).with_valve(ValveLeg::new(i))));
                d
            })
            .collect();

        if !itb {
            // ---- plenum manifold: runners open into a plenum volume fed by a throttle body
            let v_pl = (vol_k * disp_l * 1.0e-3).max(0.8e-3);
            let pl = chamber(&mut chambers, v_pl);
            plenum = Some(pl);
            report.plenum_litres = v_pl * 1e3;
            for &r in &runners {
                net.add_node(Node::tank("plenum_in", PipeLeg::new(r, End::Right).with_loss(1.0, 0.25), pl));
            }
            let d_tube = (0.050 + 0.0075 * disp_l).clamp(0.052, 0.090);
            a_max = std::f32::consts::FRAC_PI_4 * d_tube * d_tube * 0.85;
            let wt = wall(d_tube, PipeMaterial::MildSteel, 2.0);
            let tube = pipe(&mut net, env, "intake_tube", 0.32, d_tube, d_tube, wt, inp.dx);
            let mut th = ValveLeg::new(pl);
            th.area = 0.2 * a_max;
            let nd = net.add_node(Node::junction("throttle").with_leg(PipeLeg::new(tube, End::Right).with_loss(0.0, 0.2)).with_valve(th));
            throttle_node = Some(nd);
            // ---- airbox, snorkel and filter, or a bare cone on the tube
            match tun.air_filter {
                AirFilter::Stock => {
                    let v_ab = (0.9e-3 * disp_l + 2.5e-3).clamp(3.0e-3, 9.0e-3);
                    let ab = chamber(&mut chambers, v_ab);
                    let d_sn = d_tube * 1.1;
                    let wsn = wall(d_sn, PipeMaterial::MildSteel, 2.5);
                    let snorkel = pipe(&mut net, env, "snorkel", 0.55, d_sn, d_sn, wsn, inp.dx);
                    let l_neck = 0.03;
                    let a_neck = 1.8 * std::f32::consts::FRAC_PI_4 * d_tube * d_tube;
                    // the airbox: a volume in parallel on the line between snorkel and tube, behind the filter
                    let filt = SlugLeg::new(ab, a_neck, l_neck).with_resistance(30.0);
                    net.add_node(Node::junction("airbox").with_leg(PipeLeg::new(snorkel, End::Right).with_loss(0.0, 0.5)).with_leg(PipeLeg::new(tube, End::Left).with_loss(0.8, 0.0)).with_slug(filt));
                    let area = net.grid.end_area(snorkel, End::Left);
                    let mut oe = OpenEnd::radiating(area);
                    oe.k_in = 1.2;
                    let m = net.add_node(Node::open("intake_mouth", PipeLeg::new(snorkel, End::Left), oe));
                    mouths.push((m, d_sn));
                    report.airbox_hz = Some(343.0 / std::f32::consts::TAU * (a_neck / (v_ab * (l_neck + 0.85 * (a_neck / std::f32::consts::PI).sqrt()))).sqrt());
                }
                AirFilter::ColdAir | AirFilter::OpenStack => {
                    let l = if tun.air_filter == AirFilter::ColdAir { 0.55 } else { 0.12 };
                    let wc = wall(d_tube, PipeMaterial::MildSteel, 1.5);
                    let cone = pipe(&mut net, env, "cold_air_pipe", l, d_tube, d_tube, wc, inp.dx);
                    net.add_node(Node::junction("tube_joint").with_leg(PipeLeg::new(cone, End::Right)).with_leg(PipeLeg::new(tube, End::Left)));
                    let area = net.grid.end_area(cone, End::Left);
                    let mut oe = OpenEnd::radiating(area * 1.0);
                    oe.k_in = if tun.air_filter == AirFilter::ColdAir { 0.9 } else { 0.4 };
                    oe.delta *= if tun.air_filter == AirFilter::OpenStack { 1.4 } else { 1.0 };
                    let m = net.add_node(Node::open("intake_mouth", PipeLeg::new(cone, End::Left), oe));
                    mouths.push((m, d_tube));
                }
            }
        } else {
            // ---- one throttle body per cylinder with its own open velocity stack
            a_max = std::f32::consts::FRAC_PI_4 * d_run * d_run * 0.85;
            for (i, &r) in runners.iter().enumerate() {
                let ws = wall(d_run, PipeMaterial::MildSteel, 1.5);
                let stack = pipe(&mut net, env, &format!("stack{}", i + 1), 0.09, d_run * 1.5, d_run, ws, inp.dx);
                let nd = net.add_node(Node::junction("itb").with_leg(PipeLeg::new(r, End::Right)).with_leg(PipeLeg::new(stack, End::Right)));
                itb_nodes.push((nd, net.grid.end_area(r, End::Right)));
                let area = net.grid.end_area(stack, End::Left);
                let mut oe = OpenEnd::radiating(area);
                oe.k_in = 0.1;
                oe.delta *= 0.9;
                let m = net.add_node(Node::open("stack_mouth", PipeLeg::new(stack, End::Left), oe));
                mouths.push((m, d_run * 1.5));
            }
        }
        report.cells = (0..net.grid.ducts.len()).map(|d| net.grid.n(d)).sum();
        report.nodes = net.nodes.len();
        report.mouths = mouths.len();
        let a_min = 0.012 * a_max.max(1e-4);
        IntakeSystem {
            net,
            port_node,
            mouths,
            chambers,
            res_base,
            plenum,
            throttle_node,
            itb: itb_nodes,
            throttle_area: 0.2 * a_max.max(1e-4),
            a_min,
            a_max: a_max.max(1e-4),
            plenum_avg: env.p_amb,
            report,
        }
    }

    #[inline]
    pub fn set_valve_area(&mut self, cyl: usize, area: f32) {
        self.net.nodes[self.port_node[cyl]].valves[0].area = area;
    }

    /// Drives the throttle so the plenum (or, on throttle bodies, the throttle's flow) holds the
    /// manifold pressure the engine model says it has: the model stays the authority on the mean
    /// state, the network supplies the dynamics around it.
    pub fn control(&mut self, res: &[Reservoir], target_p: f32, dt: f32) {
        let tgt = target_p.clamp(0.12 * 101_325.0, 0.995 * self.net.env.p_amb);
        let p_now = match self.plenum {
            Some(pl) => res[pl].p(),
            None => {
                // throttle bodies: use the mean pressure at the runner side of the throttles
                let (mut s, mut k) = (0.0, 0.0);
                for &(nd, _) in &self.itb {
                    s += self.net.nodes[nd].legs[0].p_face;
                    k += 1.0;
                }
                if k > 0.0 { s / k } else { self.net.env.p_amb }
            }
        };
        self.plenum_avg += (p_now - self.plenum_avg) * (1.0 - (-dt / 0.02).exp());
        let err = (self.plenum_avg - tgt) / tgt;
        // plenum too high: close the throttle; too low: open it
        self.throttle_area = (self.throttle_area * (-12.0 * err * dt * 6.0).exp()).clamp(self.a_min, self.a_max);
        if let Some(nd) = self.throttle_node {
            self.net.nodes[nd].valves[0].area = self.throttle_area;
        }
        if !self.itb.is_empty() {
            let a_each = self.throttle_area * 1.2;
            for &(nd, a_r) in &self.itb {
                let r = (a_r / (0.7 * a_each.max(1e-6))).max(1.0);
                let k = (r - 1.0).powi(2).min(4000.0);
                let node = &mut self.net.nodes[nd];
                node.legs[0].k_in = 0.5 * k;
                node.legs[0].k_out = 0.5 * k;
                node.legs[1].k_in = 0.5 * k;
                node.legs[1].k_out = 0.5 * k;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::engines;

    #[test]
    fn every_engine_gets_an_intake() {
        for cfg in engines() {
            for runners in [Runners::Stock, Runners::Itb, Runners::LongRunner, Runners::ShortRunner] {
                for filter in [AirFilter::Stock, AirFilter::ColdAir, AirFilter::OpenStack] {
                    let t = Tuning { runners, air_filter: filter, ..Tuning::default() };
                    let s = IntakeSystem::build(&IntakeInputs { cfg: &cfg, tuning: &t, dx: 0.04, env: Env::new(101_325.0, 293.0), res_base: cfg.cylinder_count + 10 });
                    assert!(s.report.mouths >= 1, "{} {:?} {:?}", cfg.id, runners, filter);
                    assert_eq!(s.port_node.len(), cfg.cylinder_count);
                    assert!(s.report.cells < 1200, "{}: {} cells", cfg.id, s.report.cells);
                }
            }
        }
    }
}
