//! Calorically imperfect gas models for the acoustic solver.
//!
//! Internal energy is quadratic in temperature, `e(T) = a T + b T^2`, which fits the specific heats of
//! air and of stoichiometric burned gas to within ~2.5 % over 300-1500 K and still inverts in closed
//! form. That matters: the speed of sound is `sqrt(gamma R T)` and gamma falls from ~1.37 in a cold
//! tailpipe to ~1.29 in a hot header, so a constant-gamma solver puts every pipe resonance ~2-3 % off.

/// Specific gas constant and caloric law of one gas.
#[derive(Clone, Copy, Debug)]
pub struct Gas {
    /// J/(kg K)
    pub r: f32,
    /// e(T) = a T + b T^2, J/kg
    pub a: f32,
    pub b: f32,
    /// Prandtl number
    pub pr: f32,
    /// 2 / R, so the isentropic exponent needs no division.
    pub two_over_r: f32,
}

impl Gas {
    /// Dry air (cv 718 J/kgK at 300 K, 923 at 1500 K).
    pub const AIR: Gas = Gas { r: 287.05, a: 666.8, b: 0.0854, pr: 0.71, two_over_r: 2.0 / 287.05 };
    /// Burned stoichiometric gasoline products (13 % CO2, 13 % H2O, 74 % N2 by mole).
    pub const EXHAUST: Gas = Gas { r: 288.7, a: 696.5, b: 0.1375, pr: 0.75, two_over_r: 2.0 / 288.7 };

    /// Specific internal energy, J/kg.
    #[inline]
    pub fn e(&self, t: f32) -> f32 {
        t * (self.a + self.b * t)
    }

    /// Temperature from specific internal energy (inverse of [`Gas::e`]).
    #[inline]
    pub fn t_from_e(&self, e: f32) -> f32 {
        2.0 * e / (self.a + (self.a * self.a + 4.0 * self.b * e).sqrt())
    }

    /// Specific enthalpy, J/kg.
    #[inline]
    pub fn h(&self, t: f32) -> f32 {
        t * (self.a + self.r + self.b * t)
    }

    /// Temperature from specific enthalpy.
    #[inline]
    pub fn t_from_h(&self, h: f32) -> f32 {
        let a = self.a + self.r;
        2.0 * h / (a + (a * a + 4.0 * self.b * h).sqrt())
    }

    #[inline]
    pub fn cv(&self, t: f32) -> f32 {
        self.a + 2.0 * self.b * t
    }

    #[inline]
    pub fn cp(&self, t: f32) -> f32 {
        self.cv(t) + self.r
    }

    #[inline]
    pub fn gamma(&self, t: f32) -> f32 {
        1.0 + self.r / self.cv(t)
    }

    /// Speed of sound, m/s.
    #[inline]
    pub fn sound_speed(&self, t: f32) -> f32 {
        (self.gamma(t) * self.r * t).sqrt()
    }

    /// Dynamic viscosity (Sutherland), Pa s.
    #[inline]
    pub fn viscosity(&self, t: f32) -> f32 {
        1.458e-6 * t * t.sqrt() / (t + 110.4)
    }

    /// Thermal conductivity, W/(m K).
    #[inline]
    pub fn conductivity(&self, t: f32) -> f32 {
        self.cp(t) * self.viscosity(t) / self.pr
    }

    /// Integer `n = round(2/(gamma-1))` used for the isentropic relations `rho ~ s^n`, `p ~ s^(n+2)`
    /// (exact for gamma = 1 + 2/n; the rounding costs well under 1 % in nonlinear wave steepening and
    /// none at all in the linear acoustics).
    #[inline]
    pub fn isentropic_n(&self, t: f32) -> i32 {
        (self.cv(t) * self.two_over_r + 0.5) as i32
    }
}

/// `s^n` for small positive integer `n` without `powf`.
#[inline]
pub fn powi_fast(s: f32, n: i32) -> f32 {
    let s2 = s * s;
    match n {
        4 => s2 * s2,
        5 => s2 * s2 * s,
        6 => {
            let s3 = s2 * s;
            s3 * s3
        }
        7 => {
            let s3 = s2 * s;
            s3 * s3 * s
        }
        8 => {
            let s4 = s2 * s2;
            s4 * s4
        }
        9 => {
            let s4 = s2 * s2;
            s4 * s4 * s
        }
        _ => s.powi(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_matches_textbook_values() {
        let g = Gas::AIR;
        assert!((g.sound_speed(293.15) - 343.2).abs() < 1.0);
        assert!((g.gamma(300.0) - 1.4).abs() < 0.002);
        assert!((g.cv(300.0) - 718.0).abs() < 1.0);
        // cv(1000 K) real air: 854 J/kgK (within the 2.5 % fit)
        assert!((g.cv(1000.0) / 854.0 - 1.0).abs() < 0.025);
    }

    #[test]
    fn exhaust_gas_is_slower_and_lower_gamma_when_hot() {
        let g = Gas::EXHAUST;
        // ~640 m/s at 1100 K, ~330 m/s near ambient; gamma drops with temperature
        assert!((g.sound_speed(1100.0) - 640.0).abs() < 12.0, "{}", g.sound_speed(1100.0));
        assert!(g.gamma(1100.0) < g.gamma(400.0) - 0.05);
        assert!(g.sound_speed(1100.0) > g.sound_speed(500.0));
    }

    #[test]
    fn inverses_round_trip() {
        for g in [Gas::AIR, Gas::EXHAUST] {
            for t in [250.0, 300.0, 600.0, 1000.0, 1500.0] {
                assert!((g.t_from_e(g.e(t)) - t).abs() < 0.05, "e: {t}");
                assert!((g.t_from_h(g.h(t)) - t).abs() < 0.05, "h: {t}");
            }
        }
    }

    #[test]
    fn isentropic_exponent_is_an_integer_near_gamma() {
        assert_eq!(Gas::AIR.isentropic_n(300.0), 5);
        assert_eq!(Gas::EXHAUST.isentropic_n(1100.0), 7);
        assert!((powi_fast(1.1, 5) - 1.1f32.powi(5)).abs() < 1e-5);
        assert!((powi_fast(0.9, 7) - 0.9f32.powi(7)).abs() < 1e-6);
    }
}
