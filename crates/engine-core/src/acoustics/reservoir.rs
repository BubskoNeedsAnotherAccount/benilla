//! Zero-dimensional gas volumes (cylinders, plenums, muffler chambers) and orifice flow.

use super::gas::Gas;

/// A well-mixed volume of gas at rest. Nodes add and remove mass and enthalpy through orifices;
/// [`Reservoir::commit`] folds the step's flows into its state.
#[derive(Clone, Debug)]
pub struct Reservoir {
    pub gas: Gas,
    pub volume: f32,
    pub mass: f32,
    /// Internal energy, J.
    pub energy: f32,
    /// Mass added this step (kg) and enthalpy added this step (J); cleared by [`Reservoir::commit`].
    pub dm: f32,
    pub dh: f32,
    /// A volume so large it never changes (the atmosphere).
    pub infinite: bool,
    pub min_mass: f32,
}

impl Reservoir {
    pub fn new(gas: Gas, volume: f32, p: f32, t: f32) -> Self {
        let mass = p * volume / (gas.r * t);
        Reservoir { gas, volume, mass, energy: mass * gas.e(t), dm: 0.0, dh: 0.0, infinite: false, min_mass: 1.0e-9 }
    }

    pub fn atmosphere(gas: Gas, p: f32, t: f32) -> Self {
        let mut r = Reservoir::new(gas, 1.0e3, p, t);
        r.infinite = true;
        r
    }

    #[inline]
    pub fn t(&self) -> f32 {
        self.gas.t_from_e((self.energy / self.mass.max(self.min_mass)).max(5.0e4))
    }

    #[inline]
    pub fn p(&self) -> f32 {
        self.mass * self.gas.r * self.t() / self.volume
    }

    #[inline]
    pub fn rho(&self) -> f32 {
        self.mass / self.volume
    }

    /// Resets the state to `p`, `t` in the current volume.
    pub fn set_state(&mut self, p: f32, t: f32) {
        self.mass = (p * self.volume / (self.gas.r * t)).max(self.min_mass);
        self.energy = self.mass * self.gas.e(t);
    }

    /// Moves the volume to `v`, doing the compression work on the gas.
    pub fn set_volume(&mut self, v: f32) {
        if !self.infinite {
            let p = self.p();
            self.energy -= p * (v - self.volume);
        }
        self.volume = v;
    }

    /// Applies the step's accumulated inflows.
    pub fn commit(&mut self) {
        if !self.infinite {
            self.mass = (self.mass + self.dm).max(self.min_mass);
            self.energy = (self.energy + self.dh - 0.0).max(self.mass * 5.0e4);
        }
        self.dm = 0.0;
        self.dh = 0.0;
    }
}

/// Normalised flow function of an ideal nozzle: `f(x) = psi(r) / psi_max` against
/// `x = (1 - r)/(1 - r_crit)`, where `psi` is the isentropic mass-flux function and `r = p_dn/p_up`.
/// `f` rises like `sqrt(x)` and reaches 1 at x = 1 (choked). The table stores `g = f / sqrt(x)`, which is
/// smooth, for gamma = 1.3; the shape moves by well under 1 % between gamma 1.25 and 1.40.
fn orifice_shape(gamma: f64) -> impl Fn(f64) -> f64 {
    move |x: f64| {
        let rc = (2.0 / (gamma + 1.0)).powf(gamma / (gamma - 1.0));
        let psi_max = gamma.sqrt() * (2.0 / (gamma + 1.0)).powf((gamma + 1.0) / (2.0 * (gamma - 1.0)));
        let x = x.max(1.0e-6);
        let r = 1.0 - x * (1.0 - rc);
        let psi = (2.0 * gamma / (gamma - 1.0) * (r.powf(2.0 / gamma) - r.powf((gamma + 1.0) / gamma))).max(0.0).sqrt();
        psi / psi_max
    }
}

const SHAPE_N: usize = 64;

fn shape_table() -> &'static [f32; SHAPE_N + 1] {
    static T: std::sync::OnceLock<[f32; SHAPE_N + 1]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let f = orifice_shape(1.3);
        let mut t = [0.0f32; SHAPE_N + 1];
        for (i, v) in t.iter_mut().enumerate() {
            let x = (i as f64 / SHAPE_N as f64).max(1.0e-4);
            *v = (f(x) / x.sqrt()) as f32;
        }
        t
    })
}

/// Mass flow (kg/s) through an ideal orifice of effective area `a` from stagnation state
/// (`p_up`, `t_up`) to static pressure `p_dn`: subsonic isentropic, choked at the critical ratio.
/// Zero when `p_dn >= p_up`.
pub fn orifice_flow(gas: &Gas, a: f32, p_up: f32, t_up: f32, p_dn: f32) -> f32 {
    orifice_flow_d(gas, a, p_up, t_up, p_dn).0
}

