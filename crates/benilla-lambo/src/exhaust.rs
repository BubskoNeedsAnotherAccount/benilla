//! The exhaust's pop on an upshift: a short flame out of each tailpipe, with the engine sound's
//! backfire (`sound.rs`). The flames ride on the car and are hidden between pops.

use bevy::prelude::*;

use crate::car::{exhaust_outlets, Car};

/// How long one pop's flame lasts, seconds.
const FLAME_LIFE: f32 = 0.16;
/// Of that, the time the flame takes to shoot out to its full length.
const FLAME_GROW: f32 = 0.025;
/// The fade's steps: a flame swaps to a dimmer material at each.
const STAGES: usize = 4;
/// Each layer's full length and width in yards: the orange outer flame and its white-hot core.
const OUTER: (f32, f32) = (0.95, 0.30);
const CORE: (f32, f32) = (0.55, 0.13);

/// One flame layer at one tailpipe, a child of the car.
#[derive(Component)]
pub struct ExhaustFlame {
    outlet: Vec3,
    dir: Vec3,
    core: bool,
}

#[derive(Resource)]
pub struct ExhaustFlames {
    outer: [Handle<StandardMaterial>; STAGES],
    core: [Handle<StandardMaterial>; STAGES],
    /// Seconds since the last pop.
    age: f32,
    /// This pop's length, a little different each time.
    reach: f32,
    rng: u32,
}

impl ExhaustFlames {
    fn next_f32(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / 16_777_216.0
    }
}

/// The flame's shape at `age` seconds into a pop, as (length, width) fractions of the full flame;
/// `None` once it is out.
fn envelope(age: f32) -> Option<(f32, f32)> {
    if !(0.0..FLAME_LIFE).contains(&age) {
        return None;
    }
    let grow = (age / FLAME_GROW).min(1.0);
    let t = age / FLAME_LIFE;
    Some((grow * (1.0 - 0.45 * t), grow.max(0.4) * (1.0 - 0.5 * t)))
}

pub fn setup_exhaust_flames(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    car: Query<Entity, With<Car>>,
) {
    let Ok(car) = car.single() else {
        return;
    };
    // A unit-wide blob, stretched to a flame by its scale.
    let mesh = meshes.add(
        Sphere::new(0.5)
            .mesh()
            .ico(2)
            .expect("a low subdivision count is valid"),
    );
    // Added light: a dimmer colour is a fainter flame.
    let mut fade = |rgb: [f32; 3]| {
        std::array::from_fn(|i| {
            let k = 1.0 - i as f32 / STAGES as f32;
            materials.add(StandardMaterial {
                base_color: Color::srgb(rgb[0] * k, rgb[1] * k, rgb[2] * k),
                alpha_mode: AlphaMode::Add,
                unlit: true,
                ..default()
            })
        })
    };
    let outer = fade([1.0, 0.45, 0.08]);
    let core = fade([1.0, 0.9, 0.65]);

    let (outlets, dir) = exhaust_outlets();
    for outlet in outlets {
        for (is_core, material) in [(false, &outer[0]), (true, &core[0])] {
            commands.spawn((
                ExhaustFlame {
                    outlet,
                    dir,
                    core: is_core,
                },
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(outlet),
                Visibility::Hidden,
                ChildOf(car),
            ));
        }
    }
    commands.insert_resource(ExhaustFlames {
        outer,
        core,
        age: FLAME_LIFE,
        reach: 1.0,
        rng: 0x9E37_79B9,
    });
}

/// Lights the flames as the car shifts up and shoots them out and fades them over the pop.
pub fn pop_exhaust_flames(
    time: Res<Time>,
    car: Query<&Car>,
    flames: Option<ResMut<ExhaustFlames>>,
    mut layers: Query<(
        &ExhaustFlame,
        &mut Transform,
        &mut Visibility,
        &mut MeshMaterial3d<StandardMaterial>,
    )>,
) {
    let (Ok(car), Some(mut flames)) = (car.single(), flames) else {
        return;
    };
    if car.just_upshifted {
        flames.age = 0.0;
        flames.reach = 0.8 + 0.4 * flames.next_f32();
    } else if flames.age < FLAME_LIFE {
        flames.age += time.delta_secs();
    } else {
        // Out: leave the hidden flames untouched.
        return;
    }

    let Some((length, width)) = envelope(flames.age) else {
        for (_, _, mut vis, _) in &mut layers {
            *vis = Visibility::Hidden;
        }
        return;
    };
    let stage = ((flames.age / FLAME_LIFE * STAGES as f32) as usize).min(STAGES - 1);
    for (flame, mut tf, mut vis, mut mat) in &mut layers {
        let (full_len, full_width) = if flame.core { CORE } else { OUTER };
        // A live flame flickers in width.
        let flicker = 0.85 + 0.3 * flames.next_f32();
        let len = full_len * flames.reach * length;
        let w = full_width * width * flicker;
        // The flame's base sits on the outlet.
        tf.translation = flame.outlet + flame.dir * (len * 0.5);
        tf.rotation = Quat::from_rotation_arc(Vec3::Z, flame.dir);
        tf.scale = Vec3::new(w, w, len);
        *vis = Visibility::Inherited;
        let want = if flame.core {
            &flames.core[stage]
        } else {
            &flames.outer[stage]
        };
        if mat.0 != *want {
            mat.0 = want.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flame_shoots_out_then_shrinks_and_goes_out() {
        let at = |t: f32| envelope(t).map(|(l, _)| l);
        assert_eq!(at(0.0), Some(0.0));
        let full = at(FLAME_GROW).unwrap();
        assert!(full > 0.9, "full length {full}");
        assert!(at(0.5 * FLAME_LIFE).unwrap() < full);
        assert!(at(FLAME_LIFE).is_none() && at(-0.01).is_none());
    }

    #[test]
    fn the_flames_point_out_of_the_back_of_the_car() {
        let ([left, right], dir) = exhaust_outlets();
        // The car faces −Z: the tail is +Z, behind the rear wheels.
        assert!(dir.abs_diff_eq(Vec3::Z, 1e-5), "{dir}");
        assert!(left.z > crate::model::SMOKE_Z && right.z > crate::model::SMOKE_Z);
        assert!((left.x + right.x).abs() < 1e-5 && left.x.abs() > 0.2);
        // Above the road, below the wing.
        assert!(left.y > -crate::car::RIDE && left.y < 1.0, "{left}");
    }
}
