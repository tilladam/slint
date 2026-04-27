// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

//! # FemtoVG WGPU Headless Screenshot
//!
//! Renders the Slint UI offscreen to a PNG file using [`FemtoVGWGPURenderer`]
//! without opening a window. Useful for CI visual regression testing or
//! documentation screenshots.
//!
//! ```bash
//! cargo run -p femtovg_wgpu -- --screenshot output.png
//! ```

mod headless;
mod readback;

slint::include_modules!();

use slint::ComponentHandle;
use wgpu_28 as wgpu;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let output_path = args
        .windows(2)
        .find_map(|pair| (pair[0] == "--screenshot").then(|| pair[1].clone()))
        .unwrap_or_else(|| {
            eprintln!("Usage: femtovg_wgpu --screenshot <output.png>");
            std::process::exit(1);
        });

    run_headless_screenshot(&output_path);
}

fn run_headless_screenshot(output_path: &str) {
    let (instance, device, queue) = pollster::block_on(create_wgpu_device());

    let platform = headless::HeadlessPlatform::new(instance, device.clone(), queue.clone());
    slint::platform::set_platform(Box::new(platform)).expect("Failed to set platform");

    let app = Scene::new().expect("Failed to create Scene");
    let window = app.window();
    window.show().expect("Failed to show window");

    let width = 800u32;
    let height = 600u32;
    let scale_factor = 1.0f32;

    window.dispatch_event(slint::platform::WindowEvent::Resized {
        size: slint::LogicalSize::new(width as f32 / scale_factor, height as f32 / scale_factor),
    });

    slint::platform::update_timers_and_animations();

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot texture"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

    let adapter = headless::last_adapter().expect("No adapter created");
    adapter.resize(slint::PhysicalSize::new(width, height), scale_factor);
    adapter.renderer.render_to_texture(&texture).expect("Failed to render to texture");

    let pixels = readback::read_texture_to_pixels(&device, &queue, &texture);

    image::save_buffer(output_path, &pixels, width, height, image::ColorType::Rgba8)
        .expect("Failed to save PNG");

    println!("Screenshot saved to {output_path} ({width}x{height})");
}

async fn create_wgpu_device() -> (wgpu::Instance, wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .expect("Failed to find a suitable GPU adapter");

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("femtovg_wgpu example"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::default(),
        })
        .await
        .expect("Failed to create device");

    (instance, device, queue)
}