/// The same with the partial derivatives: `(flow, d flow / d p_up, d flow / d p_dn)`.
pub fn orifice_flow_d(gas: &Gas, a: f32, p_up: f32, t_up: f32, p_dn: f32) -> (f32, f32, f32) {
    if p_dn >= p_up || a <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let g = gas.gamma(t_up);
    // critical ratio and peak flux function, both nearly linear in gamma over 1.2 .. 1.45
    let rc = 0.5283 - 0.174 * (g - 1.4);
    let psi_max = 0.6847 + 0.1745 * (g - 1.4);
    let d = 1.0 - p_dn / p_up;
    let inv_band = 1.0 / (1.0 - rc);
    let x = d * inv_band;
    let (f, df) = if x >= 1.0 {
        (1.0, 0.0)
    } else {
        let t = shape_table();
        let u = x * SHAPE_N as f32;
        let i = (u as usize).min(SHAPE_N - 1);
        let fr = u - i as f32;
        let gi = t[i] + (t[i + 1] - t[i]) * fr;
        let dg = (t[i + 1] - t[i]) * SHAPE_N as f32;
        let sx = x.sqrt().max(1.0e-4);
        (gi * sx, dg * sx + gi * 0.5 / sx)
    };
    // The textbook law has an infinite slope at zero pressure difference, which no Newton solver likes;
    // blend it to a smooth ~(dp)^1.5 below 0.3 % of the upstream pressure (invisible above that).
    let den = d * d + 9.0e-6;
    let smooth = d / den.sqrt();
    let dsmooth = 9.0e-6 / (den * den.sqrt());
    let k = a * psi_max / (gas.r * t_up).sqrt();
    let m = k * p_up * f * smooth;
    // dm/dd at fixed p_up
    let dm_dd = k * p_up * (df * inv_band * smooth + f * dsmooth);
    let dm_dpup = k * f * smooth + dm_dd * (p_dn / (p_up * p_up));
    let dm_dpdn = -dm_dd / p_up;
    (m, dm_dpup, dm_dpdn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orifice_chokes_at_the_critical_ratio() {
        let g = Gas::AIR;
        let a = 1.0e-4;
        let m_crit = orifice_flow(&g, a, 2.0e5, 300.0, 2.0e5 * 0.5);
        let m_deep = orifice_flow(&g, a, 2.0e5, 300.0, 1.0e4);
        let m_low = orifice_flow(&g, a, 2.0e5, 300.0, 1.8e5);
        assert!((m_deep / m_crit - 1.0).abs() < 0.03, "{m_deep} vs {m_crit}");
        assert!(m_low < m_crit * 0.7);
        // textbook: choked mass flux of air = 0.0404 p0 / sqrt(T0) (SI) per m^2
        let textbook = 0.0404 * 2.0e5 / 300.0f32.sqrt() * a;
        assert!((m_deep / textbook - 1.0).abs() < 0.03, "{m_deep} vs {textbook}");
        assert_eq!(orifice_flow(&g, a, 1.0e5, 300.0, 1.5e5), 0.0);
    }

    #[test]
    fn tabulated_orifice_law_matches_the_closed_form() {
        // exact isentropic nozzle flow, gamma 1.35 (the table is built for 1.3)
        let g = Gas::AIR;
        let t_up = 700.0;
        let gam = g.gamma(t_up) as f64;
        let exact = |r: f64| {
            let rc = (2.0 / (gam + 1.0)).powf(gam / (gam - 1.0));
            let r = r.max(rc);
            (2.0 * gam / (gam - 1.0) * (r.powf(2.0 / gam) - r.powf((gam + 1.0) / gam))).sqrt()
        };
        for &pr in &[0.99f64, 0.95, 0.9, 0.8, 0.7, 0.6, 0.55, 0.5, 0.3] {
            let m = orifice_flow(&g, 1.0e-3, 3.0e5, t_up, (3.0e5 * pr) as f32) as f64;
            let want = 1.0e-3 * 3.0e5 / (g.r as f64 * t_up as f64).sqrt() * exact(pr) * (0.99f64.min(1.0)).max(0.0);
            let smooth = {
                let d = 1.0 - pr;
                d / (d * d + 9.0e-6).sqrt()
            };
            let want = want / 0.99 * smooth;
            assert!((m / want - 1.0).abs() < 0.02, "pr {pr}: {m} vs {want}");
        }
    }

    #[test]
    fn orifice_derivatives_match_finite_differences() {
        let g = Gas::EXHAUST;
        for &(p_up, p_dn) in &[(3.0e5f32, 2.9e5f32), (3.0e5, 2.0e5), (3.0e5, 1.5e5), (3.0e5, 0.5e5), (1.2e5, 1.19e5)] {
            let (m, du, dd) = orifice_flow_d(&g, 1.0e-3, p_up, 900.0, p_dn);
            let e = 20.0;
            let fu = (orifice_flow(&g, 1.0e-3, p_up + e, 900.0, p_dn) - orifice_flow(&g, 1.0e-3, p_up - e, 900.0, p_dn)) / (2.0 * e);
            let fd = (orifice_flow(&g, 1.0e-3, p_up, 900.0, p_dn + e) - orifice_flow(&g, 1.0e-3, p_up, 900.0, p_dn - e)) / (2.0 * e);
            assert!(m > 0.0);
            assert!((du - fu).abs() <= 0.06 * fu.abs().max(du.abs()) + 2e-9, "d/dp_up at {p_up}->{p_dn}: {du} vs {fu}");
            assert!((dd - fd).abs() <= 0.06 * fd.abs().max(dd.abs()) + 2e-9, "d/dp_dn at {p_up}->{p_dn}: {dd} vs {fd}");
        }
    }

    #[test]
    fn reservoir_gas_law_and_compression() {
        let mut r = Reservoir::new(Gas::AIR, 1.0e-3, 101_325.0, 300.0);
        assert!((r.p() - 101_325.0).abs() < 5.0);
        assert!((r.t() - 300.0).abs() < 0.2);
        // adiabatic compression to a tenth of the volume heats the gas by roughly 10^0.4
        for _ in 0..1000 {
            let v = r.volume * 0.9977;
            r.set_volume(v);
        }
        assert!((r.volume - 1.0e-3 * 0.9977f32.powi(1000)).abs() < 1e-6);
        let ratio = 1.0e-3 / r.volume;
        let t_ideal = 300.0 * ratio.powf(0.4);
        assert!((r.t() / t_ideal - 1.0).abs() < 0.12, "{} vs {}", r.t(), t_ideal);
    }
}
