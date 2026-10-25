//! Pipe materials and the wall model built from them.
//!
//! The material decides three things the ear can hear:
//! * how fast the gas cools along the pipe (wall heat capacity, conductivity, emissivity) - and with it
//!   the speed of sound and so every resonant frequency of the system;
//! * how rough the bore is (friction and so damping of the pulses);
//! * how the shell itself rings (mass per area, stiffness, internal loss factor): a thin titanium can
//!   radiates a bright, ringing "rasp" where a cast-iron manifold just thuds.

/// Pipe / shell materials a player can choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum PipeMaterial {
    /// Aluminised mild steel: what most production mid-pipes and mufflers are made of.
    #[default]
    MildSteel,
    /// 409 ferritic stainless: the usual OEM "stainless" system.
    Stainless409,
    /// 304 austenitic stainless: aftermarket headers and cat-backs.
    Stainless304,
    /// Commercially pure / 6Al-4V titanium: light, poorly conducting, lightly damped.
    Titanium,
    /// Inconel 625: Formula-1 style thin-wall headers.
    Inconel,
    /// Grey cast iron: stock exhaust manifolds. Thick, rough bore, very heavily damped.
    CastIron,
}

/// Physical properties of a material (SI).
#[derive(Clone, Copy, Debug)]
pub struct MaterialProps {
    pub density: f32,
    pub cp: f32,
    pub conductivity: f32,
    /// Emissivity of the outer surface once oxidised.
    pub emissivity: f32,
    pub youngs_pa: f32,
    pub poisson: f32,
    /// Structural loss factor (eta): sets how long a shell mode rings.
    pub loss_factor: f32,
    /// Absolute roughness of the bore, m.
    pub roughness_m: f32,
    /// Typical wall thickness, m.
    pub wall_m: f32,
}

impl PipeMaterial {
    pub const ALL: [PipeMaterial; 6] = [
        PipeMaterial::MildSteel,
        PipeMaterial::Stainless409,
        PipeMaterial::Stainless304,
        PipeMaterial::Titanium,
        PipeMaterial::Inconel,
        PipeMaterial::CastIron,
    ];

    /// Materials offered for the pipe system (cast iron is a manifold-only choice).
    pub const PIPES: [PipeMaterial; 5] = [
        PipeMaterial::MildSteel,
        PipeMaterial::Stainless409,
        PipeMaterial::Stainless304,
        PipeMaterial::Titanium,
        PipeMaterial::Inconel,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PipeMaterial::MildSteel => "Aluminised mild steel",
            PipeMaterial::Stainless409 => "409 stainless",
            PipeMaterial::Stainless304 => "304 stainless",
            PipeMaterial::Titanium => "Titanium",
            PipeMaterial::Inconel => "Inconel 625",
            PipeMaterial::CastIron => "Cast iron",
        }
    }

    pub fn props(self) -> MaterialProps {
        match self {
            PipeMaterial::MildSteel => MaterialProps {
                density: 7850.0,
                cp: 490.0,
                conductivity: 50.0,
                emissivity: 0.55,
                youngs_pa: 205e9,
                poisson: 0.29,
                loss_factor: 0.0015,
                roughness_m: 45e-6,
                wall_m: 0.0016,
            },
            PipeMaterial::Stainless409 => MaterialProps {
                density: 7740.0,
                cp: 460.0,
                conductivity: 25.0,
                emissivity: 0.45,
                youngs_pa: 200e9,
                poisson: 0.28,
                loss_factor: 0.0020,
                roughness_m: 35e-6,
                wall_m: 0.0015,
            },
            PipeMaterial::Stainless304 => MaterialProps {
                density: 8000.0,
                cp: 500.0,
                conductivity: 16.0,
                emissivity: 0.40,
                youngs_pa: 193e9,
                poisson: 0.29,
                loss_factor: 0.0025,
                roughness_m: 30e-6,
                wall_m: 0.0016,
            },
            PipeMaterial::Titanium => MaterialProps {
                density: 4500.0,
                cp: 523.0,
                conductivity: 7.0,
                emissivity: 0.35,
                youngs_pa: 110e9,
                poisson: 0.34,
                loss_factor: 0.0006,
                roughness_m: 20e-6,
                wall_m: 0.0010,
            },
            PipeMaterial::Inconel => MaterialProps {
                density: 8440.0,
                cp: 410.0,
                conductivity: 9.8,
                emissivity: 0.55,
                youngs_pa: 205e9,
                poisson: 0.31,
                loss_factor: 0.0020,
                roughness_m: 20e-6,
                wall_m: 0.0008,
            },
            PipeMaterial::CastIron => MaterialProps {
                density: 7200.0,
                cp: 500.0,
                conductivity: 52.0,
                emissivity: 0.85,
                youngs_pa: 100e9,
                poisson: 0.26,
                loss_factor: 0.012,
                roughness_m: 400e-6,
                wall_m: 0.0040,
            },
        }
    }
}

