//! The car's paint: lit from the world's time-of-day sun and ambient (`WowLighting`) the way the
//! world's models are, with a clear-coat highlight and a sky reflection (`shaders/car.wgsl`).
//! The `embedded_asset!` call must stay in a file directly under `src/`.

use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

use benilla_world::lighting::{LightingConsumeSet, WowLighting};

use crate::model::Finish;

/// The car's material; the `StandardMaterial` shell supplies only the blend and cull state.
pub type CarMaterial = ExtendedMaterial<StandardMaterial, CarPaint>;

/// The light and finish on binding 100, and the glb's texture; `car.wgsl` matches the field order.
#[derive(Asset, AsBindGroup, Clone, TypePath)]
pub struct CarPaint {
    /// xyz the direction the sun's light travels.
    #[uniform(100)]
    pub sun: Vec4,
    #[uniform(100)]
    pub ambient: Vec4,
    #[uniform(100)]
    pub diffuse: Vec4,
    #[uniform(100)]
    pub spec: Vec4,
    /// The horizon colour the paint reflects.
    #[uniform(100)]
    pub sky: Vec4,
    /// x highlight strength, y highlight exponent, z reflection strength, w how transparent a
    /// ghost car is (0 for an ordinary one).
    #[uniform(100)]
    pub finish: Vec4,
    /// rgb the colour the surface is multiplied by: the picked body colour on the paint, white
    /// elsewhere.
    #[uniform(100)]
    pub tint: Vec4,
    /// The glb's texture; untextured parts sample white.
    #[texture(101)]
    #[sampler(102)]
    pub texture: Option<Handle<Image>>,
}

impl MaterialExtension for CarPaint {
    fn fragment_shader() -> ShaderRef {
        "embedded://benilla_lambo/shaders/car.wgsl".into()
    }
}

/// How each part takes the light: x highlight strength, y highlight exponent, z reflection.
pub fn finish(part: crate::model::PartKind, finish: Finish) -> Vec4 {
    match (part, finish) {
        (_, Finish::Glass) => Vec4::new(1.0, 120.0, 0.9, 0.0),
        (crate::model::PartKind::Body, _) | (_, Finish::Paint) => Vec4::new(0.9, 64.0, 0.55, 0.0),
        // Tyres and rims: a dull sheen.
        _ => Vec4::new(0.25, 24.0, 0.12, 0.0),
    }
}

/// A body colour as the shader takes it.
pub fn tint(rgb: [u8; 3]) -> Vec4 {
    Vec4::new(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
        1.0,
    )
}

/// A paint lit as an ordinary afternoon until the first `WowLighting` resolve.
pub fn paint(finish: Vec4, tint: Vec4, texture: Option<Handle<Image>>) -> CarPaint {
    let mut paint = CarPaint {
        sun: Vec3::new(-0.4, -0.8, -0.45).extend(0.0),
        ambient: Vec4::splat(0.45),
        diffuse: Vec4::splat(0.6),
        spec: Vec4::splat(0.5),
        sky: Vec4::new(0.6, 0.7, 0.8, 1.0),
        finish,
        tint,
        texture,
    };
    paint.sun = paint.sun.normalize();
    paint
}

pub struct CarPaintPlugin;

impl Plugin for CarPaintPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "shaders/car.wgsl");
        app.add_plugins(MaterialPlugin::<CarMaterial>::default())
            .add_systems(Update, follow_light.in_set(LightingConsumeSet));
    }
}

fn rgb(c: [f32; 3]) -> Vec4 {
    Vec4::new(c[0], c[1], c[2], 1.0)
}

/// Copies the resolved light into every car material when it changes, a car's own paint included;
/// a material written is re-uploaded, so an unchanged light writes nothing. A paint made later is
/// cloned from a lit one.
fn follow_light(
    light: Option<Res<WowLighting>>,
    mut materials: ResMut<Assets<CarMaterial>>,
    mut last: Local<Option<WowLighting>>,
) {
    let Some(light) = light else {
        return;
    };
    if last.as_ref() == Some(&*light) || light.sun_dir == Vec3::ZERO {
        return;
    }
    *last = Some(*light);
    for (_, mat) in materials.iter_mut() {
        let p = &mut mat.extension;
        p.sun = light.sun_dir.normalize().extend(0.0);
        p.ambient = rgb(light.ambient);
        p.diffuse = rgb(light.diffuse);
        p.spec = rgb(light.spec);
        p.sky = rgb(light.fog_color);
    }
}
