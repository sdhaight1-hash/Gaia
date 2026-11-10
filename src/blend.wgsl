// Gaia's first native GPU terrain-compute primitive.
// The MaskedBlend and Blend graph nodes use this with a CPU reference path.
struct Params {
    count: u32,
    mix: f32,
    _padding0: u32,
    _padding1: u32,
};
@group(0) @binding(0) var<storage, read> source_a: array<f32>;
@group(0) @binding(1) var<storage, read> source_b: array<f32>;
@group(0) @binding(2) var<storage, read> mask: array<f32>;
@group(0) @binding(3) var<storage, read_write> destination: array<f32>;
@group(0) @binding(4) var<uniform> params: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index >= params.count) {
        return;
    }
    let weight = clamp(mask[index], 0.0, 1.0) * clamp(params.mix, 0.0, 1.0);
    destination[index] = source_a[index] * (1.0 - weight) + source_b[index] * weight;
}
