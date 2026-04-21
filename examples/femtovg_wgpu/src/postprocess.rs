// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

//! # CRT Post-Processing Demo
//!
//! Demonstrates `set_rendering_notifier()` to inject a custom wgpu post-processing
//! shader (CRT / retro monitor effect) into Slint's rendering pipeline.
//!
//! ```bash
//! cargo run -p femtovg_wgpu --bin postprocess
//! ```

use slint::wgpu_28::{WGPUConfiguration, WGPUSettings, wgpu};

slint::slint! {
    import { VerticalBox, HorizontalBox, Button, Slider, GroupBox } from "std-widgets.slint";

    export component PostprocessApp inherits Window {
        title: "Slint CRT Post-Processing Demo";
        preferred-width: 800px;
        preferred-height: 600px;

        in property <image> crt-texture;
        out property <length> requested-texture-width: crt-display.width;
        out property <length> requested-texture-height: crt-display.height;

        out property <float> scanline-intensity: scanline-slider.value / 100;
        out property <float> curvature: curvature-slider.value / 100;
        out property <float> chromatic-aberration: aberration-slider.value / 100;
        out property <float> vignette-intensity: vignette-slider.value / 100;

        in-out property <int> click-count: 0;

        HorizontalBox {
            crt-display := Image {
                source: crt-texture;
                min-width: 400px;
                horizontal-stretch: 2;
            }

            VerticalBox {
                horizontal-stretch: 1;
                alignment: start;

                Text {
                    text: "CRT Effect Controls";
                    font-size: 20px;
                    font-weight: 700;
                }

                GroupBox {
                    title: "Shader Parameters";
                    VerticalBox {
                        HorizontalBox {
                            Text { text: "Scanlines:"; vertical-alignment: center; }
                            scanline-slider := Slider { minimum: 0; maximum: 100; value: 40; }
                        }
                        HorizontalBox {
                            Text { text: "Curvature:"; vertical-alignment: center; }
                            curvature-slider := Slider { minimum: 0; maximum: 100; value: 30; }
                        }
                        HorizontalBox {
                            Text { text: "Chromatic:"; vertical-alignment: center; }
                            aberration-slider := Slider { minimum: 0; maximum: 100; value: 25; }
                        }
                        HorizontalBox {
                            Text { text: "Vignette:"; vertical-alignment: center; }
                            vignette-slider := Slider { minimum: 0; maximum: 100; value: 50; }
                        }
                    }
                }

                GroupBox {
                    title: "Interactive Content";
                    VerticalBox {
                        Text {
                            text: "Button clicks: " + click-count;
                            font-size: 16px;
                        }
                        Button {
                            text: "Click me!";
                            clicked => { click-count += 1; }
                        }
                    }
                }
            }
        }
    }
}

/// Holds the CRT post-processing pipeline resources.
struct CrtPostProcessor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    // Intermediate texture to capture the Slint-rendered content
    source_texture: wgpu::Texture,
    output_texture: wgpu::Texture,
    /// Whether the static test pattern needs to be re-uploaded (only on resize).
    content_dirty: bool,
    start_time: std::time::Instant,
}

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

impl CrtPostProcessor {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("CRT postprocess shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../crt_postprocess.wgsl").into(),
            ),
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("CRT sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("CRT params"),
            size: std::mem::size_of::<CrtParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout =
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("CRT pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("CRT postprocess pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8UnormSrgb.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let source_texture = Self::create_texture(device, 320, 200);
        let output_texture = Self::create_texture(device, 320, 200);

        Self {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            bind_group_layout,
            sampler,
            uniform_buffer,
            source_texture,
            output_texture,
            content_dirty: true,
            start_time: std::time::Instant::now(),
        }
    }

    fn create_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("CRT source texture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    fn render(
        &mut self,
        width: u32,
        height: u32,
        scanline_intensity: f32,
        curvature: f32,
        chromatic_aberration: f32,
        vignette_intensity: f32,
    ) -> wgpu::Texture {
        let width = width.max(1);
        let height = height.max(1);

        if self.source_texture.size().width != width
            || self.source_texture.size().height != height
        {
            self.source_texture = Self::create_texture(&self.device, width, height);
            self.output_texture = Self::create_texture(&self.device, width, height);
            self.content_dirty = true;
        }

        // Only regenerate the static test pattern when the texture size changed
        if self.content_dirty {
            self.render_content(width, height);
            self.content_dirty = false;
        }

        // Now apply the CRT effect
        let elapsed = self.start_time.elapsed().as_secs_f32();
        let params = CrtParams {
            resolution: [width as f32, height as f32],
            time: elapsed,
            scanline_intensity,
            curvature,
            chromatic_aberration,
            vignette_intensity,
            _pad: 0.0,
        };
        self.queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&params));

        let source_view =
            self.source_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view =
            self.output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("CRT bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("CRT postprocess pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
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
            rpass.set_pipeline(&self.pipeline);
            rpass.set_bind_group(0, &bind_group, &[]);
            rpass.draw(0..3, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.output_texture.clone()
    }

    /// Writes a colorful test pattern into the source texture for the CRT effect.
    fn render_content(&self, width: u32, height: u32) {
        let bytes_per_pixel = 4u32;
        let row_size = width * bytes_per_pixel;
        let mut data = vec![0u8; (row_size * height) as usize];

        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * bytes_per_pixel) as usize;
                // Create a simple test pattern: colored bars + grid
                let r = ((x as f32 / width as f32) * 200.0) as u8 + 55;
                let g = ((y as f32 / height as f32) * 150.0) as u8 + 50;
                let b = (((x + y) as f32 / (width + height) as f32) * 200.0) as u8 + 55;

                // Add grid lines
                let grid = (x % 40 < 2) || (y % 40 < 2);
                let brightness: f32 = if grid { 1.5 } else { 1.0 };

                data[idx] = (r as f32 * brightness).min(255.0) as u8;
                data[idx + 1] = (g as f32 * brightness).min(255.0) as u8;
                data[idx + 2] = (b as f32 * brightness).min(255.0) as u8;
                data[idx + 3] = 255;
            }
        }

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.source_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_size),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }
}

pub fn main() {
    slint::BackendSelector::new()
        .require_wgpu_28(WGPUConfiguration::Automatic(WGPUSettings::default()))
        .select()
        .expect("Unable to create Slint backend with WGPU renderer");

    let app = PostprocessApp::new().unwrap();

    let mut processor: Option<CrtPostProcessor> = None;
    let app_weak = app.as_weak();

    app.window()
        .set_rendering_notifier(move |state, graphics_api| {
            match state {
                slint::RenderingState::RenderingSetup => {
                    if let slint::GraphicsAPI::WGPU28 { device, queue, .. } = graphics_api {
                        processor = Some(CrtPostProcessor::new(device, queue));
                    }
                }
                slint::RenderingState::BeforeRendering => {
                    if let (Some(proc), Some(app)) = (processor.as_mut(), app_weak.upgrade()) {
                        let texture = proc.render(
                            app.get_requested_texture_width() as u32,
                            app.get_requested_texture_height() as u32,
                            app.get_scanline_intensity(),
                            app.get_curvature(),
                            app.get_chromatic_aberration(),
                            app.get_vignette_intensity(),
                        );
                        app.set_crt_texture(slint::Image::try_from(texture).unwrap());
                        app.window().request_redraw();
                    }
                }
                slint::RenderingState::AfterRendering => {}
                slint::RenderingState::RenderingTeardown => {
                    drop(processor.take());
                }
                _ => {}
            }
        })
        .expect("Unable to set rendering notifier");

    app.run().unwrap();
}
