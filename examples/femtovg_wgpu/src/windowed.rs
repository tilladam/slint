// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

//! Windowed compositing mode: renders a custom wgpu background with the Slint UI
//! composited on top, all without using Slint's built-in windowing.

use crate::headless::HeadlessWindowAdapter;
use crate::Scene;
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalPosition};
use std::rc::Rc;
use std::sync::Arc;
use wgpu_28 as wgpu;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent as WinitWindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    // Background rendering
    bg_pipeline: wgpu::RenderPipeline,
    bg_uniform_buffer: wgpu::Buffer,
    bg_bind_group: wgpu::BindGroup,
    // Compositing
    composite_pipeline: wgpu::RenderPipeline,
    composite_bind_group_layout: wgpu::BindGroupLayout,
    composite_sampler: wgpu::Sampler,
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
}

impl App {
    fn new() -> Self {
        Self { window: None, gpu: None, slint_adapter: None, slint_app: None, cursor_pos: None }
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

        // --- Composite pipeline ---
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../composite.wgsl").into()),
        });

        let composite_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("composite sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let composite_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("composite bind group layout"),
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

        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("composite pipeline layout"),
                bind_group_layouts: &[&composite_bind_group_layout],
                immediate_size: 0,
            });

        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite pipeline"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
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
        let platform = crate::headless::HeadlessPlatform::new(
            instance,
            device.clone(),
            queue.clone(),
        );
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
            composite_pipeline,
            composite_bind_group_layout,
            composite_sampler,
            slint_texture,
            start_time: std::time::Instant::now(),
        });
        self.window = Some(window);
    }

    fn render(&mut self) {
        let gpu = self.gpu.as_ref().unwrap();
        let window = self.window.as_ref().unwrap();

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

        // Pass 3: Composite Slint texture over background
        let slint_view = gpu.slint_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let composite_bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite bind group"),
            layout: &gpu.composite_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&slint_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&gpu.composite_sampler),
                },
            ],
        });

        let mut encoder =
            gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite pass"),
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
            rpass.set_pipeline(&gpu.composite_pipeline);
            rpass.set_bind_group(0, &composite_bind_group, &[]);
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

impl ApplicationHandler for App {
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
    let event_loop = EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("Event loop failed");
}
