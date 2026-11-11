@group(0) @binding(0) var color_tex: texture_2d<f32>;
@group(0) @binding(1) var color_sampler: sampler;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32) -> Out {
    let xy = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var output: Out;
    let position = xy[vertex];
    output.position = vec4<f32>(position, 0.0, 1.0);
    output.uv = vec2<f32>(position.x * 0.5 + 0.5, 0.5 - position.y * 0.5);
    return output;
}

@fragment
fn fs_main(input: Out) -> @location(0) vec4<f32> {
    return textureSample(color_tex, color_sampler, input.uv);
}
