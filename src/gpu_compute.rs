//! Native wgpu compute for pointwise terrain graph operations.
//! Executed by the background worker; only the small result buffer is
//! read back to CPU for downstream nodes and PNG/OBJ export.
use crate::pipeline::BlendCompute;
use crate::terrain::HeightField;
use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::wgpu::{self, util::DeviceExt as _};
use std::{sync::mpsc, time::Duration};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Parameters {
    count: u32,
    mix: f32,
    padding: [u32; 2],
}

pub struct GpuBlend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl GpuBlend {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Gaia pointwise terrain blend compute"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blend.wgsl").into()),
        });
        let entries: Vec<_> = (0..5).map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: if binding == 4 {
                    wgpu::BufferBindingType::Uniform
                } else {
                    wgpu::BufferBindingType::Storage { read_only: binding != 3 }
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }).collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Gaia GPU blend bindings"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Gaia GPU compute layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Gaia GPU blend"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self { device, queue, layout, pipeline }
    }

    fn storage(&self, label: &'static str, data: &[f32]) -> wgpu::Buffer {
        self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(data),
            usage: wgpu::BufferUsages::STORAGE,
        })
    }
}

impl BlendCompute for GpuBlend {
    fn blend(
        &self, a: &HeightField, b: &HeightField,
        mask: Option<&HeightField>, mix: f32,
    ) -> Result<HeightField, String> {
        if a.size != b.size || a.values.len() != b.values.len()
            || mask.is_some_and(|m| m.size != a.size || m.values.len() != a.values.len())
        {
            return Err("GPU blend fields must have identical dimensions".to_owned());
        }
        let len = a.values.len();
        if len == 0 || len > u32::MAX as usize {
            return Err("GPU blend input length is invalid".to_owned());
        }
        let ones = vec![1.0f32; len];
        let a_buf = self.storage("gaia source a", &a.values);
        let b_buf = self.storage("gaia source b", &b.values);
        let mask_buf = self.storage("gaia mask", mask.map_or(&ones, |m| &m.values));
        let bytes = (len * std::mem::size_of::<f32>()) as u64;
        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gaia blend result"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let stage = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gaia blend readback"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params = Parameters {
            count: len as u32,
            mix: mix.clamp(0.0, 1.0),
            padding: [0, 0],
        };
        let uniform = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("gaia blend params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let buffers = [&a_buf, &b_buf, &mask_buf, &output, &uniform];
        let bindings: Vec<_> = buffers.iter().enumerate().map(|(slot, buffer)| {
            wgpu::BindGroupEntry {
                binding: slot as u32,
                resource: buffer.as_entire_binding(),
            }
        }).collect();
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gaia blend bind group"),
            layout: &self.layout,
            entries: &bindings,
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gaia blend compute encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("gaia blend pass"), timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(((len as u32) + 63) / 64, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &stage, 0, bytes);
        let submitted = self.queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::sync_channel(1);
        let mapped = stage.slice(..);
        mapped.map_async(wgpu::MapMode::Read, move |status| {
            let _ = tx.send(status.map_err(|err| err.to_string()));
        });
        self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submitted),
            timeout: Some(Duration::from_secs(10)),
        }).map_err(|err| format!("GPU compute poll: {err}"))?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|err| format!("GPU readback timeout: {err}"))?
            .map_err(|err| format!("GPU buffer mapping failed: {err}"))?;
        let data = {
            let view = mapped.get_mapped_range();
            bytemuck::cast_slice::<u8, f32>(&view).to_vec()
        };
        stage.unmap();
        Ok(HeightField { size: a.size, values: data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_shader_declares_dispatch_and_guard() {
        let shader = include_str!("blend.wgsl");
        assert!(shader.contains("@compute @workgroup_size(64)"));
        assert!(shader.contains("index >= params.count"));
    }
}
