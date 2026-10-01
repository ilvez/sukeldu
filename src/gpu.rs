use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use winit::window::Window;

use crate::tiles::TILE;

pub const MAX_INSTANCES: usize = 4096;
const ORBIT_ENTRY_BYTES: u64 = 16;
const SKIP_ENTRY_BYTES: u64 = 48;
const STATE_BYTES: u64 = 64;
pub const STATE_SLOTS: u32 = 12;
pub const TILE_PIXELS: u32 = TILE * TILE;

// NOTE: one entry per state slot, matching `Tile` in the iteration shaders; every dispatch covers all slots, and a shader skips the slots whose kernel is not its own (0 = idle slot).
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TileParams {
    pub origin: [f64; 2],
    pub centre: [f64; 2],
    pub step: f64,
    pub layer: u32,
    pub max_iter: u32,
    pub samples: u32,
    pub ref_len: u32,
    pub skip_p: u32,
    pub use_skip: u32,
    pub kernel: u32,
    pub first: u32,
    pub steps: u32,
    pub bulbs: u32,
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kernel {
    Direct = 1,
    Perturb = 2,
    Perturb32 = 3,
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
    perturb_pipeline: wgpu::ComputePipeline,
    perturb32_pipeline: wgpu::ComputePipeline,
    compute_bgl: wgpu::BindGroupLayout,
    compute_bg: wgpu::BindGroup,
    params_buf: wgpu::Buffer,
    reference_bufs: [wgpu::Buffer; 4],
    state_buf: wgpu::Buffer,
    done: DoneCounts,
    submitted: u64,
    tile_tex: wgpu::Texture,
    tile_view: wgpu::TextureView,
    render_pipeline: wgpu::RenderPipeline,
    render_bg: wgpu::BindGroup,
    globals_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    readback_buf: wgpu::Buffer,
    text_pipeline: wgpu::RenderPipeline,
    text_tex: wgpu::Texture,
}

impl Gpu {
    pub fn new(window: Arc<Window>, layers: u32, vsync: bool) -> Self {
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
        let modes = surface.get_capabilities(&adapter).present_modes;
        config.present_mode = if vsync {
            wgpu::PresentMode::Fifo
        } else {
            [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
                .into_iter()
                .find(|m| modes.contains(m))
                .unwrap_or(wgpu::PresentMode::Fifo)
        };
        config.usage |= wgpu::TextureUsages::COPY_SRC;
        surface.configure(&device, &config);

        let tile_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tiles"),
            size: wgpu::Extent3d {
                width: TILE,
                height: TILE,
                depth_or_array_layers: layers,
            },
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
            size: (std::mem::size_of::<TileParams>() * STATE_SLOTS as usize) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let compute_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                Self::read_only_storage(0),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                Self::read_only_storage(6),
                Self::read_only_storage(7),
            ],
        });
        let reference_bufs = [
            ("orbit", 2 * ORBIT_ENTRY_BYTES),
            ("skip", SKIP_ENTRY_BYTES),
            ("orbit32", ORBIT_ENTRY_BYTES),
            ("skip32", SKIP_ENTRY_BYTES),
        ]
        .map(|(label, size)| Self::create_storage_buffer(&device, label, size));
        let state_size = STATE_SLOTS as u64 * (TILE * TILE) as u64 * STATE_BYTES;
        let state_buf = Self::create_storage_buffer(&device, "state", state_size);
        let done = DoneCounts::new(&device);
        let compute_bg = Self::compute_bind_group(
            &device,
            &compute_bgl,
            &params_buf,
            &tile_view,
            Self::storage_bindings(&reference_bufs, &state_buf, &done.counts),
        );
        let compute_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&compute_bgl],
            push_constant_ranges: &[],
        });
        let iteration_pipeline = |label: &str, source: &str| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&compute_layout),
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let compute_pipeline = iteration_pipeline("direct", include_str!("shaders/compute.wgsl"));
        let perturb_pipeline = iteration_pipeline("perturb", include_str!("shaders/perturb.wgsl"));
        let perturb32_pipeline =
            iteration_pipeline("perturb32", include_str!("shaders/perturb32.wgsl"));

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
            size: wgpu::Extent3d {
                width: TEXT_W,
                height: TEXT_H,
                depth_or_array_layers: 1,
            },
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
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tile_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: globals_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: instance_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&text_view),
                },
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
            perturb_pipeline,
            perturb32_pipeline,
            compute_bgl,
            compute_bg,
            params_buf,
            reference_bufs,
            state_buf,
            done,
            submitted: 0,
            tile_tex,
            tile_view,
            render_pipeline,
            render_bg,
            globals_buf,
            instance_buf,
            readback_buf,
            text_pipeline,
            text_tex,
        }
    }

    pub fn upload_reference(&mut self, orbit: &[[f64; 2]], skip: &[[f64; 6]]) {
        let orbit32: Vec<[f32; 2]> = orbit.iter().map(|z| z.map(|v| v as f32)).collect();
        let skip32: Vec<[f32; 6]> = skip.iter().map(narrow_skip_entry).collect();
        let data: [&[u8]; 4] = [
            bytemuck::cast_slice(orbit),
            bytemuck::cast_slice(skip),
            bytemuck::cast_slice(&orbit32),
            bytemuck::cast_slice(&skip32),
        ];
        let labels = ["orbit", "skip", "orbit32", "skip32"];
        let mut regrown = false;
        for ((buf, bytes), label) in self.reference_bufs.iter_mut().zip(data).zip(labels) {
            if bytes.len() as u64 > buf.size() {
                let size = (bytes.len() as u64).next_power_of_two();
                *buf = Self::create_storage_buffer(&self.device, label, size);
                regrown = true;
            }
        }
        if regrown {
            self.compute_bg = Self::compute_bind_group(
                &self.device,
                &self.compute_bgl,
                &self.params_buf,
                &self.tile_view,
                Self::storage_bindings(&self.reference_bufs, &self.state_buf, &self.done.counts),
            );
        }
        for (buf, bytes) in self.reference_bufs.iter().zip(data) {
            self.queue.write_buffer(buf, 0, bytes);
        }
    }

    pub fn submitted(&self) -> u64 {
        self.submitted
    }

    pub fn reset_done(&self, slot: u32) {
        self.queue.write_buffer(
            &self.done.counts,
            (slot * 4) as u64,
            bytemuck::bytes_of(&0u32),
        );
    }

    // NOTE: per-slot counts of finished pixels, as of the returned submission; the copy is read asynchronously, so they trail the dispatches issued since.
    pub fn poll_done(&mut self) -> Option<(u64, Vec<u32>)> {
        let at = self.done.in_flight?;
        let _ = self.device.poll(wgpu::PollType::Poll);
        if !self.done.mapped.load(Ordering::Acquire) {
            return None;
        }
        let counts =
            bytemuck::cast_slice(&self.done.readback.slice(..).get_mapped_range()).to_vec();
        self.done.readback.unmap();
        self.done.mapped.store(false, Ordering::Release);
        self.done.in_flight = None;
        Some((at, counts))
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
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TEXT_W),
                rows_per_image: Some(TEXT_H),
            },
            wgpu::Extent3d {
                width: TEXT_W,
                height: TEXT_H,
                depth_or_array_layers: 1,
            },
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

    // NOTE: returns false when the frame was dropped (surface lost); its dispatches never ran, so the caller must not count them.
    pub fn frame(
        &mut self,
        tiles: &[TileParams],
        instances: &[Instance],
        globals: Globals,
        capture: Option<&str>,
    ) -> bool {
        let output = match self.surface.get_current_texture() {
            Ok(o) => o,
            Err(wgpu::SurfaceError::Outdated) | Err(wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
            Err(e) => {
                eprintln!("surface error: {e:?}");
                return false;
            }
        };
        let view = output.texture.create_view(&Default::default());

        let active = tiles.iter().any(|t| t.kernel != 0);
        if active {
            self.queue
                .write_buffer(&self.params_buf, 0, bytemuck::cast_slice(tiles));
        }
        self.queue
            .write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));
        if !instances.is_empty() {
            self.queue
                .write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(instances));
        }

        let mut encoder = self.device.create_command_encoder(&Default::default());
        if active {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &self.compute_bg, &[]);
            for (kernel, pipeline) in [
                (Kernel::Direct, &self.compute_pipeline),
                (Kernel::Perturb, &self.perturb_pipeline),
                (Kernel::Perturb32, &self.perturb32_pipeline),
            ] {
                if tiles.iter().any(|t| t.kernel == kernel as u32) {
                    pass.set_pipeline(pipeline);
                    pass.dispatch_workgroups(TILE / 16, TILE / 16, tiles.len() as u32);
                }
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
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
        let capture = capture.map(|path| (path, self.copy_frame(&mut encoder, &output.texture)));
        let read_done = active && self.done.in_flight.is_none();
        if read_done {
            encoder.copy_buffer_to_buffer(
                &self.done.counts,
                0,
                &self.done.readback,
                0,
                self.done.counts.size(),
            );
        }
        self.queue.submit([encoder.finish()]);
        self.submitted += 1;
        if read_done {
            self.done.in_flight = Some(self.submitted);
            let mapped = self.done.mapped.clone();
            self.done
                .readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    mapped.store(result.is_ok(), Ordering::Release)
                });
        }
        output.present();
        if let Some((path, buffer)) = capture {
            self.write_ppm(path, &buffer);
        }
        true
    }

    fn copy_frame(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::Texture,
    ) -> wgpu::Buffer {
        let (w, h) = (self.config.width, self.config.height);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: (Self::padded_row(w) * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            frame.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(Self::padded_row(w)),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        buffer
    }

    fn write_ppm(&self, path: &str, buffer: &wgpu::Buffer) {
        let (w, h) = (self.config.width, self.config.height);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::Wait).expect("poll");
        let data = slice.get_mapped_range();
        let bgr = matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        for row in data.chunks(Self::padded_row(w) as usize) {
            for px in row[..(w * 4) as usize].chunks(4) {
                let (r, g, b) = if bgr {
                    (px[2], px[1], px[0])
                } else {
                    (px[0], px[1], px[2])
                };
                out.extend_from_slice(&[r, g, b]);
            }
        }
        drop(data);
        buffer.unmap();
        if let Err(e) = std::fs::write(path, out) {
            eprintln!("capture {path}: {e}");
        }
    }

    fn padded_row(width: u32) -> u32 {
        (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
    }

    pub fn read_layer(&self, layer: u32) -> Vec<f32> {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tile_tex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback_buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE * 4),
                    rows_per_image: Some(TILE),
                },
            },
            wgpu::Extent3d {
                width: TILE,
                height: TILE,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = self.readback_buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::Wait).expect("poll");
        let data: Vec<f32> = bytemuck::cast_slice(&slice.get_mapped_range()).to_vec();
        self.readback_buf.unmap();
        data
    }

    fn storage_bindings<'a>(
        reference: &'a [wgpu::Buffer; 4],
        state: &'a wgpu::Buffer,
        done: &'a wgpu::Buffer,
    ) -> [&'a wgpu::Buffer; 6] {
        let [orbit, skip, orbit32, skip32] = reference;
        [orbit, skip, state, done, orbit32, skip32]
    }

    fn read_only_storage(binding: u32) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }
    }

    fn create_storage_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn compute_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        params_buf: &wgpu::Buffer,
        tile_view: &wgpu::TextureView,
        storage: [&wgpu::Buffer; 6],
    ) -> wgpu::BindGroup {
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(tile_view),
            },
        ];
        entries.extend(
            storage
                .iter()
                .zip(2..)
                .map(|(buffer, binding)| wgpu::BindGroupEntry {
                    binding,
                    resource: buffer.as_entire_binding(),
                }),
        );
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &entries,
        })
    }
}

