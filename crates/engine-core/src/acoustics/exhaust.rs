//! Builds the exhaust system of an engine as an acoustic network: ports and primaries (or a log
//! manifold), collectors, turbine, downpipe, catalyst, crossover, resonator, muffler and tailpipe,
//! sized from the engine and the player's parts.
//!
//! Every dimension is in metres and follows from the engine (bore, displacement, cylinders per bank) and
//! the [`Tuning`]: header style and primary length / diameter, pipe diameter and material, wall gauge,
//! mid- and tailpipe length, catalyst, muffler, crossover, turbos.

use super::gas::Gas;
use super::grid::{DuctSpec, End};
use super::material::{PipeMaterial, Wall};
use super::network::Network;
use super::node::{Env, Node, OpenEnd, PipeLeg, SlugLeg, ValveLeg};
use super::reservoir::Reservoir;
use crate::config::{EngineConfig, IgnitionType};
use crate::tuning::{Cat, Crossover, Header, Muffler, Tuning};

/// A tailpipe mouth.
#[derive(Clone, Debug)]
pub struct Tail {
    pub node: usize,
    pub duct: usize,
    pub diameter: f32,
}

/// A radiating shell: a group of ducts whose wall vibrates with the pressure inside.
#[derive(Clone, Debug)]
pub struct Body {
    pub name: String,
    pub ducts: Vec<usize>,
    /// Radiating surface, m^2, and mean diameter, m.
    pub surface: f32,
    pub diameter: f32,
    pub wall: Wall,
}

/// Summary numbers for the UI and the validation reports.
#[derive(Clone, Debug, Default)]
pub struct SystemReport {
    /// Path length from the exhaust valve through the longest route to the tailpipe mouth, m.
    pub path_length: f32,
    pub primary_length: f32,
    pub primary_diameter: f32,
    pub main_diameter: f32,
    pub tail_length: f32,
    pub sides: usize,
    pub tails: usize,
    pub cells: usize,
    pub nodes: usize,
    pub header_material: &'static str,
    pub pipe_material: &'static str,
    /// Helmholtz frequency of the stock resonator, if fitted.
    pub resonator_hz: Option<f32>,
}

pub struct ExhaustSystem {
    pub net: Network,
    /// Node of each cylinder's exhaust port (its valve leg is `valves[0]`).
    pub port_node: Vec<usize>,
    pub tails: Vec<Tail>,
    pub bodies: Vec<Body>,
    /// Chamber reservoirs (muffler volumes); their indices start at `res_base`.
    pub chambers: Vec<Reservoir>,
    pub res_base: usize,
    /// Nodes that are turbines: their `extract` follows the shaft speed.
    pub turbines: Vec<usize>,
    pub report: SystemReport,
    /// Distance of each duct's midpoint from the exhaust valves along the gas path, m.
    pub dist: Vec<f32>,
}

pub struct ExhaustInputs<'a> {
    pub cfg: &'a EngineConfig,
    pub tuning: &'a Tuning,
    /// Bank of every cylinder, indexed by label - 1.
    pub banks: &'a [u8],
    /// Cell size, m.
    pub dx: f32,
    pub env: Env,
    /// Index of the first chamber reservoir (after the cylinders).
    pub res_base: usize,
}

/// Loss coefficients for a leg of area `a` meeting a node whose largest leg has area `a_ref`:
/// an expansion (flow out of the small leg into the node) loses `(1 - r)^2` of the small leg's dynamic
/// head, a contraction (flow from the node into the small leg) `0.5 (1 - r)^0.75`.
fn step_loss(a: f32, a_ref: f32, scale: f32) -> (f32, f32) {
    let r = (a / a_ref).min(1.0);
    (
        0.9 * (1.0 - r).powi(2) * scale,
        0.5 * (1.0 - r).powf(0.75) * scale,
    )
}

struct Builder<'a> {
    net: Network,
    chambers: Vec<Reservoir>,
    res_base: usize,
    bodies: Vec<Body>,
    dx: f32,
    wall_scale: f32,
    env: Env,
    _cfg: std::marker::PhantomData<&'a EngineConfig>,
}

impl Builder<'_> {
    fn wall(&self, mat: PipeMaterial, base_mm: Option<f32>, wrapped: bool, dia: f32) -> Wall {
        let t = base_mm
            .map(|m| m * 1e-3)
            .unwrap_or_else(|| mat.props().wall_m)
            * self.wall_scale;
        Wall::pipe(mat, t, wrapped, dia)
    }

    fn duct(&mut self, name: &str, len: f32, d_in: f32, d_out: f32, wall: Wall) -> usize {
        // hot gas is fast, so its cells can be longer for the same CFL number; the cool tail gets finer ones
        let hot = [
            "primary",
            "collector",
            "log",
            "pair",
            "turbo",
            "down",
            "cat",
        ]
        .iter()
        .any(|p| name.starts_with(p));
        let cool = ["mu_", "tail", "hpipe"].iter().any(|p| name.starts_with(p));
        let dx = self.dx
            * if hot {
                1.3
            } else if cool {
                0.85
            } else {
                1.0
            };
        let mut spec = DuctSpec::pipe(name, len.max(0.04), d_in, wall, dx).taper(d_out);
        // headers, collectors, logs and the turbo sit in the engine bay
        spec.engine_bay = ["primary", "collector", "log", "pair", "turbo"]
            .iter()
            .any(|p| name.starts_with(p));
        self.net.add_duct(&spec, self.env.p_amb, self.env.t_amb)
    }

    fn area_of(&self, d: usize, e: End) -> f32 {
        self.net.grid.end_area(d, e)
    }

    /// Joins duct ends at one node with step losses derived from the area ratios.
    fn join(&mut self, name: &str, legs: &[(usize, End)], gentle: f32) -> usize {
        let a_ref = legs
            .iter()
            .map(|&(d, e)| self.area_of(d, e))
            .fold(0.0f32, f32::max);
        let mut n = Node::junction(name);
        for &(d, e) in legs {
            let (ki, ko) = step_loss(self.area_of(d, e), a_ref, gentle);
            n = n.with_leg(PipeLeg::new(d, e).with_loss(ki, ko));
        }
        self.net.add_node(n)
    }

    fn chamber(&mut self, volume: f32) -> usize {
        self.chambers.push(Reservoir::new(
            Gas::EXHAUST,
            volume,
            self.env.p_amb,
            self.env.t_amb,
        ));
        self.res_base + self.chambers.len() - 1
    }

    fn body(&mut self, name: &str, ducts: Vec<usize>, dia: f32, wall: Wall) {
        let surface: f32 = ducts
            .iter()
            .map(|&d| std::f32::consts::PI * dia * self.net.grid.ducts[d].length)
            .sum();
        self.bodies.push(Body {
            name: name.to_string(),
            ducts,
            surface,
            diameter: dia,
            wall,
        });
    }
}

