//! True depth-tested wgpu preview, rendered offscreen and composited into egui.
//! Camera movement is uniform-only: do not rebuild or sort the mesh on orbit.
use gaia::terrain::HeightField;
use gaia::color::ColorField;
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
    splat: [f32; 4],
}

// 64 bytes of projection-view matrix followed by eye position (16-byte
// aligned), used for view-dependent dielectric specular highlights.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    matrix: [f32; 16],
    eye: [f32; 4],
}

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

struct Offscreen {
    dimensions: [u32; 2],
    _color: wgpu::Texture,
    color_view: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    composite_bind: wgpu::BindGroup,
}

struct TerrainRenderResources {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    composite_pipeline: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,
    composite_sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    offscreen: Option<Offscreen>,
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

/// Smooth default material preview. Height thresholds must not form
/// abrupt horizontal color rings on smooth crater/volcano geometry.
fn smooth_band(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| a[i] * (1.0 - t) + b[i] * t)
}

fn terrain_palette(height: f32, slope: f32) -> [f32; 3] {
    let base = mix_rgb([0.21, 0.32, 0.29], [0.41, 0.47, 0.31],
        smooth_band(0.04, 0.37, height));
    let rock = smooth_band(0.12, 0.40, slope)
        * smooth_band(0.12, 0.40, height);
    let worn = mix_rgb(base, [0.49, 0.45, 0.41], rock);
    mix_rgb(worn, [0.79, 0.81, 0.79], smooth_band(0.72, 1.02, height))
}

/// The selected RGBA map colors the output heightfield at matching grid UVs.
fn mesh(field: &HeightField, color_map: Option<&ColorField>,
    splat_map: Option<&ColorField>,
) -> (Vec<Vertex>, Vec<u32>) {
    let n = field.size;
    let mut vertices = Vec::with_capacity(n * n);
    let step = 2.0 / (n - 1) as f32;
    // Do not index mismatched asset dimensions. Use the established
    // elevation/slope palette when no valid Color field was selected.
    let color_map = color_map.filter(|map|
        map.size == n && map.pixels.len() == field.values.len()
    );
    let splat_map = splat_map.filter(|map|
        map.size == n && map.pixels.len() == field.values.len()
            && map.pixels.iter().all(|px| px.iter().all(|v|
                v.is_finite() && *v >= 0.0) && px.iter().sum::<f32>() > 0.0)
    );
    for y in 0..n {
        for x in 0..n {
            let height = field.get(x, y).clamp(0.0, 2.0);
            let left = field.get(x.saturating_sub(1), y);
            let right = field.get((x + 1).min(n - 1), y);
            let up = field.get(x, y.saturating_sub(1));
            let down = field.get(x, (y + 1).min(n - 1));
            let normal = Vec3::new(left - right, step * 2.0, up - down).normalize_or_zero();
            let slope = 1.0 - normal.y;
            let color = if let Some(map) = color_map {
                let rgba = map.pixels[y * n + x];
                // Terrain is opaque; alpha remains in the graph Color field
                // for future compositing rather than changing this pass.
                [rgba[0].clamp(0.0, 1.0),
                 rgba[1].clamp(0.0, 1.0),
                 rgba[2].clamp(0.0, 1.0)]
            } else {
                terrain_palette(height, slope)
            };
            vertices.push(Vertex {
                position: [-1.0 + x as f32 * step, height, -1.0 + y as f32 * step],
                normal: normal.to_array(),
                color,
                splat: splat_map.map_or([0.0; 4], |map| map.pixels[y * n + x]),
            });
        }
    }
    let mut indices = Vec::with_capacity((n - 1) * (n - 1) * 6);
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let a = (y * n + x) as u32;
            let b = a + 1;
            let c = a + n as u32;
            let d = c + 1;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    (vertices, indices)
}

