// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

//! Windowed compositing mode: renders a custom wgpu background with the Slint UI
//! composited on top, all without using Slint's built-in windowing.

use crate::headless::HeadlessWindowAdapter;
use crate::Scene;
use slint::platform::{ChannelEventLoopReceiver, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use std::ops::ControlFlow;
use std::rc::Rc;
use std::sync::Arc;
use wgpu_28 as wgpu;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent as WinitWindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CrtParams {
    resolution: [f32; 2],
    time: f32,
    scanline_intensity: f32,
    curvature: f32,
    chromatic_aberration: f32,
    vignette_intensity: f32,
    _pad: f32,
}

struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    // Background rendering
    bg_pipeline: wgpu::RenderPipeline,
    bg_uniform_buffer: wgpu::Buffer,
    bg_bind_group: wgpu::BindGroup,
    // CRT Post-processing
    crt_pipeline: wgpu::RenderPipeline,
    crt_bind_group_layout: wgpu::BindGroupLayout,
    crt_sampler: wgpu::Sampler,
    crt_uniform_buffer: wgpu::Buffer,
    // Slint offscreen texture
    slint_texture: wgpu::Texture,
    start_time: std::time::Instant,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    slint_adapter: Option<Rc<HeadlessWindowAdapter>>,
    slint_app: Option<Scene>,
    cursor_pos: Option<LogicalPosition>,
    slint_proxy: slint::platform::ChannelEventLoopProxy,
    slint_receiver: ChannelEventLoopReceiver,
}

impl App {
    fn new(
        slint_proxy: slint::platform::ChannelEventLoopProxy,
        slint_receiver: ChannelEventLoopReceiver,
    ) -> Self {
        Self {
            window: None,
            gpu: None,
            slint_adapter: None,
            slint_app: None,
            cursor_pos: None,
            slint_proxy,
            slint_receiver,
        }
    }

    fn init_gpu(&mut self, window: Arc<Window>) {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance.create_surface(window.clone()).expect("Failed to create surface");

        let adapter = pollster::block_on(async {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
                .expect("Failed to find GPU adapter")
        });

        let (device, queue) = pollster::block_on(async {
            adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("femtovg_wgpu windowed"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    trace: wgpu::Trace::default(),
                })
                .await
                .expect("Failed to create device")
        });

        let surface_caps = surface.get_capabilities(&adapter);