// NOTE: skip coefficients grow like the orbit's derivative and can exceed the f32 range; such an entry gets radius 0, so the 32-bit shader never takes it and steps instead.
fn narrow_skip_entry(e: &[f64; 6]) -> [f32; 6] {
    let narrow = e.map(|v| v as f32);
    if narrow.iter().all(|v| v.is_finite()) {
        narrow
    } else {
        [0.0; 6]
    }
}

struct DoneCounts {
    counts: wgpu::Buffer,
    readback: wgpu::Buffer,
    mapped: Arc<AtomicBool>,
    in_flight: Option<u64>,
}

impl DoneCounts {
    fn new(device: &wgpu::Device) -> Self {
        let size = (STATE_SLOTS * 4) as u64;
        DoneCounts {
            counts: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("done"),
                size,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("done readback"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            mapped: Arc::new(AtomicBool::new(false)),
            in_flight: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use wgpu::naga::{front::wgsl, valid};

    fn validate(source: &str) {
        let module = wgsl::parse_str(source).expect("wgsl parses");
        valid::Validator::new(valid::ValidationFlags::all(), valid::Capabilities::FLOAT64)
            .validate(&module)
            .expect("wgsl validates");
    }

    const SHADERS: [&str; 3] = [
        include_str!("shaders/compute.wgsl"),
        include_str!("shaders/perturb.wgsl"),
        include_str!("shaders/perturb32.wgsl"),
    ];

    #[test]
    fn iteration_shaders_are_valid() {
        for source in SHADERS {
            validate(source);
        }
    }

    fn struct_size(source: &str, name: &str) -> usize {
        let module = wgsl::parse_str(source).expect("wgsl parses");
        module
            .types
            .iter()
            .find_map(|(_, ty)| match (&ty.name, &ty.inner) {
                (Some(n), wgpu::naga::TypeInner::Struct { span, .. }) if n == name => {
                    Some(*span as usize)
                }
                _ => None,
            })
            .expect("struct exists")
    }

    #[test]
    fn iteration_shaders_match_the_rust_tile_params_layout() {
        for source in SHADERS {
            assert_eq!(
                struct_size(source, "Tile"),
                std::mem::size_of::<super::TileParams>()
            );
        }
    }

    #[test]
    fn skip_table_entries_match_the_rust_layout() {
        assert_eq!(
            struct_size(SHADERS[1], "Skip"),
            std::mem::size_of::<[f64; 6]>()
        );
        assert_eq!(
            struct_size(SHADERS[2], "Skip"),
            std::mem::size_of::<[f32; 6]>()
        );
    }

    #[test]
    fn iteration_shaders_lay_out_pixel_state_with_the_allocated_stride() {
        for source in SHADERS {
            assert_eq!(struct_size(source, "State") as u64, super::STATE_BYTES);
        }
    }
}
