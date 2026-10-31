// The car's paint (`paint.rs`, `CarPaint`): the world models' gamma-space light
// `clamp(ambient + diffuse·max(N·L, 0))` from the time-of-day sun, the ambient dimmed on faces
// turned to the ground, then a clear-coat highlight and a Fresnel sky reflection on top.
#import bevy_pbr::{
    forward_io::VertexOutput,
    mesh_view_bindings::view,
}

struct Paint {
    sun: vec4<f32>,     // xyz = the direction the sun's light travels (Bevy space)
    ambient: vec4<f32>, // rgb, Light.dbc ambient
    diffuse: vec4<f32>, // rgb, Light.dbc sun diffuse
    spec: vec4<f32>,    // rgb, Light.dbc specular: the highlight's colour
    sky: vec4<f32>,     // rgb, the horizon (fog) colour the paint reflects
    finish: vec4<f32>,  // x = highlight strength, y = highlight exponent, z = reflection strength
};
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> paint: Paint;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var paint_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var paint_sampler: sampler;

// How much of the ambient a face turned straight down keeps: the car shades the road under it.
const UNDERSIDE: f32 = 0.55;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var base = vec4<f32>(1.0);
#ifdef VERTEX_COLORS
    base = in.color;
#endif
#ifdef VERTEX_UVS_A
    base = base * textureSample(paint_texture, paint_sampler, in.uv);
#endif

    let p = in.world_position.xyz;
    let v = normalize(view.world_position.xyz - p);
    // The car is double-sided and its glb is mirrored, so winding says nothing about which side
    // is seen: turn the normal to the side of the face toward the eye, judged on the flat face so
    // smooth normals at the silhouette do not flip.
    var face = normalize(cross(dpdx(p), dpdy(p)));
    face = select(-face, face, dot(face, v) >= 0.0);
    var n = normalize(in.world_normal);
    n = select(-n, n, dot(n, face) >= 0.0);

    let l = -normalize(paint.sun.xyz);
    let ndotl = max(dot(n, l), 0.0);
    let sky_facing = mix(UNDERSIDE, 1.0, n.y * 0.5 + 0.5);
    let lit = clamp(paint.ambient.rgb * sky_facing + paint.diffuse.rgb * ndotl, vec3(0.0), vec3(1.0));
    var color = base.rgb * lit;

    // Fresnel (Schlick, F0 0.04) toward the sky above the horizon and the dim ground below it.
    let ndotv = clamp(dot(n, v), 0.0, 1.0);
    let fresnel = 0.04 + 0.96 * pow(1.0 - ndotv, 5.0);
    let r = reflect(-v, n);
    let ground = paint.ambient.rgb * 0.35;
    let env = mix(ground, paint.sky.rgb, smoothstep(-0.15, 0.25, r.y));
    color = mix(color, env, fresnel * paint.finish.z);

    // Blinn-Phong clear coat, faded in over the terminator so it never lights a face in shadow.
    let h = normalize(l + v);
    let highlight = pow(max(dot(n, h), 0.0), paint.finish.y) * smoothstep(0.0, 0.15, ndotl);
    color = color + paint.spec.rgb * highlight * paint.finish.x;

    // Glass turns opaque toward grazing angles, as a reflection does.
    let alpha = mix(base.a, 1.0, fresnel * paint.finish.z);
    return vec4<f32>(clamp(color, vec3(0.0), vec3(1.0)), alpha);
}
