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
    /// x highlight strength, y highlight exponent, z reflection strength.
    #[uniform(100)]
    pub finish: Vec4,
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
        (crate::model::PartKind::Body, _) => Vec4::new(0.9, 64.0, 0.55, 0.0),
        // Tyres and rims: a dull sheen.
        _ => Vec4::new(0.25, 24.0, 0.12, 0.0),
    }
}

/// A paint lit as an ordinary afternoon until the first `WowLighting` resolve.
pub fn paint(finish: Vec4, texture: Option<Handle<Image>>) -> CarPaint {
    let mut paint = CarPaint {
        sun: Vec3::new(-0.4, -0.8, -0.45).extend(0.0),
        ambient: Vec4::splat(0.45),
        diffuse: Vec4::splat(0.6),
        spec: Vec4::splat(0.5),
        sky: Vec4::new(0.6, 0.7, 0.8, 1.0),
        finish,
        texture,
    };
    paint.sun = paint.sun.normalize();
    paint
}

/// The car's materials, which follow the world's light.
#[derive(Resource, Default)]
pub struct CarMaterials(pub Vec<Handle<CarMaterial>>);

pub struct CarPaintPlugin;

impl Plugin for CarPaintPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "shaders/car.wgsl");
        app.add_plugins(MaterialPlugin::<CarMaterial>::default())
            .init_resource::<CarMaterials>()
            .add_systems(Update, follow_light.in_set(LightingConsumeSet));
    }
}

fn rgb(c: [f32; 3]) -> Vec4 {
    Vec4::new(c[0], c[1], c[2], 1.0)
}

/// Copies the resolved light into the car's materials when it changes; a material written is
/// re-uploaded, so an unchanged light writes nothing.
fn follow_light(
    light: Option<Res<WowLighting>>,
    cars: Res<CarMaterials>,
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
    for handle in &cars.0 {
        if let Some(mat) = materials.get_mut(handle) {
            let p = &mut mat.extension;
            p.sun = light.sun_dir.normalize().extend(0.0);
            p.ambient = rgb(light.ambient);
            p.diffuse = rgb(light.diffuse);
            p.spec = rgb(light.spec);
            p.sky = rgb(light.fog_color);
        }
    }
}