impl ExhaustSystem {
    pub fn build(inp: &ExhaustInputs) -> ExhaustSystem {
        let cfg = inp.cfg;
        let tun = inp.tuning;
        let n = cfg.cylinder_count;
        let diesel = cfg.ignition_type == IgnitionType::Compression;
        let disp_l = cfg.displacement_l() as f32;
        let bore = cfg.bore_meters as f32;
        let mut b = Builder {
            net: Network::new(inp.env, Gas::EXHAUST),
            chambers: Vec::new(),
            res_base: inp.res_base,
            bodies: Vec::new(),
            dx: inp.dx,
            wall_scale: tun.wall_scale.clamp(0.4, 2.5) as f32,
            env: inp.env,
            _cfg: std::marker::PhantomData,
        };
        let mut report = SystemReport {
            header_material: "",
            pipe_material: tun.pipe_material.label(),
            ..Default::default()
        };

        // ------------------------------------------------------------------ primaries
        let header_mat = tun
            .header_material
            .unwrap_or_else(|| tun.header.natural_material());
        report.header_material = header_mat.label();
        let (dia_k, coll_len, log) = match tun.header {
            Header::Stock => (0.40, 0.10, true),
            Header::Shorty => (0.43, 0.16, false),
            Header::LongTube => (0.45, 0.30, false),
        };
        let l_port = Tuning::port_length(cfg) as f32;
        let l_prim = (tun.header_base_length(cfg) as f32 * tun.primary_len_scale as f32).max(0.06);
        let d_prim = dia_k * bore * tun.primary_dia_scale as f32;
        let d_port = (0.36 * bore).min(d_prim * 1.05);
        let h_wall = |b: &Builder| {
            let t_mm = if header_mat == PipeMaterial::CastIron {
                4.0
            } else {
                header_mat.props().wall_m * 1e3
            };
            b.wall(
                header_mat,
                Some(t_mm),
                tun.header_wrap && header_mat != PipeMaterial::CastIron,
                d_prim,
            )
        };
        let nb = inp.banks.iter().map(|&x| x as usize).max().unwrap_or(0) + 1;
        let mut prim = Vec::with_capacity(n);
        let mut port_node = Vec::with_capacity(n);
        for i in 0..n {
            let w = h_wall(&b);
            let d = b.duct(
                &format!("primary{}", i + 1),
                l_port + l_prim,
                d_port,
                d_prim,
                w,
            );
            prim.push(d);
            let node = b.net.add_node(
                Node::junction(&format!("port{}", i + 1))
                    .with_leg(PipeLeg::new(d, End::Left))
                    .with_valve(ValveLeg::new(i)),
            );
            port_node.push(node);
        }
        report.primary_length = l_port + l_prim;
        report.primary_diameter = d_prim;
        b.body("headers", prim.clone(), d_prim, h_wall(&b));

        // ------------------------------------------------------------------ collectors, one per bank
        let mut streams: Vec<usize> = Vec::new(); // duct whose right end carries each bank's exhaust
        let mut coll_ducts = Vec::new();
        let pitch = (1.22 * bore).max(0.06);
        for bk in 0..nb {
            let cyls: Vec<usize> = (0..n).filter(|&i| inp.banks[i] as usize == bk).collect();
            if cyls.is_empty() {
                continue;
            }
            let m = cyls.len();
            let a_prim: f32 = std::f32::consts::FRAC_PI_4 * d_prim * d_prim;
            let d_coll = (4.0 * a_prim * (m as f32 * if log { 0.9 } else { 0.75 }).min(5.0)
                / std::f32::consts::PI)
                .sqrt()
                .max(d_prim * 1.15);
            let w = h_wall(&b);
            let coll = b.duct(&format!("collector{bk}"), coll_len, d_coll, d_coll, w);
            coll_ducts.push(coll);
            if log && m > 1 {
                // a cast log: segments between the entries, each runner joining the log at its own port
                let d_log = d_prim * 1.45;
                let mut segs = Vec::new();
                for j in 0..m - 1 {
                    let w = h_wall(&b);
                    segs.push(b.duct(&format!("log{bk}_{j}"), pitch, d_log, d_log, w));
                    coll_ducts.push(*segs.last().unwrap());
                }
                for (j, &c) in cyls.iter().enumerate() {
                    let mut legs = vec![(prim[c], End::Right)];
                    if j > 0 {
                        legs.push((segs[j - 1], End::Right));
                    }
                    legs.push(if j + 1 < m {
                        (segs[j], End::Left)
                    } else {
                        (coll, End::Left)
                    });
                    b.join(&format!("entry{}", c + 1), &legs, 0.5);
                }
            } else {
                let mut legs: Vec<(usize, End)> =
                    cyls.iter().map(|&c| (prim[c], End::Right)).collect();
                if legs.len() > 6 {
                    // eight-into-one is really 4-2-1: pair the primaries first
                    let mut pairs = Vec::new();
                    for ch in legs.chunks(2) {
                        if ch.len() == 2 {
                            let w = h_wall(&b);
                            let d =
                                b.duct(&format!("pair{bk}"), 0.18, d_prim * 1.3, d_prim * 1.3, w);
                            coll_ducts.push(d);
                            let mut l2 = ch.to_vec();
                            l2.push((d, End::Left));
                            b.join("pair", &l2, 1.0);
                            pairs.push((d, End::Right));
                        } else {
                            pairs.push(ch[0]);
                        }
                    }
                    legs = pairs;
                }
                legs.push((coll, End::Left));
                b.join(&format!("collector{bk}"), &legs, 1.0);
            }
            streams.push(coll);
        }
        let col_wall = h_wall(&b);
        b.body("collectors", coll_ducts, d_prim * 1.8, col_wall);

        // ------------------------------------------------------------------ main pipe size
        let sides_wanted = if streams.len() >= 2 { 2 } else { 1 };
        let disp_side = disp_l / sides_wanted as f32;
        let d_main = (0.040 + 0.0085 * disp_side).clamp(0.045, 0.110)
            * tun.pipe_scale.clamp(0.7, 1.5) as f32;
        report.main_diameter = d_main;
        let pmat = tun.pipe_material;
        let a_main = std::f32::consts::FRAC_PI_4 * d_main * d_main;

        // ------------------------------------------------------------------ turbines
        let n_turbo = tun.turbo_count_for(cfg) as usize;
        let mut turbines = Vec::new();
        if n_turbo > 0 {
            let groups = n_turbo.min(streams.len()).max(1);
            let mut grouped: Vec<Vec<usize>> = vec![Vec::new(); groups];
            for (i, &s) in streams.iter().enumerate() {
                grouped[i % groups].push(s);
            }
            let per_group = (n_turbo as f32 / groups as f32).max(1.0);
            let cap = per_group * tun.turbo_size.clamp(0.6, 1.6) as f32;
            let k_turb = 4.0 / (cap * cap);
            let mut next = Vec::new();
            for (gi, g) in grouped.into_iter().enumerate() {
                let d_in = (d_main * 1.25).max(0.05);
                let w = b.wall(PipeMaterial::Inconel, Some(2.0), false, d_in);
                let mut src = g[0];
                if g.len() > 1 {
                    let hub = b.duct(&format!("turbo_hub{gi}"), 0.14, d_in, d_in, w);
                    let mut legs: Vec<(usize, End)> = g.iter().map(|&s| (s, End::Right)).collect();
                    legs.push((hub, End::Left));
                    b.join("turbo_merge", &legs, 1.0);
                    src = hub;
                }
                let w1 = b.wall(PipeMaterial::Inconel, Some(2.0), false, d_in);
                let inlet = b.duct(&format!("turbine_in{gi}"), 0.10, d_in, d_in, w1);
                b.join("to_turbine", &[(src, End::Right), (inlet, End::Left)], 1.0);
                let w2 = b.wall(PipeMaterial::Inconel, Some(2.0), false, d_main * 1.1);
                let outlet = b.duct(
                    &format!("turbine_out{gi}"),
                    0.14,
                    d_main * 1.15,
                    d_main * 1.1,
                    w2,
                );
                let nd = Node::junction("turbine")
                    .with_leg(PipeLeg::new(inlet, End::Right).with_loss(k_turb * 0.5, 0.0))
                    .with_leg(PipeLeg::new(outlet, End::Left).with_loss(0.0, k_turb * 0.5));
                turbines.push(b.net.add_node(nd));
                next.push(outlet);
            }
            streams = next;
        }

        // ------------------------------------------------------------------ sides: downpipe, cat, mid pipe, muffler, tail
        let sides = if streams.len() >= 2 { 2 } else { 1 };
        report.sides = sides;
        let mut side_src: Vec<Vec<usize>> = vec![Vec::new(); sides];
        for (i, &s) in streams.iter().enumerate() {
            let side = if streams.len() >= 4 {
                i * sides / streams.len()
            } else {
                i % sides
            };
            side_src[side].push(s);
        }
        let l_down = 0.42 * (1.0 + 0.1 * (tun.mid_len_scale as f32 - 1.0));
        let l_mid = 1.15 * tun.mid_len_scale as f32;
        let l_tail = 0.55 * tun.tail_len_scale as f32;
        report.tail_length = l_tail;
        let mut mids: Vec<(usize, usize)> = Vec::new(); // per side: the two halves of the mid pipe
        let mut tails_out: Vec<Tail> = Vec::new();
        let mut resonator_hz = None;
        let mut side_len = vec![0.0f32; sides];
        for (si, srcs) in side_src.iter().enumerate() {
            // merge several collector streams into one downpipe
            let w = b.wall(pmat, Some(1.6), false, d_main);
            let down = b.duct(&format!("downpipe{si}"), l_down, d_main, d_main, w);
            let mut legs: Vec<(usize, End)> = srcs.iter().map(|&s| (s, End::Right)).collect();
            legs.push((down, End::Left));
            b.join(&format!("to_downpipe{si}"), &legs, 1.0);
            let mut ducts_body = vec![down];
            let mut tail_of_path = (down, End::Right);

            // catalyst
            if tun.cat != Cat::Delete {
                let (can_k, phi, ch_mm, len) = match tun.cat {
                    Cat::Stock => (1.9, 0.72, 1.0, 0.15),
                    _ => (2.0, 0.86, 1.5, 0.12),
                };
                let d_can = (can_k * d_main).min(0.16);
                let wc = b.wall(pmat, Some(1.2), false, d_main);
                let cone_in = b.duct(&format!("cat_in{si}"), 0.09, d_main, d_can, wc);
                let wm = Wall::catalyst_substrate(ch_mm * 1e-3, 0.1e-3, pmat);
                let mut spec = DuctSpec::pipe(&format!("cat{si}"), len, d_can, wm, b.dx);
                spec.area_scale = phi;
                spec.hyd_dia = Some(ch_mm * 1e-3);
                let mono = b.net.add_duct(&spec, inp.env.p_amb, inp.env.t_amb);
                let wc2 = b.wall(pmat, Some(1.2), false, d_main);
                let cone_out = b.duct(&format!("cat_out{si}"), 0.09, d_can, d_main, wc2);
                b.join(
                    "cat_a",
                    &[(tail_of_path.0, tail_of_path.1), (cone_in, End::Left)],
                    0.4,
                );
                // flow enters and leaves the monolith through its channel mouths: contraction in, expansion out
                let a_open = b.area_of(mono, End::Left);
                let a_can = std::f32::consts::FRAC_PI_4 * d_can * d_can;
                let k_in = 0.5 * (1.0 - a_open / a_can).powf(0.75);
                let k_out = (1.0 - a_open / a_can).powi(2);
                let n1 = Node::junction("cat_in")
                    .with_leg(PipeLeg::new(cone_in, End::Right).with_loss(0.0, 0.0))
                    .with_leg(PipeLeg::new(mono, End::Left).with_loss(k_out * 0.2, k_in));
                b.net.add_node(n1);
                let n2 = Node::junction("cat_out")
                    .with_leg(PipeLeg::new(mono, End::Right).with_loss(k_out, k_in * 0.2))
                    .with_leg(PipeLeg::new(cone_out, End::Left));
                b.net.add_node(n2);
                ducts_body.extend([cone_in, cone_out]);
                let wcan = b.wall(pmat, Some(1.2), false, d_can);
                b.body(
                    &format!("cat{si}"),
                    vec![cone_in, mono, cone_out],
                    d_can,
                    wcan,
                );
                tail_of_path = (cone_out, End::Right);
            }

            // mid pipe, in two halves so a crossover (and a resonator) can sit between them
            let wmid = b.wall(pmat, Some(1.6), false, d_main);
            let wmid2 = b.wall(pmat, Some(1.6), false, d_main);
            let mid1 = b.duct(&format!("mid{si}a"), 0.5 * l_mid, d_main, d_main, wmid);
            let mid2 = b.duct(&format!("mid{si}b"), 0.5 * l_mid, d_main, d_main, wmid2);
            b.join(
                &format!("to_mid{si}"),
                &[(tail_of_path.0, tail_of_path.1), (mid1, End::Left)],
                0.5,
            );
            ducts_body.extend([mid1, mid2]);
            mids.push((mid1, mid2));
            side_len[si] = ducts_body
                .iter()
                .map(|&d| b.net.grid.ducts[d].length)
                .sum::<f32>();
            let wb = b.wall(pmat, Some(1.6), false, d_main);
            b.body(&format!("pipes{si}"), ducts_body, d_main, wb);
        }

        // junctions between the mid halves: crossover and/or stock resonator
        let stock_res = tun.muffler == Muffler::Stock && !diesel;
        let f_res_target =
            (cfg.redline_rpm as f32 * 0.30 / 60.0 * n as f32 * 0.5).clamp(55.0, 260.0);
        let cross = if sides == 2 {
            tun.crossover
        } else {
            Crossover::None
        };
        match cross {
            Crossover::XPipe if sides == 2 => {
                b.join(
                    "xpipe",
                    &[
                        (mids[0].0, End::Right),
                        (mids[1].0, End::Right),
                        (mids[0].1, End::Left),
                        (mids[1].1, End::Left),
                    ],
                    0.3,
                );
            }
            _ => {
                let h = if cross == Crossover::HPipe && sides == 2 {
                    let wh = b.wall(pmat, Some(1.5), false, 0.8 * d_main);
                    Some(b.duct("hpipe", 0.50, 0.85 * d_main, 0.85 * d_main, wh))
                } else {
                    None
                };
                for si in 0..sides {
                    let mut node = Node::junction(&format!("mid_joint{si}"))
                        .with_leg(PipeLeg::new(mids[si].0, End::Right))
                        .with_leg(PipeLeg::new(mids[si].1, End::Left));
                    if let Some(hp) = h {
                        node = node.with_leg(
                            PipeLeg::new(hp, if si == 0 { End::Left } else { End::Right })
                                .with_loss(0.3, 0.3),
                        );
                    }
                    if stock_res {
                        // Helmholtz resonator tuned to the drone band
                        let c = 470.0;
                        let v = (1.0e-3 * disp_side).clamp(0.8e-3, 3.5e-3);
                        let l_eff = 0.05f32;
                        let a_n = (f_res_target * std::f32::consts::TAU / c).powi(2) * v * l_eff;
                        let a_n = a_n.clamp(1.0e-4, 0.5 * a_main);
                        resonator_hz = Some(c / std::f32::consts::TAU * (a_n / (v * l_eff)).sqrt());
                        let ci = b.chamber(v);
                        node = node.with_slug(SlugLeg::new(ci, a_n, l_eff));
                    }
                    b.net.add_node(node);
                }
                if let Some(hp) = h {
                    let _ = hp;
                    b.body(
                        "hpipe",
                        vec![hp],
                        0.85 * d_main,
                        b.wall(pmat, Some(1.5), false, 0.85 * d_main),
                    );
                }
            }
        }

        // ---- muffler and tail on each side
        let v_can = 4.5e-3 * disp_side; // m^3
        for si in 0..sides {
            let from = mids[si].1;
            let (entry, exit, mut can_ducts, can_dia, can_len) =
                build_muffler(&mut b, tun.muffler, si, d_main, v_can, pmat, diesel);
            // mid2 -> muffler entry
            if let Some(entry) = entry {
                b.join(
                    &format!("to_muffler{si}"),
                    &[(from, End::Right), (entry, End::Left)],
                    1.0,
                );
            }
            let after = exit.unwrap_or(from);
            // dual tips on single-sided cars with a sport or straight can
            let dual = sides == 1 && matches!(tun.muffler, Muffler::Sport | Muffler::Straight);
            let tips = if dual { 2 } else { 1 };
            let d_tail = d_main * if dual { 0.8 } else { 1.0 };
            let mut tail_ducts = Vec::new();
            let mut legs = vec![(after, End::Right)];
            for t in 0..tips {
                let wt = b.wall(pmat, Some(1.6), false, d_tail);
                let td = b.duct(&format!("tail{si}_{t}"), l_tail, d_tail, d_tail, wt);
                legs.push((td, End::Left));
                tail_ducts.push(td);
            }
            b.join(&format!("to_tail{si}"), &legs, 1.0);
            for (t, &td) in tail_ducts.iter().enumerate() {
                let area = b.area_of(td, End::Right);
                let node = b.net.add_node(Node::open(
                    &format!("mouth{si}_{t}"),
                    PipeLeg::new(td, End::Right),
                    OpenEnd::radiating(area),
                ));
                tails_out.push(Tail {
                    node,
                    duct: td,
                    diameter: d_tail,
                });
            }
            let wt = b.wall(pmat, Some(1.6), false, d_tail);
            b.body(&format!("tailpipe{si}"), tail_ducts.clone(), d_tail, wt);
            if !can_ducts.is_empty() {
                let wcan = b.wall(pmat, Some(1.1), false, can_dia);
                b.body(
                    &format!("muffler{si}"),
                    std::mem::take(&mut can_ducts),
                    can_dia,
                    wcan,
                );
            }
            side_len[si] += can_len + l_tail;
        }

        // ------------------------------------------------------------------ report
        let total_cells: usize = (0..b.net.grid.ducts.len()).map(|d| b.net.grid.n(d)).sum();
        report.cells = total_cells;
        report.nodes = b.net.nodes.len();
        report.tails = tails_out.len();
        report.resonator_hz = resonator_hz;
        let turbo_len = if n_turbo > 0 { 0.24 } else { 0.0 };
        report.path_length = report.primary_length + coll_len + turbo_len + side_len[0];
        let mut sys = ExhaustSystem {
            net: b.net,
            port_node,
            tails: tails_out,
            bodies: b.bodies,
            chambers: b.chambers,
            res_base: inp.res_base,
            turbines,
            report,
            dist: Vec::new(),
        };
        sys.dist = sys.compute_dist();
        sys
    }

