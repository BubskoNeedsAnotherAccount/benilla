//! Frequency-domain (plane-wave transfer-matrix) acoustics of a pipe network.
//!
//! This is the classical method of muffler design: each duct is a chain of uniform segments with a
//! 2x2 transfer matrix, junctions impose a common pressure and flow continuity, and the open mouth is a
//! radiation impedance. It is an independent implementation of the same physics the time-domain solver
//! integrates, so it serves two purposes: validating the solver's resonances on a real exhaust, and giving
//! the UI a cheap readout of the resonances the pipes have right now (at their present gas temperatures).
//! It is linear and has no mean flow, so it is exact for small signals and only approximate at blow-down.

use super::gas::Gas;
use super::grid::End;
use super::network::Network;
use super::node::Terminal;
use super::reservoir::Reservoir;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct C {
    pub re: f64,
    pub im: f64,
}

impl C {
    pub const ONE: C = C { re: 1.0, im: 0.0 };
    pub const ZERO: C = C { re: 0.0, im: 0.0 };
    pub fn new(re: f64, im: f64) -> C {
        C { re, im }
    }
    pub fn j(x: f64) -> C {
        C { re: 0.0, im: x }
    }
    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
    pub fn recip(self) -> C {
        let d = self.re * self.re + self.im * self.im;
        C {
            re: self.re / d,
            im: -self.im / d,
        }
    }
    pub fn cos(self) -> C {
        C {
            re: self.re.cos() * self.im.cosh(),
            im: -self.re.sin() * self.im.sinh(),
        }
    }
    pub fn sin(self) -> C {
        C {
            re: self.re.sin() * self.im.cosh(),
            im: self.re.cos() * self.im.sinh(),
        }
    }
}