fn upload_buffers(
    device: &wgpu::Device,
    field: &HeightField,
    color_map: Option<&ColorField>, splat_map: Option<&ColorField>,
) -> (wgpu::Buffer, wgpu::Buffer, u32) {
    let (vertices, indices): (Vec<Vertex>, Vec<u32>) =
        mesh(field, color_map, splat_map);
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

pub fn install(render_state: &egui_wgpu::RenderState, field: &HeightField, _eye: Vec3) {
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
                min_binding_size: NonZeroU64::new(std::mem::size_of::<CameraUniform>() as u64),
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Gaia terrain pipeline layout"),
        bind_group_layouts: &[Some(&group_layout)],
        immediate_size: 0,
    });
    const ATTRS: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32x4
    ];
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Gaia terrain pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &ATTRS,
            })],
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
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Gaia camera"),
        contents: bytemuck::bytes_of(&CameraUniform {
            matrix: [0.0; 16], eye: [0.0; 4],
        }),
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
    let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Gaia texture composite shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
    });
    let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Gaia composite texture layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let composite_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Gaia composite pipeline layout"),
        bind_group_layouts: &[Some(&composite_layout)],
        immediate_size: 0,
    });
    let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Gaia offscreen composite pipeline"),
        layout: Some(&composite_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &composite_shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &composite_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(render_state.target_format.into())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let composite_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("Gaia composite sampler"),
        min_filter: wgpu::FilterMode::Linear,
        mag_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let (vertices, indices, index_count) =
        upload_buffers(device, field, None, None);
    render_state.renderer.write().callback_resources.insert(TerrainRenderResources {
        pipeline, bind_group, uniform, vertices, indices, index_count,
        composite_pipeline, composite_layout, composite_sampler,
        format: render_state.target_format,
        offscreen: None,
    });
}

pub fn upload(render_state: &egui_wgpu::RenderState, field: &HeightField,
    color_map: Option<&ColorField>, splat_map: Option<&ColorField>,
) {
    let (vertices, indices, index_count) =
        upload_buffers(&render_state.device, field, color_map, splat_map);
    if let Some(resources) = render_state.renderer.write()
        .callback_resources.get_mut::<TerrainRenderResources>()
    {
        resources.vertices = vertices;
        resources.indices = indices;
        resources.index_count = index_count;
    }
}

impl TerrainRenderResources {
    fn ensure_target(&mut self, device: &wgpu::Device, pixels: [u32; 2]) {
        let dimensions = [pixels[0].max(1), pixels[1].max(1)];
        if self.offscreen.as_ref().is_some_and(|target| target.dimensions == dimensions) {
            return;
        }
        let size = wgpu::Extent3d {
            width: dimensions[0], height: dimensions[1], depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Gaia terrain offscreen color"),
            size, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Gaia terrain depth"),
            size, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let composite_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Gaia offscreen color sampler bind"),
            layout: &self.composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.composite_sampler),
                },
            ],
        });
        self.offscreen = Some(Offscreen {
            dimensions, _color: color, color_view,
            _depth: depth, depth_view, composite_bind,
        });
    }
}

pub struct TerrainCallback {
    pub camera: [f32; 16],
    pub eye: [f32; 4],
    pub pixels: [u32; 2],
}