        let surface_format = surface_caps
            .formats
            .iter()
            .find(|f| !f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        // --- Background pipeline ---
        let bg_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("background shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../background.wgsl").into()),
        });

        let bg_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bg uniforms"),
            size: 16, // vec4 aligned: just a f32 time + padding
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bg_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bg bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let bg_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg bind group"),
            layout: &bg_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: bg_uniform_buffer.as_entire_binding(),
            }],
        });

        let bg_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bg pipeline layout"),
            bind_group_layouts: &[&bg_bind_group_layout],
            immediate_size: 0,
        });

        let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("background pipeline"),
            layout: Some(&bg_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &bg_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &bg_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(surface_format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- CRT Post-processing pipeline ---
        let crt_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("CRT shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../crt_postprocess.wgsl").into()),
        });

        let crt_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("CRT sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let crt_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("CRT uniforms"),
            size: std::mem::size_of::<CrtParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let crt_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("CRT bind group layout"),
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
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let crt_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("CRT pipeline layout"),
            bind_group_layouts: &[&crt_bind_group_layout],
            immediate_size: 0,
        });

        let crt_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("CRT pipeline"),
            layout: Some(&crt_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &crt_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &crt_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- Slint offscreen texture ---
        let slint_texture = create_slint_texture(&device, size.width.max(1), size.height.max(1));

        // --- Initialize Slint ---
        let platform = crate::headless::HeadlessPlatform::new(instance, device.clone(), queue.clone())
            .with_proxy(self.slint_proxy.clone());
        #[cfg(feature = "mcp")]
        slint::mcp::register();
        slint::platform::set_platform(Box::new(platform)).expect("Failed to set Slint platform");

        let slint_app = Scene::new().expect("Failed to create Scene");
        let slint_window = slint_app.window();
        slint_window.show().expect("Failed to show Slint window");

        // Retrieve the adapter that was created by set_platform when Scene::new() was called
        self.slint_adapter = crate::headless::last_adapter();

        let scale_factor = window.scale_factor() as f32;
        if let Some(adapter) = &self.slint_adapter {
            adapter.resize(
                slint::PhysicalSize::new(size.width.max(1), size.height.max(1)),
                scale_factor,
            );
        }
        slint_window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        self.slint_app = Some(slint_app);
        self.gpu = Some(GpuState {
            surface,
            device,
            queue,
            surface_config,
            bg_pipeline,
            bg_uniform_buffer,
            bg_bind_group,
            crt_pipeline,
            crt_bind_group_layout,
            crt_sampler,
            crt_uniform_buffer,
            slint_texture,
            start_time: std::time::Instant::now(),
        });
        self.window = Some(window);
    }

    fn render(&mut self) {
        let gpu = self.gpu.as_ref().unwrap();
        let window = self.window.as_ref().unwrap();
        let slint_app = self.slint_app.as_ref().unwrap();

        let elapsed = gpu.start_time.elapsed().as_secs_f32();

        // Update background uniforms
        gpu.queue.write_buffer(&gpu.bg_uniform_buffer, 0, bytemuck::bytes_of(&elapsed));

        // Update Slint timers/animations
        slint::platform::update_timers_and_animations();

        // Get the surface texture
        let output = match gpu.surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                let size = window.inner_size();
                self.resize(size.width, size.height);
                return;
            }
            Err(e) => {
                eprintln!("Surface error: {e}");
                return;
            }
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder =
            gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        // Pass 1: Render animated background
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("background pass"),
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
                multiview_mask: None,
            });
            rpass.set_pipeline(&gpu.bg_pipeline);
            rpass.set_bind_group(0, &gpu.bg_bind_group, &[]);
            rpass.draw(0..3, 0..1);
        }

        gpu.queue.submit(std::iter::once(encoder.finish()));

        // Pass 2: Render Slint UI to its offscreen texture
        if let Some(adapter) = &self.slint_adapter {
            if let Err(e) = adapter.renderer.render_to_texture(&gpu.slint_texture) {
                eprintln!("Slint render error: {e}");
            }
        }

        // Pass 3: Composite Slint texture over background WITH CRT EFFECT
        // Update CRT uniforms from Slint properties
        let params = CrtParams {
            resolution: [gpu.surface_config.width as f32, gpu.surface_config.height as f32],
            time: elapsed,
            scanline_intensity: slint_app.get_scanline_intensity(),
            curvature: slint_app.get_curvature(),
            chromatic_aberration: slint_app.get_chromatic_aberration(),
            vignette_intensity: slint_app.get_vignette_intensity(),
            _pad: 0.0,
        };
        gpu.queue.write_buffer(&gpu.crt_uniform_buffer, 0, bytemuck::bytes_of(&params));

        let slint_view = gpu.slint_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let crt_bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("CRT bind group"),
            layout: &gpu.crt_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&slint_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&gpu.crt_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: gpu.crt_uniform_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder =
            gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("CRT composite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rpass.set_pipeline(&gpu.crt_pipeline);
            rpass.set_bind_group(0, &crt_bind_group, &[]);
            rpass.draw(0..3, 0..1);
        }

        gpu.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        // Request next frame
        window.request_redraw();
    }

    fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if let Some(gpu) = &mut self.gpu {
            gpu.surface_config.width = width;
            gpu.surface_config.height = height;
            gpu.surface.configure(&gpu.device, &gpu.surface_config);
            gpu.slint_texture = create_slint_texture(&gpu.device, width, height);
        }

        if let Some(window) = &self.window {
            if let Some(adapter) = &self.slint_adapter {
                adapter.resize(
                    slint::PhysicalSize::new(width, height),
                    window.scale_factor() as f32,
                );
            }
        }
    }

    fn dispatch_slint_event(&self, event: WindowEvent) {
        if let Some(app) = &self.slint_app {
            app.window().dispatch_event(event);
        }
    }
}

