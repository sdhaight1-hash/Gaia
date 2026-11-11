// Dielectric GGX terrain preview with four splat-weighted material layers.
// Tileable 2D-array detail textures use triplanar projection, including cliffs.
// No authored PBR image assets, height/displacement maps, or IBL yet.
struct Camera {
    matrix: mat4x4<f32>,
    eye: vec4<f32>,
    materials: array<vec4<f32>, 4>,
    lighting: vec4<f32>,
    detail: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var material_atlas: texture_2d_array<f32>;
@group(1) @binding(1) var material_sampler: sampler;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) splat: vec4<f32>,
};
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) splat: vec4<f32>,
};
@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var output: VertexOut;
    output.position = camera.matrix * vec4<f32>(input.position, 1.0);
    output.world_position = input.position;
    output.normal = input.normal;
    output.color = input.color;
    output.splat = input.splat;
    return output;
}

fn fresnel_schlick(cos_theta: f32) -> vec3<f32> {
    let f0 = vec3<f32>(0.04, 0.04, 0.04);
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - cos_theta, 5.0);
}

// Explicit LOD allows per-fragment material selection without implicit
// derivative-uniformity requirements; atlas has one level and 64x64 tiles.
fn triplanar_detail(layer: i32, p: vec3<f32>, axis: vec3<f32>) -> vec4<f32> {
    let uv = p * max(camera.detail.x, 1.0);
    let x = textureSampleLevel(material_atlas, material_sampler, uv.zy, layer, 0.0);
    let y = textureSampleLevel(material_atlas, material_sampler, uv.xz, layer, 0.0);
    let z = textureSampleLevel(material_atlas, material_sampler, uv.xy, layer, 0.0);
    return x * axis.x + y * axis.y + z * axis.z;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let weights = max(input.splat, vec4<f32>(0.0));
    let coverage = dot(weights, vec4<f32>(1.0));
    var albedo = input.color;
    var roughness = 0.78;
    if coverage > 0.0001 {
        let w = weights / coverage;
        // R sand / G soil / B rock / A snow.
        albedo = camera.materials[0].xyz * w.x
            + camera.materials[1].xyz * w.y
            + camera.materials[2].xyz * w.z
            + camera.materials[3].xyz * w.w;
        roughness = dot(w, vec4<f32>(
            camera.materials[0].w, camera.materials[1].w,
            camera.materials[2].w, camera.materials[3].w));
        // Material detail is projected in terrain world-space, not attached
        // to horizontal UVs, so steep cliffs retain texture density.
        let n0 = normalize(input.normal);
        let axis = pow(abs(n0), vec3<f32>(4.0));
        let axis_weights = axis / max(dot(axis, vec3<f32>(1.0)), 0.0001);
        let detail = triplanar_detail(0, input.world_position, axis_weights) * w.x
            + triplanar_detail(1, input.world_position, axis_weights) * w.y
            + triplanar_detail(2, input.world_position, axis_weights) * w.z
            + triplanar_detail(3, input.world_position, axis_weights) * w.w;
        albedo *= vec3<f32>(0.70) + detail.rgb * 0.55;
        roughness = clamp(roughness * (0.85 + 0.3 * detail.a), 0.05, 1.0);
    }
    let n = normalize(input.normal);
    let v = normalize(camera.eye.xyz - input.world_position);
    let l = normalize(camera.lighting.xyz);
    let h = normalize(v + l);
    let ndl = max(dot(n, l), 0.0);
    let ndv = max(dot(n, v), 0.001);
    let ndh = max(dot(n, h), 0.0);
    let vdh = max(dot(v, h), 0.0);

    let alpha = roughness * roughness;
    let alpha2 = alpha * alpha;
    let t = ndh * ndh * (alpha2 - 1.0) + 1.0;
    let d = alpha2 / max(3.14159265 * t * t, 0.00001);
    let k = (roughness + 1.0) * (roughness + 1.0) * 0.125;
    let gv = ndv / (ndv * (1.0 - k) + k);
    let gl = ndl / (ndl * (1.0 - k) + k);
    let f = fresnel_schlick(vdh);
    let specular = (d * gv * gl * f) / max(4.0 * ndv * ndl, 0.001);
    let diffuse = (vec3<f32>(1.0) - f) * albedo * (1.0 / 3.14159265);
    let ambient = albedo * 0.28;
    let directional = (diffuse + specular) * ndl * camera.lighting.w;
    return vec4<f32>(clamp(ambient + directional,
        vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
