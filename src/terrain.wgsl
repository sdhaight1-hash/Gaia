// Dielectric GGX terrain preview with four splat-weighted material layers.
// This is a material shading baseline: no texture sampling or IBL yet.
struct Camera {
    matrix: mat4x4<f32>,
    eye: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

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

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let weights = max(input.splat, vec4<f32>(0.0));
    let coverage = dot(weights, vec4<f32>(1.0));
    var albedo = input.color;
    var roughness = 0.78;
    if coverage > 0.0001 {
        let w = weights / coverage;
        // R sand / G soil / B rock / A snow.
        albedo = vec3<f32>(0.74, 0.63, 0.43) * w.x
            + vec3<f32>(0.20, 0.34, 0.20) * w.y
            + vec3<f32>(0.45, 0.46, 0.49) * w.z
            + vec3<f32>(0.91, 0.94, 0.97) * w.w;
        roughness = dot(w, vec4<f32>(0.88, 0.96, 0.68, 0.38));
    }
    let n = normalize(input.normal);
    let v = normalize(camera.eye.xyz - input.world_position);
    let l = normalize(vec3<f32>(0.5, 1.0, 0.36));
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
    let directional = (diffuse + specular) * ndl * 2.0;
    return vec4<f32>(clamp(ambient + directional,
        vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