impl MaterialProps {
    /// Speed of longitudinal waves in a thin plate, m/s.
    pub fn plate_wave_speed(&self) -> f32 {
        (self.youngs_pa / (self.density * (1.0 - self.poisson * self.poisson))).sqrt()
    }
}

/// Everything the duct solver needs to know about a pipe wall.
#[derive(Clone, Copy, Debug)]
pub struct Wall {
    pub material: PipeMaterial,
    pub thickness_m: f32,
    /// Heat capacity per unit of inner surface, J/(m^2 K).
    pub cap: f32,
    /// Outer emissivity.
    pub eps: f32,
    /// Series thermal resistance of any heat wrap, m^2 K / W (0 = bare).
    pub r_ins: f32,
    /// Outer area / inner area (1 for a pipe; tiny for a catalyst substrate whose skin is a small can).
    pub outer_ratio: f32,
    /// Bore roughness, m.
    pub roughness_m: f32,
    /// Structural loss factor including any wrap damping.
    pub loss_factor: f32,
    /// Shell surface mass, kg/m^2.
    pub mass_area: f32,
}

impl Wall {
    /// A pipe or can of `dia_m` bore made of `material`, `thickness_m` thick. `wrapped` adds a header wrap.
    pub fn pipe(material: PipeMaterial, thickness_m: f32, wrapped: bool, dia_m: f32) -> Wall {
        let p = material.props();
        let t = thickness_m.max(1e-4);
        let mass_area = p.density * t;
        let mut cap = mass_area * p.cp * (1.0 + t / dia_m.max(0.01));
        let mut loss = p.loss_factor;
        let mut r_ins = 0.0;
        let mut mass = mass_area;
        if wrapped {
            // 3 mm basalt/fibreglass wrap: k ~ 0.12 W/mK, damps the shell and adds some mass
            r_ins = 0.003 / 0.12;
            loss += 0.04;
            mass += 0.5;
            cap += 0.5 * 840.0 * 0.5;
        }
        Wall {
            material,
            thickness_m: t,
            cap,
            eps: if wrapped { 0.9 } else { p.emissivity },
            r_ins,
            outer_ratio: 1.0,
            roughness_m: p.roughness_m,
            loss_factor: loss,
            mass_area: mass,
        }
    }

    /// The ceramic substrate of a catalyst: thin walls of cordierite inside a steel can.
    pub fn catalyst_substrate(channel_m: f32, wall_m: f32, can: PipeMaterial) -> Wall {
        let can_p = can.props();
        // cordierite: 2100 kg/m^3, 1000 J/kgK; the walls surround each channel, half belongs to each side
        let cap = 2100.0 * wall_m * 1000.0 * 0.5 * (1.0 + wall_m / channel_m.max(1e-4)) + 40.0;
        Wall {
            material: can,
            thickness_m: wall_m,
            cap,
            eps: can_p.emissivity,
            r_ins: 0.004,
            outer_ratio: 0.02,
            roughness_m: 5e-6,
            loss_factor: 0.02,
            mass_area: 8.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titanium_is_light_and_poorly_conducting() {
        let ti = PipeMaterial::Titanium.props();
        let st = PipeMaterial::MildSteel.props();
        assert!(ti.density < st.density * 0.65);
        assert!(ti.conductivity < st.conductivity * 0.2);
        assert!(ti.loss_factor < st.loss_factor);
        // thin-wall titanium has far less mass per square metre than a stock steel pipe
        let a = Wall::pipe(PipeMaterial::Titanium, ti.wall_m, false, 0.06);
        let b = Wall::pipe(PipeMaterial::MildSteel, st.wall_m, false, 0.06);
        assert!(a.mass_area < b.mass_area * 0.4);
        assert!(a.cap < b.cap * 0.4);
    }

    #[test]
    fn cast_iron_is_thick_rough_and_dead() {
        let c = PipeMaterial::CastIron.props();
        let s = PipeMaterial::Stainless304.props();
        assert!(c.wall_m > 2.0 * s.wall_m);
        assert!(c.roughness_m > 5.0 * s.roughness_m);
        assert!(c.loss_factor > 4.0 * s.loss_factor);
    }

    #[test]
    fn plate_wave_speed_is_about_five_kms_for_steel() {
        let c = PipeMaterial::MildSteel.props().plate_wave_speed();
        assert!((4900.0..5500.0).contains(&c), "{c}");
    }

    #[test]
    fn heat_wrap_insulates_and_damps() {
        let bare = Wall::pipe(PipeMaterial::Stainless304, 0.0016, false, 0.05);
        let wrap = Wall::pipe(PipeMaterial::Stainless304, 0.0016, true, 0.05);
        assert!(wrap.r_ins > 0.0 && bare.r_ins == 0.0);
        assert!(wrap.loss_factor > 10.0 * bare.loss_factor);
        assert!(wrap.cap > bare.cap);
    }
}
