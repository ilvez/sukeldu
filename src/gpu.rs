use std::num::NonZeroU64;
use std::sync::Arc;

use winit::window::Window;

use crate::tiles::{TileKey, TILE};

pub const MAX_DISPATCH: usize = 256;
pub const MAX_INSTANCES: usize = 4096;
const PARAMS_STRIDE: u64 = 256;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    origin: [f64; 2],
    step: f64,
    layer: u32,
    max_iter: u32,
    row0: u32,
    samples: u32,
    _pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Globals {
    pub screen: [f32; 2],
    pub color_scale: f32,
    pub color_offset: f32,
    pub text_rect: [f32; 4],
    pub color_lin: f32,
    pub stripe: f32,
    pub _pad: [f32; 2],
}

pub const TEXT_W: u32 = 768;
pub const TEXT_H: u32 = crate::text::CELL_H as u32 * 2;
pub const SLICE_ROWS: u32 = 32;

pub struct Job {
    pub layer: u32,
    pub key: TileKey,
    pub max_iter: u32,
    pub row0: u32,
    pub samples: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub layer: u32,
    pub _pad: [u32; 3],
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    compute_pipeline: wgpu::ComputePipeline,
    compute_bg: wgpu::BindGroup,
    params_buf: wgpu::Buffer,
    tile_tex: wgpu::Texture,
    render_pipeline: wgpu::RenderPipeline,
    render_bg: wgpu::BindGroup,
    globals_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    readback_buf: wgpu::Buffer,
    text_pipeline: wgpu::RenderPipeline,
    text_tex: wgpu::Texture,
}

impl Gpu {
    pub fn new(window: Arc<Window>, layers: u32) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("adapter");
        println!("adapter: {:?}", adapter.get_info().name);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::SHADER_F64 | wgpu::Features::FLOAT32_FILTERABLE,
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .expect("device");

        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("surface config");
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        let tile_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tiles"),
            size: wgpu::Extent3d { width: TILE, height: TILE, depth_or_array_layers: layers },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let tile_view = tile_tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: PARAMS_STRIDE * MAX_DISPATCH as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let compute_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compute"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/compute.wgsl").into()),
        });
        let compute_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                    count: None,
                },
            ],
        });
        let compute_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &compute_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &params_buf,
                        offset: 0,
                        size: NonZeroU64::new(std::mem::size_of::<Params>() as u64),
                    }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tile_view) },
            ],
        });
        let compute_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&compute_bgl],
            push_constant_ranges: &[],
        });
        let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mandel"),
            layout: Some(&compute_layout),
            module: &compute_module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let render_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("render"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/render.wgsl").into()),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (std::mem::size_of::<Instance>() * MAX_INSTANCES) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let render_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let text_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("text"),
            size: wgpu::Extent3d { width: TEXT_W, height: TEXT_H, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let text_view = text_tex.create_view(&Default::default());
        let render_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &render_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&tile_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: globals_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: instance_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&text_view) },
            ],
        });
        let render_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&render_bgl],
            push_constant_ranges: &[],
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tiles"),
            layout: Some(&render_layout),
            vertex: wgpu::VertexState {
                module: &render_module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &render_module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text"),
            layout: Some(&render_layout),
            vertex: wgpu::VertexState {
                module: &render_module,
                entry_point: Some("vs_text"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &render_module,
                entry_point: Some("fs_text"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (TILE * TILE * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Gpu {
            device,
            queue,
            surface,
            config,
            compute_pipeline,
            compute_bg,
            params_buf,
            tile_tex,
            render_pipeline,
            render_bg,
            globals_buf,
            instance_buf,
            readback_buf,
            text_pipeline,
            text_tex,
        }
    }

    pub fn write_text(&self, pixels: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.text_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(TEXT_W), rows_per_image: Some(TEXT_H) },
            wgpu::Extent3d { width: TEXT_W, height: TEXT_H, depth_or_array_layers: 1 },
        );
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn frame(&mut self, jobs: &[Job], instances: &[Instance], globals: Globals) {
        let output = match self.surface.get_current_texture() {
            Ok(o) => o,
            Err(wgpu::SurfaceError::Outdated) | Err(wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Err(e) => {
                eprintln!("surface error: {e:?}");
                return;
            }
        };
        let view = output.texture.create_view(&Default::default());

        for (i, job) in jobs.iter().enumerate() {
            let (ox, oy) = job.key.origin();
            let p = Params {
                origin: [ox, oy],
                step: job.key.step(),
                layer: job.layer,
                max_iter: job.max_iter,
                row0: job.row0,
                samples: job.samples,
                _pad: [0; 2],
            };
            self.queue.write_buffer(&self.params_buf, i as u64 * PARAMS_STRIDE, bytemuck::bytes_of(&p));
        }
        self.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));
        if !instances.is_empty() {
            self.queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(instances));
        }

        let mut encoder = self.device.create_command_encoder(&Default::default());
        if !jobs.is_empty() {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.compute_pipeline);
            for i in 0..jobs.len() {
                pass.set_bind_group(0, &self.compute_bg, &[(i as u64 * PARAMS_STRIDE) as u32]);
                pass.dispatch_workgroups(TILE / 16, SLICE_ROWS / 16, 1);
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.render_pipeline);
            pass.set_bind_group(0, &self.render_bg, &[]);
            pass.draw(0..4, 0..instances.len() as u32);
            pass.set_pipeline(&self.text_pipeline);
            pass.draw(0..4, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        output.present();
    }

    pub fn read_layer(&self, layer: u32) -> Vec<f32> {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tile_tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback_buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(TILE * 4), rows_per_image: Some(TILE) },
            },
            wgpu::Extent3d { width: TILE, height: TILE, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let slice = self.readback_buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::Wait).expect("poll");
        let data: Vec<f32> = bytemuck::cast_slice(&slice.get_mapped_range()).to_vec();
        self.readback_buf.unmap();
        data
    }
}
