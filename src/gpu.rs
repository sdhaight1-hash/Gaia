//! Native GPU terrain preview. Uses egui's wgpu render pass and a custom shader.
//! The first vertical slice depth-sorts heightfield cells because the egui pass
//! has no depth attachment. A dedicated depth-buffered pass is a later milestone.
use crate::terrain::HeightField;
use bytemuck::{Pod, Zeroable};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
    egui_wgpu::wgpu::util::DeviceExt as _,
};
use glam::{Mat4, Vec3};
use std::num::NonZeroU64;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
}

struct TerrainRenderResources {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

pub fn eye_position(yaw: f32, pitch: f32, distance: f32) -> Vec3 {
    Vec3::new(
        yaw.cos() * pitch.cos(),
        pitch.sin(),
        yaw.sin() * pitch.cos(),
    ) * distance + Vec3::new(0.0, 0.2, 0.0)
}

pub fn view_matrix(yaw: f32, pitch: f32, distance: f32, aspect: f32) -> [f32; 16] {
    let eye = eye_position(yaw, pitch, distance);
    let view = Mat4::look_at_rh(eye, Vec3::new(0.0, 0.2, 0.0), Vec3::Y);
    let proj = Mat4::perspective_rh(45_f32.to_radians(), aspect.max(0.1), 0.01, 100.0);
    (proj * view).to_cols_array()
}

fn mesh(field: &HeightField, eye: Vec3) -> (Vec<Vertex>, Vec<u32>) {
    let n = field.size;
    let mut vertices = Vec::with_capacity(n * n);
    let step = 2.0 / (n - 1) as f32;
    for y in 0..n {
        for x in 0..n {
            let height = field.get(x, y).clamp(0.0, 2.0);
            let left = field.get(x.saturating_sub(1), y);
            let right = field.get((x + 1).min(n - 1), y);
            let up = field.get(x, y.saturating_sub(1));
            let down = field.get(x, (y + 1).min(n - 1));
            let normal = Vec3::new(left - right, step * 2.0, up - down).normalize_or_zero();
            let slope = 1.0 - normal.y;
            let color = if height < 0.07 {
                [0.21, 0.31, 0.30]
            } else if height < 0.28 {
                [0.37, 0.45, 0.29]
            } else if height < 0.62 && slope < 0.24 {
                [0.41, 0.47, 0.31]
            } else if height < 0.82 {
                [0.47, 0.43, 0.40]
            } else {
                [0.79, 0.81, 0.79]
            };
            vertices.push(Vertex {
                position: [-1.0 + x as f32 * step, height, -1.0 + y as f32 * step],
                normal: normal.to_array(),
                color,
            });
        }
    }
    let mut cells = Vec::with_capacity((n - 1) * (n - 1));
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let a = (y * n + x) as u32;
            let b = a + 1;
            let c = a + n as u32;
            let d = c + 1;
            let centroid = (Vec3::from_array(vertices[a as usize].position)
                + Vec3::from_array(vertices[d as usize].position)) * 0.5;
            let depth = centroid.distance_squared(eye);
            cells.push((depth, [a, c, b, b, c, d]));
        }
    }
    cells.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
    let indices = cells.into_iter().flat_map(|(_, cell)| cell).collect();
    (vertices, indices)
}

fn upload_buffers(
    device: &wgpu::Device,
    field: &HeightField,
    eye: Vec3,
) -> (wgpu::Buffer, wgpu::Buffer, u32) {
    let (vertices, indices): (Vec<Vertex>, Vec<u32>) = mesh(field, eye);
    let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Gaia terrain vertices"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Gaia terrain indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    (vertex_buf, index_buf, indices.len() as u32)
}

pub fn install(render_state: &egui_wgpu::RenderState, field: &HeightField, eye: Vec3) {
    let device = &render_state.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Gaia terrain shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("terrain.wgsl").into()),
    });
    let group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Gaia camera layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(64),
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Gaia terrain pipeline layout"),
        bind_group_layouts: &[Some(&group_layout)],
        immediate_size: 0,
    });
    const ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
        0 => Float32x3, 1 => Float32x3, 2 => Float32x3
    ];
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Gaia terrain pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &ATTRS,
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(render_state.target_format.into())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Gaia camera"),
        contents: bytemuck::cast_slice(&[0.0_f32; 16]),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::UNIFORM,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Gaia camera bind group"),
        layout: &group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        }],
    });
    let (vertices, indices, index_count) = upload_buffers(device, field, eye);
    render_state.renderer.write().callback_resources.insert(TerrainRenderResources {
        pipeline, bind_group, uniform, vertices, indices, index_count,
    });
}

pub fn upload(render_state: &egui_wgpu::RenderState, field: &HeightField, eye: Vec3) {
    let (vertices, indices, index_count) = upload_buffers(&render_state.device, field, eye);
    if let Some(resources) = render_state.renderer.write()
        .callback_resources.get_mut::<TerrainRenderResources>()
    {
        resources.vertices = vertices;
        resources.indices = indices;
        resources.index_count = index_count;
    }
}

pub struct TerrainCallback {
    pub camera: [f32; 16],
}

impl egui_wgpu::CallbackTrait for TerrainCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(state) = resources.get::<TerrainRenderResources>() {
            queue.write_buffer(&state.uniform, 0, bytemuck::cast_slice(&self.camera));
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(state) = resources.get::<TerrainRenderResources>() {
            pass.set_pipeline(&state.pipeline);
            pass.set_bind_group(0, &state.bind_group, &[]);
            pass.set_vertex_buffer(0, state.vertices.slice(..));
            pass.set_index_buffer(state.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..state.index_count, 0, 0..1);
        }
    }
}