impl egui_wgpu::CallbackTrait for TerrainCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(state) = resources.get_mut::<TerrainRenderResources>() {
            queue.write_buffer(&state.uniform, 0, bytemuck::bytes_of(&CameraUniform {
                matrix: self.camera,
                eye: self.eye,
            }));
            state.ensure_target(device, self.pixels);
            if let Some(target) = &state.offscreen {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Gaia depth-tested offscreen pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target.color_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.038, g: 0.057, b: 0.075, a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &target.depth_view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&state.pipeline);
                pass.set_bind_group(0, &state.bind_group, &[]);
                pass.set_vertex_buffer(0, state.vertices.slice(..));
                pass.set_index_buffer(state.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..state.index_count, 0, 0..1);
            }
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
            if let Some(target) = &state.offscreen {
                pass.set_pipeline(&state.composite_pipeline);
                pass.set_bind_group(0, &target.composite_bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preview_palette_is_continuous_near_former_color_thresholds() {
        for threshold in [0.07, 0.28, 0.62, 0.82] {
            for slope in [0.0, 0.15, 0.25, 0.60] {
                let a = terrain_palette(threshold - 0.001, slope);
                let b = terrain_palette(threshold + 0.001, slope);
                for channel in 0..3 {
                    assert!((a[channel] - b[channel]).abs() < 0.025,
                        "Color jump at {threshold} and slope {slope}");
                }
            }
        }
        for height in [0.0, 0.2, 0.5, 1.0, 1.8] {
            for slope in [0.0, 0.3, 0.8] {
                assert!(terrain_palette(height, slope)
                    .iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)));
            }
        }
    }

    #[test]
    fn terrain_mesh_uses_selected_color_field_without_moving_geometry() {
        let terrain = HeightField::flat(32, 0.4);
        let map = ColorField::flat(32, [0.85, 0.21, 0.04, 0.3]);
        let (colored, colored_indices) = mesh(&terrain, Some(&map), None);
        let (default, default_indices) = mesh(&terrain, None, None);
        assert_eq!(colored_indices, default_indices);
        for (a, b) in colored.iter().zip(default.iter()) {
            assert_eq!(a.position, b.position);
            assert_eq!(a.normal, b.normal);
            assert_eq!(a.color, [0.85, 0.21, 0.04]);
        }
        assert_ne!(colored[0].color, default[0].color);
    }

    #[test]
    fn invalid_color_resolution_falls_back_to_base_palette() {
        let terrain = HeightField::flat(32, 0.4);
        let mismatched = ColorField::flat(64, [1.0, 0.0, 0.0, 1.0]);
        let (colored, _) = mesh(&terrain, Some(&mismatched), None);
        let (original, _) = mesh(&terrain, None, None);
        assert_eq!(colored[0].color, original[0].color);
    }

    #[test]
    fn material_splat_changes_only_vertex_weights_not_geometry_or_color() {
        let terrain = HeightField::flat(32, 0.4);
        let mut mask = ColorField::flat(32, [0.0, 0.0, 1.0, 0.0]);
        mask.pixels[0] = [0.15, 0.25, 0.4, 0.2];
        let (material, material_indices) = mesh(&terrain, None, Some(&mask));
        let (plain, plain_indices) = mesh(&terrain, None, None);
        assert_eq!(material_indices, plain_indices);
        assert_eq!(material[0].splat, [0.15, 0.25, 0.4, 0.2]);
        for (a, b) in material.iter().zip(&plain) {
            assert_eq!(a.position, b.position);
            assert_eq!(a.normal, b.normal);
            assert_eq!(a.color, b.color);
            assert_eq!(b.splat, [0.0; 4]);
        }
        assert_eq!(material[100].splat, [0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn material_splat_mismatch_or_invalid_weights_revert_to_plain_preview() {
        let terrain = HeightField::flat(32, 0.4);
        for map in [
            ColorField::flat(64, [1.0, 0.0, 0.0, 0.0]),
            ColorField::flat(32, [0.0; 4]),
            ColorField::flat(32, [f32::NAN, 0.0, 0.0, 1.0]),
        ] {
            let (vertices, _) = mesh(&terrain, None, Some(&map));
            assert!(vertices.iter().all(|v| v.splat == [0.0; 4]));
        }
    }

    #[test]
    fn cpu_validates_terrain_pbr_wgsl_without_graphics_adapter() {
        let module = naga::front::wgsl::parse_str(include_str!("terrain.wgsl"))
            .expect("WGSL shader should parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        ).validate(&module).expect("WGSL shader must validate");
        assert_eq!(std::mem::size_of::<CameraUniform>(), 80);
        assert_eq!(std::mem::size_of::<Vertex>(), 52);
    }

    #[test]
    fn terrain_mesh_is_two_triangles_per_cell() {
        let terrain = HeightField::flat(32, 0.4);
        let (vertices, indices) = mesh(&terrain, None, None);
        assert_eq!(vertices.len(), 32 * 32);
        assert_eq!(indices.len(), 31 * 31 * 6);
    }
}