impl ApplicationHandler<()> for App {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.slint_receiver.drain() == ControlFlow::Break(()) {
            event_loop.exit();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = winit::window::WindowAttributes::default()
            .with_title("Slint FemtoVG + WGPU Compositing")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("Failed to create window"));
        self.init_gpu(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WinitWindowEvent,
    ) {
        match event {
            WinitWindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WinitWindowEvent::Resized(size) => {
                self.resize(size.width, size.height);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WinitWindowEvent::RedrawRequested => {
                self.render();
            }
            WinitWindowEvent::CursorMoved { position, .. } => {
                let scale_factor =
                    self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0) as f32;
                let pos = LogicalPosition::new(
                    position.x as f32 / scale_factor,
                    position.y as f32 / scale_factor,
                );
                self.cursor_pos = Some(pos);
                self.dispatch_slint_event(WindowEvent::PointerMoved { position: pos });
            }
            WinitWindowEvent::CursorLeft { .. } => {
                self.cursor_pos = None;
                self.dispatch_slint_event(WindowEvent::PointerExited);
            }
            WinitWindowEvent::MouseInput { state, button, .. } => {
                if let Some(position) = self.cursor_pos {
                    let btn = match button {
                        MouseButton::Left => slint::platform::PointerEventButton::Left,
                        MouseButton::Right => slint::platform::PointerEventButton::Right,
                        MouseButton::Middle => slint::platform::PointerEventButton::Middle,
                        _ => slint::platform::PointerEventButton::Other,
                    };
                    let event = match state {
                        ElementState::Pressed => {
                            WindowEvent::PointerPressed { button: btn, position }
                        }
                        ElementState::Released => {
                            WindowEvent::PointerReleased { button: btn, position }
                        }
                    };
                    self.dispatch_slint_event(event);
                }
            }
            WinitWindowEvent::KeyboardInput { event, .. } => {
                if let Key::Named(named) = &event.logical_key {
                    let slint_key = match named {
                        NamedKey::Enter => Some(slint::platform::Key::Return),
                        NamedKey::Backspace => Some(slint::platform::Key::Backspace),
                        NamedKey::Tab => Some(slint::platform::Key::Tab),
                        NamedKey::Escape => Some(slint::platform::Key::Escape),
                        _ => None,
                    };
                    if let Some(key) = slint_key {
                        let text: slint::SharedString = key.into();
                        let we = match event.state {
                            ElementState::Pressed => WindowEvent::KeyPressed { text },
                            ElementState::Released => WindowEvent::KeyReleased { text },
                        };
                        self.dispatch_slint_event(we);
                    }
                }
                if let Key::Character(ch) = &event.logical_key {
                    let text = slint::SharedString::from(ch.as_str());
                    let we = match event.state {
                        ElementState::Pressed => WindowEvent::KeyPressed { text },
                        ElementState::Released => WindowEvent::KeyReleased { text },
                    };
                    self.dispatch_slint_event(we);
                }
            }
            _ => {}
        }
    }
}

fn create_slint_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("slint offscreen texture"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

pub fn run() {
    let event_loop = EventLoop::<()>::with_user_event().build().expect("Failed to create event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let winit_proxy = event_loop.create_proxy();
    let (slint_proxy, slint_receiver) =
        slint::platform::channel_event_loop_proxy(Some(Box::new(move || {
            let _ = winit_proxy.send_event(());
        })));
    let mut app = App::new(slint_proxy, slint_receiver);
    event_loop.run_app(&mut app).expect("Event loop failed");
}