impl std::ops::Add for C {
    type Output = C;
    fn add(self, o: C) -> C {
        C {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
}
impl std::ops::Sub for C {
    type Output = C;
    fn sub(self, o: C) -> C {
        C {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }
}
impl std::ops::Mul for C {
    type Output = C;
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
}
impl std::ops::Mul<f64> for C {
    type Output = C;
    fn mul(self, o: f64) -> C {
        C {
            re: self.re * o,
            im: self.im * o,
        }
    }
}
impl std::ops::Div for C {
    type Output = C;
    fn div(self, o: C) -> C {
        self * o.recip()
    }
}
impl std::ops::Neg for C {
    type Output = C;
    fn neg(self) -> C {
        C {
            re: -self.re,
            im: -self.im,
        }
    }
}
impl std::ops::AddAssign for C {
    fn add_assign(&mut self, o: C) {
        *self = *self + o;
    }
}

/// Solves `a x = b` by Gaussian elimination with partial pivoting.
pub fn solve(mut a: Vec<Vec<C>>, mut b: Vec<C>) -> Vec<C> {
    let n = b.len();
    for col in 0..n {
        let piv = (col..n)
            .max_by(|&i, &j| a[i][col].abs().partial_cmp(&a[j][col].abs()).unwrap())
            .unwrap();
        a.swap(col, piv);
        b.swap(col, piv);
        let d = a[col][col].recip();
        for r in col + 1..n {
            let f = a[r][col] * d;
            if f.abs() == 0.0 {
                continue;
            }
            for k in col..n {
                let v = a[col][k];
                a[r][k] = a[r][k] - f * v;
            }
            let bv = b[col];
            b[r] = b[r] - f * bv;
        }
    }
    let mut x = vec![C::ZERO; n];
    for i in (0..n).rev() {
        let mut s = b[i];
        for k in i + 1..n {
            s = s - a[i][k] * x[k];
        }
        x[i] = s / a[i][i];
    }
    x
}

/// The linear acoustics of a network at fixed gas temperatures.
pub struct Linear<'a> {
    pub net: &'a Network,
    /// Chamber reservoirs (slug legs refer to `res_base + index`).
    pub chambers: &'a [Reservoir],
    pub res_base: usize,
    /// Use this gas temperature everywhere instead of the live one (None = live).
    pub temperature: Option<f32>,
    /// Plane-wave attenuation, 1/m (friction and boundary layers).
    pub alpha: f64,
}

impl<'a> Linear<'a> {
    pub fn new(net: &'a Network, chambers: &'a [Reservoir], res_base: usize) -> Self {
        Linear {
            net,
            chambers,
            res_base,
            temperature: None,
            alpha: 0.02,
        }
    }

    fn gas(&self) -> Gas {
        self.net.grid.gas
    }

    /// Chain matrix [[A, B], [C, D]] of a duct at angular frequency `w` between pressure and volume flow.
    fn chain(&self, d: usize, w: f64) -> [[C; 2]; 2] {
        let g = &self.net.grid;
        let gas = self.gas();
        let dx = g.dx(d) as f64;
        let mut m = [[C::ONE, C::ZERO], [C::ZERO, C::ONE]];
        for i in 0..g.n(d) {
            let t = self.temperature.unwrap_or_else(|| g.t_at(d, i));
            let c = gas.sound_speed(t) as f64;
            let rho = self.net.env.p_amb as f64 / (gas.r as f64 * t as f64);
            let z = rho * c / g.area_at(d, i) as f64;
            let th = C::new(w * dx / c, -self.alpha * dx);
            let (cs, sn) = (th.cos(), th.sin());
            let cell = [[cs, C::j(z) * sn], [C::j(1.0 / z) * sn, cs]];
            // m = m * cell
            let mut n = [[C::ZERO; 2]; 2];
            for r in 0..2 {
                for k in 0..2 {
                    n[r][k] = m[r][0] * cell[0][k] + m[r][1] * cell[1][k];
                }
            }
            m = n;
        }
        m
    }

    /// Admittance of a neck + chamber (a Helmholtz-type slug leg) seen from the node.
    fn slug_admittance(&self, sl: &super::node::SlugLeg, w: f64) -> C {
        let gas = self.gas();
        let t = self.temperature.unwrap_or(self.net.env.t_amb) as f64;
        let rho = self.net.env.p_amb as f64 / (gas.r as f64 * t);
        let c = gas.sound_speed(t as f32) as f64;
        let a = sl.area as f64;
        let z_neck = C::new(sl.r as f64 / a, w * rho * sl.l_eff as f64 / a);
        let v = self
            .chambers
            .get(sl.res - self.res_base)
            .map_or(1e-3, |r| r.volume as f64);
        let z_ch = C::new(0.0, -rho * c * c / (w * v));
        (z_neck + z_ch).recip()
    }

    /// Admittance (volume flow out of the pipe per unit pressure) of an open mouth of face area `area`.
    pub fn mouth_admittance(&self, area: f64, delta: f64, w: f64) -> C {
        let env = self.net.env;
        let a_eq = (area / std::f64::consts::PI).sqrt();
        let rho = env.rho_amb as f64;
        let c = env.c_amb as f64;
        let m = rho * delta;
        let zr = rho * c * (2.0 * delta / a_eq).powi(2);
        // series inertance in parallel with the radiation resistance
        let jwm = C::j(w * m);
        let z_area = (jwm * C::new(zr, 0.0)) / (jwm + C::new(zr, 0.0));
        C::new(area, 0.0) / z_area
    }

    /// Volume flow out of every open mouth for a unit volume-velocity source at `source_node`, at `f` Hz.
    /// Returns the flows in the order the open nodes appear in the network.
    pub fn mouth_flows(&self, source_node: usize, f: f64) -> Vec<C> {
        let (p, open_nodes) = self.assemble_and_solve(source_node, f);
        open_nodes.iter().map(|&(ni, y)| p[ni] * y).collect()
    }

    fn assemble_and_solve(&self, source_node: usize, f: f64) -> (Vec<C>, Vec<(usize, C)>) {
        let w = std::f64::consts::TAU * f;
        let gas = self.gas();
        let t_air = self.temperature.unwrap_or(self.net.env.t_amb) as f64;
        let rho = self.net.env.p_amb as f64 / (gas.r as f64 * t_air);
        let nn = self.net.nodes.len();
        // one pressure unknown per node, plus one per duct end that sits behind a series inertance
        let mut nvar = nn;
        let nd = self.net.grid.ducts.len();
        let mut end_var = vec![[usize::MAX; 2]; nd];
        let mut series: Vec<(usize, usize, C)> = Vec::new(); // (node, end variable, series impedance)
        for (ni, node) in self.net.nodes.iter().enumerate() {
            for l in &node.legs {
                let slot = if l.end == End::Left { 0 } else { 1 };
                if l.inert > 0.0 {
                    let area = self.net.grid.end_area(l.duct, l.end) as f64;
                    let zs = C::j(w * rho * l.inert as f64 / area);
                    end_var[l.duct][slot] = nvar;
                    series.push((ni, nvar, zs));
                    nvar += 1;
                } else {
                    end_var[l.duct][slot] = ni;
                }
            }
        }
        let mut a = vec![vec![C::ZERO; nvar]; nvar];
        for d in 0..nd {
            let (l, r) = (end_var[d][0], end_var[d][1]);
            if l == usize::MAX || r == usize::MAX {
                continue;
            }
            let m = self.chain(d, w);
            let (aa, bb, dd) = (m[0][0], m[0][1], m[1][1]);
            let inv_b = bb.recip();
            // inflow-positive admittance matrix of the two-port
            a[l][l] += dd * inv_b;
            a[l][r] += -inv_b;
            a[r][l] += -inv_b;
            a[r][r] += aa * inv_b;
        }
        for &(ni, ev, zs) in &series {
            let y = zs.recip();
            a[ni][ni] += y;
            a[ni][ev] += -y;
            a[ev][ni] += -y;
            a[ev][ev] += y;
        }
        let mut open_nodes = Vec::new();
        for (ni, node) in self.net.nodes.iter().enumerate() {
            for sl in &node.slugs {
                a[ni][ni] += self.slug_admittance(sl, w);
            }
            if let Terminal::Open(oe) = &node.terminal {
                let y = match oe.mode {
                    super::node::OpenMode::Anechoic => C::new(
                        oe.area as f64 / (self.net.env.rho_amb as f64 * self.net.env.c_amb as f64),
                        0.0,
                    ),
                    _ => self.mouth_admittance(oe.area as f64, oe.delta as f64, w),
                };
                a[ni][ni] += y;
                open_nodes.push((ni, y));
            }
        }
        let mut s = vec![C::ZERO; nvar];
        s[source_node] = C::ONE;
        let p = solve(a, s);
        (p, open_nodes)
    }

    /// Pressure at every node for a unit volume-velocity source at `source_node`, at `f` Hz.
    pub fn node_pressures(&self, source_node: usize, f: f64) -> Vec<C> {
        self.assemble_and_solve(source_node, f).0
    }

    /// Radiated-pressure response (pressure at the listener per unit source volume velocity) summed over
    /// the mouths, for `freqs`. Monopoles: `p = 1.4 rho / (4 pi r) j w Q`.
    pub fn radiated_response(&self, source_node: usize, r: f64, freqs: &[f64]) -> Vec<f64> {
        let rho = self.net.env.rho_amb as f64;
        freqs
            .iter()
            .map(|&f| {
                let w = std::f64::consts::TAU * f;
                let q = self
                    .mouth_flows(source_node, f)
                    .into_iter()
                    .fold(C::ZERO, |a, b| a + b);
                1.4 * rho / (4.0 * std::f64::consts::PI * r) * w * q.abs()
            })
            .collect()
    }

    /// Frequencies of the response peaks between `f_lo` and `f_hi`, strongest first.
    pub fn resonances(
        &self,
        source_node: usize,
        f_lo: f64,
        f_hi: f64,
        count: usize,
    ) -> Vec<(f64, f64)> {
        let df = 0.5;
        let freqs: Vec<f64> = (0..)
            .map(|i| f_lo + i as f64 * df)
            .take_while(|&f| f <= f_hi)
            .collect();
        let resp = self.radiated_response(source_node, 1.0, &freqs);
        let mut pk: Vec<(f64, f64)> = (1..freqs.len() - 1)
            .filter(|&i| resp[i] > resp[i - 1] && resp[i] >= resp[i + 1])
            .map(|i| (freqs[i], resp[i]))
            .collect();
        pk.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        pk.truncate(count);
        pk
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acoustics::{DuctSpec, Env, Network, Node, OpenEnd, PipeLeg, PipeMaterial, Wall};

    fn pipe_net(len: f32, dia: f32) -> Network {
        let mut net = Network::new(Env::new(101_325.0, 293.0), Gas::AIR);
        let wall = Wall::pipe(PipeMaterial::MildSteel, 0.0015, false, dia);
        let d = net.add_duct(&DuctSpec::pipe("p", len, dia, wall, 0.02), 101_325.0, 293.0);
        net.add_node(Node::closed("closed", PipeLeg::new(d, End::Left)));
        let a = net.grid.end_area(d, End::Right);
        net.add_node(Node::open(
            "mouth",
            PipeLeg::new(d, End::Right),
            OpenEnd::radiating(a),
        ));
        net
    }

    #[test]
    fn transfer_matrix_finds_the_quarter_wave_resonances_of_a_pipe() {
        let net = pipe_net(1.2, 0.05);
        let lin = Linear {
            temperature: Some(293.0),
            alpha: 0.002,
            ..Linear::new(&net, &[], 0)
        };
        let pk = lin.resonances(0, 30.0, 700.0, 8);
        let c = Gas::AIR.sound_speed(293.0) as f64;
        let leff = 1.2 + 0.6133 * 0.025;
        let mut found = 0;
        for k in 1..=4 {
            let want = (2 * k - 1) as f64 * c / (4.0 * leff);
            if pk.iter().any(|p| (p.0 / want - 1.0).abs() < 0.01) {
                found += 1;
            }
        }
        println!(
            "peaks {:?}",
            pk.iter().map(|p| p.0.round()).collect::<Vec<_>>()
        );
        assert_eq!(found, 4, "peaks {:?}", pk);
    }
}