    /// Distance of every duct's midpoint from the valves, following the gas path (shortest route).
    fn compute_dist(&self) -> Vec<f32> {
        let nd = self.net.grid.ducts.len();
        let mut start = vec![f32::INFINITY; nd];
        for d in 0..self.port_node.len().min(nd) {
            start[d] = 0.0;
        }
        for _ in 0..nd {
            let mut changed = false;
            for node in &self.net.nodes {
                for a in &node.legs {
                    for c in &node.legs {
                        if a.duct != c.duct && start[a.duct].is_finite() {
                            let cand = start[a.duct] + self.net.grid.ducts[a.duct].length;
                            if cand < start[c.duct] {
                                start[c.duct] = cand;
                                changed = true;
                            }
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        (0..nd)
            .map(|d| {
                if start[d].is_finite() {
                    start[d] + 0.5 * self.net.grid.ducts[d].length
                } else {
                    0.0
                }
            })
            .collect()
    }

    /// Sets a cylinder's exhaust-valve effective area (0 = closed).
    #[inline]
    pub fn set_valve_area(&mut self, cyl: usize, area: f32) {
        self.net.nodes[self.port_node[cyl]].valves[0].area = area;
    }

    /// Starts the pipes warm: gas and walls follow a decaying profile from the exhaust-gas temperature at
    /// the headers toward a cool tailpipe (so a test or a hot restart does not wait a minute).
    pub fn preheat(&mut self, egt_k: f32) {
        let t_amb = self.net.env.t_amb;
        for d in 0..self.net.grid.ducts.len() {
            let tg = t_amb + (egt_k - t_amb) * (-self.dist[d] / 3.5).exp();
            self.net.grid.fill(d, self.net.env.p_amb, tg);
            self.net
                .grid
                .set_wall_temperature(d, t_amb + 0.8 * (tg - t_amb));
        }
    }

    /// Takes the gas and wall temperatures of `other`, an exhaust built from the same parts at another cell size.
    pub fn copy_thermal_from(&mut self, other: &ExhaustSystem) {
        if self.net.grid.ducts.len() != other.net.grid.ducts.len() {
            return;
        }
        for d in 0..self.net.grid.ducts.len() {
            let (g, w) = other.net.grid.thermal_profile(d);
            self.net.grid.set_thermal_profile(d, &g, &w);
        }
    }

    /// Mean fluctuating pressure (relative to `p_ref`) over a body's cells: what drives its shell.
    pub fn body_pressure(&self, body: usize) -> f32 {
        let bd = &self.bodies[body];
        let (mut s, mut n) = (0.0f32, 0usize);
        for &d in &bd.ducts {
            let di = &self.net.grid.ducts[d];
            for k in di.base..di.base + di.n {
                s += self.net.grid.p[k];
            }
            n += di.n;
        }
        s / n.max(1) as f32
    }
}

/// Adds the muffler of `kind` for one side. Returns (entry duct, exit duct, ducts inside the can,
/// can diameter); both ends are `None` when there is no muffler.
fn build_muffler(
    b: &mut Builder,
    kind: Muffler,
    si: usize,
    d_main: f32,
    v_can: f32,
    mat: PipeMaterial,
    _diesel: bool,
) -> (Option<usize>, Option<usize>, Vec<usize>, f32, f32) {
    let a_main = std::f32::consts::FRAC_PI_4 * d_main * d_main;
    if kind == Muffler::Delete {
        return (None, None, vec![], d_main, 0.0);
    }
    let l_can = match kind {
        Muffler::Stock => 0.52,
        Muffler::Sport => 0.48,
        _ => 0.50,
    };
    let d_can = (4.0 * v_can / (std::f32::consts::PI * l_can))
        .sqrt()
        .clamp(d_main * 1.6, 0.30);
    let a_can = std::f32::consts::FRAC_PI_4 * d_can * d_can;
    let wall = |b: &Builder, d: f32| b.wall(mat, Some(1.1), false, d);
    let mut inside = Vec::new();
    let (entry, exit);
    match kind {
        Muffler::Stock | Muffler::Sport => {
            let fr: &[f32] = if kind == Muffler::Stock {
                &[0.34, 0.28, 0.38]
            } else {
                &[0.5, 0.5]
            };
            let hole = if kind == Muffler::Stock { 1.5 } else { 2.2 } * a_main;
            let w = wall(b, d_main);
            let inlet = b.duct(&format!("mu_in{si}"), 0.12, d_main, d_main, w);
            let mut prev = inlet;
            let mut chs = Vec::new();
            for (k, &f) in fr.iter().enumerate() {
                let w = wall(b, d_can);
                let d = b.duct(&format!("mu_ch{si}_{k}"), l_can * f, d_can, d_can, w);
                chs.push(d);
                inside.push(d);
            }
            b.join("mu_expand", &[(prev, End::Right), (chs[0], End::Left)], 1.0);
            prev = chs[0];
            for k in 1..chs.len() {
                // perforated baffle: a hole of area `hole` with a short tube behind it
                let loss = (a_can / (0.65 * hole) - 1.0).powi(2);
                let iner = 0.045 * a_can / hole;
                let node = Node::junction("mu_baffle")
                    .with_leg(
                        PipeLeg::new(prev, End::Right)
                            .with_loss(0.5 * loss, 0.5 * loss)
                            .with_inertance(0.5 * iner),
                    )
                    .with_leg(
                        PipeLeg::new(chs[k], End::Left)
                            .with_loss(0.5 * loss, 0.5 * loss)
                            .with_inertance(0.5 * iner),
                    );
                // the middle chamber of a stock can has perforations into a closed side volume
                if kind == Muffler::Stock && k == 1 {
                    let aux = b.chamber(0.3 * v_can);
                    let neck = SlugLeg::new(aux, 0.35 * a_main, 0.04).with_resistance(40.0);
                    b.net.add_node(node.with_slug(neck));
                } else {
                    b.net.add_node(node);
                }
                prev = chs[k];
            }
            let w = wall(b, d_main);
            let outlet = b.duct(&format!("mu_out{si}"), 0.12, d_main, d_main, w);
            b.join(
                "mu_contract",
                &[(prev, End::Right), (outlet, End::Left)],
                1.0,
            );
            inside.extend([inlet, outlet]);
            entry = inlet;
            exit = outlet;
        }
        _ => {
            // straight-through: a perforated core in an absorber-filled shell
            let segs = 6usize;
            let seg_len = l_can / segs as f32;
            let mut ducts = Vec::new();
            for k in 0..segs {
                let w = wall(b, d_main);
                ducts.push(b.duct(&format!("mu_core{si}_{k}"), seg_len, d_main, d_main, w));
                inside.push(*ducts.last().unwrap());
            }
            let open_ratio = 0.11;
            let hole_d = 0.003f32;
            let a_holes = open_ratio * std::f32::consts::PI * d_main * seg_len;
            let l_eff = 0.0012 + 0.85 * hole_d;
            let v_seg = (0.30 * (v_can - l_can * a_main) / (segs - 1) as f32).max(2.0e-4);
            for k in 0..segs - 1 {
                // each joint carries the perforations of the stretch of core behind it, opening into an
                // absorber-packed annular chamber
                let ci = b.chamber(v_seg);
                let neck = SlugLeg::new(ci, a_holes, l_eff).with_resistance(380.0);
                let n = Node::junction("mu_core_joint")
                    .with_leg(PipeLeg::new(ducts[k], End::Right))
                    .with_leg(PipeLeg::new(ducts[k + 1], End::Left))
                    .with_slug(neck);
                b.net.add_node(n);
            }
            entry = ducts[0];
            exit = ducts[segs - 1];
        }
    }
    (Some(entry), Some(exit), inside, d_can, l_can + 0.24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::engines;
    use crate::tuning::Tuning;

    fn banks_for(cfg: &EngineConfig) -> Vec<u8> {
        let nb = crate::config::bank_tilts_deg(&cfg.id).len();
        (0..cfg.cylinder_count).map(|l| (l % nb) as u8).collect()
    }

    #[test]
    fn every_engine_gets_a_sane_exhaust() {
        for cfg in engines() {
            let banks = banks_for(&cfg);
            let tuning = Tuning::default();
            let sys = ExhaustSystem::build(&ExhaustInputs {
                cfg: &cfg,
                tuning: &tuning,
                banks: &banks,
                dx: 0.035,
                env: Env::new(101_325.0, 293.0),
                res_base: cfg.cylinder_count,
            });
            let r = &sys.report;
            println!(
                "{:>9}: {} cells, {} nodes, {} tail(s), sides {}, primary {:.2} m d {:.0} mm, main {:.0} mm, path {:.2} m, resonator {:?} Hz",
                cfg.id, r.cells, r.nodes, r.tails, r.sides, r.primary_length, r.primary_diameter * 1e3, r.main_diameter * 1e3, r.path_length, r.resonator_hz
            );
            assert!(r.tails >= 1);
            assert!(
                r.cells > 40 && r.cells < 900,
                "{}: {} cells",
                cfg.id,
                r.cells
            );
            assert_eq!(sys.port_node.len(), cfg.cylinder_count);
            assert!(
                r.primary_diameter > 0.02 && r.primary_diameter < 0.09,
                "{}",
                r.primary_diameter
            );
            assert!(r.main_diameter > 0.04 && r.main_diameter < 0.12);
        }
    }

    use crate::acoustics::analysis::{peaks, spectrum};
    use crate::acoustics::linear::Linear;
    use crate::acoustics::{Gas, Reservoir};
    use crate::sound::radiation::monopole_pressure;

    /// Rings the whole exhaust with a small pulse at the first port and compares the spectral peaks of the
    /// sound radiated by the time-domain solver with the transfer-matrix (frequency-domain) prediction.
    fn ring_and_compare(id: &str, tuning: &Tuning, t_gas: f32) -> (Vec<f64>, Vec<f64>) {
        let cfg = crate::config::engine_by_id(id).unwrap();
        let banks = banks_for(&cfg);
        let mut env = Env::new(101_325.0, 293.0);
        env.tol = 0.02;
        let n = cfg.cylinder_count;
        let mut sys = ExhaustSystem::build(&ExhaustInputs {
            cfg: &cfg,
            tuning,
            banks: &banks,
            dx: 0.03,
            env,
            res_base: n,
        });
        for d in 0..sys.net.grid.ducts.len() {
            sys.net.grid.fill(d, env.p_amb, t_gas);
        }
        sys.net.grid.dissipation = false;
        let mut res: Vec<Reservoir> = (0..n)
            .map(|_| Reservoir::new(Gas::EXHAUST, 5.0e-4, env.p_amb, t_gas))
            .collect();
        res.extend(sys.chambers.iter().map(|c| {
            let mut c = c.clone();
            c.set_state(env.p_amb, t_gas);
            c
        }));
        sys.net.grid.update_primitives();
        // ~2 kPa pressure pulse in the first cell of the first primary
        sys.net.grid.inject(0, 0, 0.3, 0.0);
        let dt = 1.0 / 44_100.0;
        let mut p = Vec::new();
        let mut q_prev = vec![0.0f32; sys.tails.len()];
        for _ in 0..40_000 {
            sys.net.step(dt, &mut res);
            for r in res.iter_mut() {
                r.commit();
            }
            let mut s = 0.0;
            for (k, t) in sys.tails.iter().enumerate() {
                let q = sys.net.nodes[t.node].open_end().unwrap().q;
                s += monopole_pressure((q - q_prev[k]) / dt, 1.0);
                q_prev[k] = q;
            }
            p.push(s);
        }
        if std::env::var_os("RING_DEBUG").is_some() {
            let psd = spectrum(&p[2000..], 32768);
            let df = 44_100.0 / 32768.0;
            let mut lin2 = Linear::new(&sys.net, &sys.chambers, sys.res_base);
            lin2.temperature = Some(t_gas);
            lin2.alpha = 0.004;
            let mut f = 40.0;
            while f < 700.0 {
                let k0 = (f / df) as usize;
                let k1 = (f * 1.08 / df) as usize;
                let tdv = psd[k0..=k1].iter().cloned().fold(0.0f64, f64::max);
                let fdv = lin2
                    .radiated_response(sys.port_node[0], 1.0, &[f, f * 1.02, f * 1.04, f * 1.06])
                    .iter()
                    .cloned()
                    .fold(0.0f64, f64::max);
                println!(
                    "{f:6.0} Hz  td {:6.1} dB   tm {:6.1} dB",
                    10.0 * tdv.max(1e-30).log10(),
                    20.0 * fdv.max(1e-30).log10()
                );
                f *= 1.08;
            }
        }
        let td: Vec<f64> = peaks(&spectrum(&p[2000..], 32768), 44_100.0, 150.0, 700.0, 8.0)
            .iter()
            .take(8)
            .map(|x| x.0)
            .collect();
        let mut lin = Linear::new(&sys.net, &sys.chambers, sys.res_base);
        lin.temperature = Some(t_gas);
        lin.alpha = 0.004;
        let fd: Vec<f64> = lin
            .resonances(sys.port_node[0], 150.0, 700.0, 20)
            .iter()
            .map(|x| x.0)
            .collect();
        (td, fd)
    }

    #[test]
    fn time_domain_and_transfer_matrix_agree_on_the_resonances_of_a_real_exhaust() {
        for (id, tuning) in [
            ("i4", Tuning::default()),
            ("v8", Tuning::default()),
            (
                "i6",
                Tuning {
                    muffler: Muffler::Sport,
                    ..Tuning::default()
                },
            ),
        ] {
            let (td, fd) = ring_and_compare(id, &tuning, 600.0);
            println!(
                "{id} {:?}/{:?}
  time domain: {:?}
  transfer matrix: {:?}",
                tuning.muffler,
                tuning.cat,
                td.iter().map(|f| f.round() as i32).collect::<Vec<_>>(),
                fd.iter().map(|f| f.round() as i32).collect::<Vec<_>>()
            );
            assert!(td.len() >= 3 && fd.len() >= 3);
            let mut matched = 0;
            for f in td.iter().take(6) {
                if fd.iter().any(|g| (g / f - 1.0).abs() < 0.035) {
                    matched += 1;
                }
            }
            assert!(matched >= 3, "{id}: only {matched} of the 6 strongest simulated peaks sit within 3.5 % of a predicted resonance");
        }
    }

    #[test]
    #[ignore]
    fn ring_configs_report() {
        let cases: Vec<(&str, Tuning)> = vec![
            (
                "v8 pipes only",
                Tuning {
                    muffler: Muffler::Delete,
                    cat: Cat::Delete,
                    ..Tuning::default()
                },
            ),
            (
                "v8 pipes + stock cat",
                Tuning {
                    muffler: Muffler::Delete,
                    ..Tuning::default()
                },
            ),
            (
                "v8 pipes + straight can",
                Tuning {
                    muffler: Muffler::Straight,
                    cat: Cat::Delete,
                    ..Tuning::default()
                },
            ),
            (
                "v8 pipes + sport can",
                Tuning {
                    muffler: Muffler::Sport,
                    cat: Cat::Delete,
                    ..Tuning::default()
                },
            ),
            (
                "v8 pipes + stock can",
                Tuning {
                    muffler: Muffler::Stock,
                    cat: Cat::Delete,
                    ..Tuning::default()
                },
            ),
            (
                "v8 pipes, no crossover",
                Tuning {
                    muffler: Muffler::Delete,
                    cat: Cat::Delete,
                    crossover: Crossover::None,
                    ..Tuning::default()
                },
            ),
            (
                "v8 pipes, x-pipe",
                Tuning {
                    muffler: Muffler::Delete,
                    cat: Cat::Delete,
                    crossover: Crossover::XPipe,
                    ..Tuning::default()
                },
            ),
        ];
        for (name, t) in cases {
            let (td, fd) = ring_and_compare("v8", &t, 600.0);
            println!(
                "{name}
  time domain: {:?}
  transfer matrix: {:?}",
                td.iter().map(|f| f.round() as i32).collect::<Vec<_>>(),
                fd.iter().map(|f| f.round() as i32).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn crossover_choice_changes_the_network() {
        let cfg = crate::config::engine_by_id("v8").unwrap();
        let banks = banks_for(&cfg);
        let env = Env::new(101_325.0, 293.0);
        let mut counts = Vec::new();
        for cross in Crossover::ALL {
            let t = Tuning {
                crossover: cross,
                muffler: Muffler::Delete,
                cat: Cat::Delete,
                ..Tuning::default()
            };
            let sys = ExhaustSystem::build(&ExhaustInputs {
                cfg: &cfg,
                tuning: &t,
                banks: &banks,
                dx: 0.035,
                env,
                res_base: 8,
            });
            let legs: Vec<usize> = sys.net.nodes.iter().map(|n| n.legs.len()).collect();
            println!(
                "{:?}: {} nodes, {} ducts, max legs {}, 4-leg nodes {}",
                cross,
                sys.net.nodes.len(),
                sys.net.grid.ducts.len(),
                legs.iter().max().unwrap(),
                legs.iter().filter(|&&l| l == 4).count()
            );
            counts.push((sys.net.nodes.len(), sys.net.grid.ducts.len()));
        }
        assert_ne!(counts[0], counts[1], "an H-pipe adds a duct");
        assert_ne!(counts[0], counts[2], "an X-pipe changes the junctions");
    }

    /// A pipe, a muffler of `kind`, a pipe, anechoic at both ends: (network, chambers, inlet node, inlet duct,
    /// outlet duct, pipe area).
    fn muffler_rig(kind: Muffler, gas: Gas) -> (Network, Vec<Reservoir>, usize, usize, usize, f32) {
        let env = Env::new(101_325.0, 293.0);
        let mut b = Builder {
            net: Network::new(env, gas),
            chambers: Vec::new(),
            res_base: 0,
            bodies: Vec::new(),
            dx: 0.03,
            wall_scale: 1.0,
            env,
            _cfg: std::marker::PhantomData,
        };
        let d_main = 0.063;
        let w = |b: &Builder| b.wall(PipeMaterial::MildSteel, Some(1.6), false, d_main);
        let (wa, wb) = (w(&b), w(&b));
        let inlet = b.duct("src", 1.5, d_main, d_main, wa);
        let (entry, exit, _, _, _) = build_muffler(
            &mut b,
            kind,
            0,
            d_main,
            4.5e-3 * 3.1,
            PipeMaterial::MildSteel,
            false,
        );
        let outlet = b.duct("out", 1.5, d_main, d_main, wb);
        let a_in = b.net.grid.end_area(inlet, End::Left);
        let n_in = b.net.add_node(Node::open(
            "src_end",
            PipeLeg::new(inlet, End::Left),
            OpenEnd::anechoic(a_in),
        ));
        b.join(
            "to_can",
            &[(inlet, End::Right), (entry.unwrap(), End::Left)],
            1.0,
        );
        b.join(
            "from_can",
            &[(exit.unwrap(), End::Right), (outlet, End::Left)],
            1.0,
        );
        let a_out = b.net.grid.end_area(outlet, End::Right);
        b.net.add_node(Node::open(
            "out_end",
            PipeLeg::new(outlet, End::Right),
            OpenEnd::anechoic(a_out),
        ));
        (b.net, b.chambers, n_in, inlet, outlet, a_in)
    }

    /// Transmission loss (dB) of a muffler design from the transfer-matrix method.
    fn muffler_tl_tm(kind: Muffler, freqs: &[f64]) -> Vec<f64> {
        let (net, chambers, n_in, _, _, a_in) = muffler_rig(kind, Gas::AIR);
        let lin = Linear {
            temperature: Some(293.0),
            alpha: 0.0,
            ..Linear::new(&net, &chambers, 0)
        };
        let out_node = net.nodes.len() - 1;
        let z0 = 1.2 * 343.0 / a_in as f64;
        freqs
            .iter()
            .map(|&f| {
                let p = lin.node_pressures(n_in, f);
                // anechoic ends: the incident wave is Q Z0 / 2, the transmitted one is the pressure at the outlet node
                20.0 * ((0.5 * z0) / p[out_node].abs()).log10()
            })
            .collect()
    }

    #[test]
    fn muffler_designs_attenuate_like_real_ones() {
        let freqs: Vec<f64> = (0..46).map(|i| 100.0 + 20.0 * i as f64).collect();
        let mut mean = Vec::new();
        for kind in [Muffler::Stock, Muffler::Sport, Muffler::Straight] {
            let tl = muffler_tl_tm(kind, &freqs);
            let avg = tl.iter().sum::<f64>() / tl.len() as f64;
            let worst = tl.iter().cloned().fold(f64::MAX, f64::min);
            let best = tl.iter().cloned().fold(f64::MIN, f64::max);
            println!(
                "{kind:?}: mean TL {avg:.1} dB over 100-1000 Hz, min {worst:.1}, max {best:.1}"
            );
            mean.push(avg);
        }
        // real production mufflers: 20-40 dB; chambered performance cans 10-30 dB; glass-pack straight-throughs 5-20 dB
        assert!(
            (12.0..45.0).contains(&mean[0]),
            "stock muffler TL {}",
            mean[0]
        );
        assert!(
            (6.0..32.0).contains(&mean[1]),
            "sport muffler TL {}",
            mean[1]
        );
        assert!(
            (2.0..22.0).contains(&mean[2]),
            "straight-through TL {}",
            mean[2]
        );
        assert!(
            mean[0] > mean[1] && mean[1] > mean[2],
            "stock > sport > straight-through: {mean:?}"
        );
    }

    #[test]
    fn time_domain_and_transfer_matrix_agree_on_muffler_transmission_loss() {
        let mut results = Vec::new();
        for kind in [Muffler::Stock, Muffler::Sport, Muffler::Straight] {
            let curve = super::super::validation::transmission(
                |net| {
                    let (rig, chambers, _, a, b, _) = muffler_rig(kind, Gas::AIR);
                    *net = rig;
                    net.grid.dissipation = false;
                    let mut res: Vec<Reservoir> = chambers;
                    for r in res.iter_mut() {
                        r.set_state(101_325.0, 293.0);
                    }
                    (a, b, res)
                },
                0.12,
                0.0085,
                16384,
            );
            let freqs: Vec<f64> = (0..20).map(|i| 150.0 + 40.0 * i as f64).collect();
            let tm = muffler_tl_tm(kind, &freqs);
            let td: Vec<f64> = freqs
                .iter()
                .map(|f| {
                    let sel: Vec<f64> = curve
                        .iter()
                        .filter(|(g, _)| (g - f).abs() < 15.0)
                        .map(|(_, r)| -10.0 * r.log10())
                        .collect();
                    sel.iter().sum::<f64>() / sel.len() as f64
                })
                .collect();
            let mut diffs: Vec<f64> = td.iter().zip(&tm).map(|(a, b)| (a - b).abs()).collect();
            diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = diffs[diffs.len() / 2];
            let n = td.len() as f64;
            let (mt, mm) = (td.iter().sum::<f64>() / n, tm.iter().sum::<f64>() / n);
            let cov: f64 = td.iter().zip(&tm).map(|(a, b)| (a - mt) * (b - mm)).sum();
            let (vt, vm): (f64, f64) = (
                td.iter().map(|a| (a - mt).powi(2)).sum(),
                tm.iter().map(|b| (b - mm).powi(2)).sum(),
            );
            let corr = cov / (vt * vm).sqrt();
            println!("{kind:?}: time-domain vs transfer-matrix TL: correlation {corr:.2}, median offset {median:.1} dB, mean TL {mt:.1} vs {mm:.1} dB");
            if std::env::var_os("TL_DEBUG").is_some() {
                for ((f, a), b) in freqs.iter().zip(&td).zip(&tm) {
                    println!("   {f:5.0} Hz  TD {a:5.1}  TM {b:5.1}");
                }
            }
            results.push((kind, corr, median));
        }
        for (kind, corr, median) in results {
            assert!(
                corr > 0.75,
                "{kind:?}: TL curves do not follow each other (correlation {corr})"
            );
            assert!(
                median < 6.0,
                "{kind:?}: the two methods are {median} dB apart"
            );
        }
    }
}
