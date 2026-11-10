struct Camera {
    matrix: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
};
@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var output: VertexOut;
    output.position = camera.matrix * vec4<f32>(input.position, 1.0);
    output.normal = input.normal;
    output.color = input.color;
    return output;
}
@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let light = normalize(vec3<f32>(0.50, 1.0, 0.36));
    let diffuse = max(dot(normalize(input.normal), light), 0.0);
    let shade = 0.34 + diffuse * 0.66;
    return vec4<f32>(input.color * shade, 1.0);
}
