// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

//! # FemtoVG WGPU Integration Example
//!
//! Demonstrates how to use Slint's `FemtoVGWGPURenderer` for direct GPU rendering
//! without a game engine, in two modes:
//!
//! ## Headless Screenshot Mode
//! ```bash
//! cargo run -p femtovg_wgpu -- --screenshot output.png
//! ```
//! Renders the Slint UI to a PNG file without opening a window. Useful for CI
//! visual regression testing or documentation screenshots.
//!
//! ## Windowed Compositing Mode (default)
//! ```bash
//! cargo run -p femtovg_wgpu
//! ```
//! Opens a window with an animated wgpu background and the Slint UI composited
//! on top. Demonstrates the "embed Slint in your own render loop" pattern.

mod headless;
mod readback;
mod windowed;

slint::include_modules!();

use slint::ComponentHandle;
use wgpu_28 as wgpu;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Check for --screenshot mode
    let screenshot_path = args.windows(2).find_map(|pair| {
        if pair[0] == "--screenshot" {
            Some(pair[1].clone())
        } else {
            None
        }
    });

    if let Some(path) = screenshot_path {
        run_headless_screenshot(&path);
    } else {
        windowed::run();
    }
}

fn run_headless_screenshot(output_path: &str) {
    let (instance, device, queue) = pollster::block_on(create_wgpu_device());

    // Set up the headless Slint platform
    let platform = headless::HeadlessPlatform::new(instance, device.clone(), queue.clone());
    slint::platform::set_platform(Box::new(platform)).expect("Failed to set platform");

    // Create and show the Slint UI
    let app = Scene::new().expect("Failed to create Scene");
    let window = app.window();
    window.show().expect("Failed to show window");

    let width = 800u32;
    let height = 600u32;
    let scale_factor = 1.0f32;

    // Dispatch a resize so the layout is computed
    window.dispatch_event(slint::platform::WindowEvent::Resized {
        size: slint::LogicalSize::new(width as f32 / scale_factor, height as f32 / scale_factor),
    });

    slint::platform::update_timers_and_animations();

    // Create a texture to render Slint UI into
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

    // Retrieve the HeadlessWindowAdapter to access the FemtoVGWGPURenderer
    let adapter = headless::last_adapter().expect("No adapter created");
    adapter.resize(slint::PhysicalSize::new(width, height), scale_factor);

    // Render the Slint UI directly to the GPU texture
    adapter.renderer.render_to_texture(&texture).expect("Failed to render to texture");

    // Read pixels back from GPU to CPU
    let pixels = readback::read_texture_to_pixels(&device, &queue, &texture);

    // Save to PNG
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
