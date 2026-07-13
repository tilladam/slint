// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

//! Headless reproduction harness for the Skia + WGPU offscreen color path.
//!
//! Renders a couple of known solid colors and a black→white gradient through Slint's
//! `SkiaWGPURenderer::render_to_texture()` into an offscreen wgpu texture, then does a raw
//! `copy_texture_to_buffer` + mapped read of the texture and compares the bytes against the
//! literal color values. The raw readback bypasses any downstream sampling/tonemap — the bytes
//! it prints are exactly what Skia wrote into the texture.
//!
//! The target texture format is selectable via `ORACLE_FORMAT`. The wrap-site `SkColorType` and
//! `SkColorSpace` used in `make_metal_surface` are selectable via `SKIA_CT` / `SKIA_CS` (this
//! branch adds those env hooks to `internal/renderers/skia/wgpu_29_surface/metal.rs`), so the
//! sRGB double-encode and the candidate fixes can be reproduced directly:
//!
//! ```sh
//! # baseline: non-sRGB target renders the literal sRGB bytes
//! ORACLE_FORMAT=rgba8unorm     cargo run -p skia-wgpu-color-oracle
//! # bug: sRGB target double-encodes (dark navy 9,15,24 -> 53,69,86)
//! ORACLE_FORMAT=rgba8unormsrgb cargo run -p skia-wgpu-color-oracle
//! # candidate fix: linear color space -> correct solids, but gamma-correct (linear) blending
//! ORACLE_FORMAT=rgba8unormsrgb SKIA_CT=srgba8888 SKIA_CS=linear cargo run -p skia-wgpu-color-oracle
//! ```

use std::cell::Cell;
use std::rc::{Rc, Weak};

use slint::PhysicalSize;
use slint::platform::WindowEvent;
use slint::platform::skia_renderer::SkiaWGPURenderer;
use wgpu_29 as wgpu;

const W: u32 = 64;
const H: u32 = 64;

slint::slint! {
    export component Demo inherits Window {
        // Opaque dark navy (#090f18) — a mid-range color where the sRGB double-encode is obvious.
        // Rendered via the Window clear-color fast path.
        background: #090f18;
        // A mid-tone gray solid fill (draw path, not clear path). Gray 128 sits on the steep
        // part of the sRGB curve, so an unwanted encode is very visible (128 -> ~188).
        Rectangle {
            x: 0px;
            y: 0px;
            width: 32px;
            height: 32px;
            background: #808080;
        }
        // Black -> white horizontal gradient. Its spatial midpoint is a 50% color blend, whose
        // stored value reveals the blend space: (128,128,128) if blended in sRGB space (matches
        // the software renderer), (188,188,188) if blended in linear space.
        Rectangle {
            x: 0px;
            y: 48px;
            width: 64px;
            height: 16px;
            background: @linear-gradient(90deg, #000000 0%, #ffffff 100%);
        }
    }
}

// --- Minimal offscreen platform / window adapter (self-contained) --------------------------------

thread_local! {
    static ADAPTER: std::cell::RefCell<Option<Weak<OffscreenAdapter>>> =
        const { std::cell::RefCell::new(None) };
}

struct OffscreenAdapter {
    size: Cell<PhysicalSize>,
    slint_window: slint::Window,
    renderer: SkiaWGPURenderer,
}

impl slint::platform::WindowAdapter for OffscreenAdapter {
    fn window(&self) -> &slint::Window {
        &self.slint_window
    }
    fn size(&self) -> PhysicalSize {
        self.size.get()
    }
    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }
    fn set_visible(&self, _visible: bool) -> Result<(), slint::PlatformError> {
        Ok(())
    }
    fn request_redraw(&self) {}
}

impl OffscreenAdapter {
    fn new(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Rc<Self> {
        let renderer = SkiaWGPURenderer::new(instance, adapter, device, queue)
            .expect("Failed to create SkiaWGPURenderer");
        Rc::new_cyclic(|self_weak: &Weak<Self>| Self {
            size: Cell::new(PhysicalSize::new(W, H)),
            slint_window: slint::Window::new(self_weak.clone()),
            renderer,
        })
    }
}

struct OffscreenPlatform {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl slint::platform::Platform for OffscreenPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        let adapter = OffscreenAdapter::new(
            self.instance.clone(),
            self.adapter.clone(),
            self.device.clone(),
            self.queue.clone(),
        );
        ADAPTER.with(|a| *a.borrow_mut() = Some(Rc::downgrade(&adapter)));
        Ok(adapter)
    }
}

// --- sRGB encode (to label a reading as "bug" vs "correct") ---------------------------------

fn srgb_encode_linear_to_srgb(u: f64) -> f64 {
    if u <= 0.0031308 { u * 12.92 } else { 1.055 * u.powf(1.0 / 2.4) - 0.055 }
}
fn enc8(v: u8) -> u8 {
    (srgb_encode_linear_to_srgb(v as f64 / 255.0) * 255.0).round() as u8
}

fn main() {
    // `format`'s derived Debug prints the variant name verbatim (e.g. `Rgba8Unorm`), so it
    // doubles as the display name; BGRA byte order is derived from it at the sample site.
    let format = match std::env::var("ORACLE_FORMAT").as_deref() {
        Ok("rgba8unormsrgb") => wgpu::TextureFormat::Rgba8UnormSrgb,
        Ok("bgra8unorm") => wgpu::TextureFormat::Bgra8Unorm,
        Ok("rgba8unorm") | Err(_) => wgpu::TextureFormat::Rgba8Unorm,
        Ok(other) => panic!("Unknown ORACLE_FORMAT={other:?}"),
    };

    // --- wgpu: Metal instance/adapter/device/queue, no surface (headless) -------------------
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::METAL,
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let adapter = spin_on::spin_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no Metal adapter");
    let (device, queue) = spin_on::spin_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("oracle device"),
        required_features: adapter.features() - wgpu::Features::all_experimental_mask(),
        required_limits: adapter.limits(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::default(),
    }))
    .expect("request_device failed");

    println!("== Skia+WGPU offscreen color oracle ==");
    println!("adapter: {:?}", adapter.get_info().backend);
    println!("texture format: {format:?}");

    // --- Slint platform + component ---------------------------------------------------------
    slint::platform::set_platform(Box::new(OffscreenPlatform {
        instance: instance.clone(),
        adapter: adapter.clone(),
        device: device.clone(),
        queue: queue.clone(),
    }))
    .expect("set_platform");

    let demo = Demo::new().expect("Demo::new");
    demo.window().show().expect("show");

    // `demo.window()` is the OffscreenAdapter's window; we retrieve the adapter only to reach
    // its `renderer` (Slint exposes no getter for that from the window).
    let adapter_rc =
        ADAPTER.with(|a| a.borrow().as_ref().and_then(Weak::upgrade)).expect("no adapter created");
    let window = demo.window();
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));
    // scale factor 1.0 => physical px == logical px == texture px
    window.dispatch_event(WindowEvent::Resized { size: PhysicalSize::new(W, H).to_logical(1.0) });
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 1.0 });

    slint::platform::update_timers_and_animations();

    // --- Offscreen texture, render into it --------------------------------------------------
    // ORACLE_VIEW_SRGB=1 adds `Rgba8UnormSrgb` to `view_formats` (base format stays whatever
    // ORACLE_FORMAT selects). On Metal a view_formats entry forces MTLTextureUsagePixelFormatView,
    // which could in principle change what `as_hal().raw_handle()` hands Skia — this lets you
    // check the write side (it doesn't: base `Rgba8Unorm` + this entry still stores literal bytes).
    let srgb_view = std::env::var("ORACLE_VIEW_SRGB").is_ok();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("oracle target"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: if srgb_view { &[wgpu::TextureFormat::Rgba8UnormSrgb] } else { &[] },
    });
    if srgb_view {
        println!("view_formats: [Rgba8UnormSrgb]");
    }

    // wgpu lazily zero-initializes a texture on its first use *through wgpu*. Skia writes into
    // this texture via raw Metal (outside wgpu's tracking), so without a prior wgpu write, our
    // later copy_texture_to_buffer would be the "first wgpu use" and wgpu would zero-init it,
    // clobbering Skia's content. Do a full-texture wgpu clear first to mark it initialized.
    // The sentinel color also disambiguates "Skia drew nothing" (readback == sentinel) from
    // "Skia drew" (readback == theme colors).
    {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sentinel") });
        {
            let _rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sentinel clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 200.0 / 255.0,
                            g: 100.0 / 255.0,
                            b: 50.0 / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit(Some(enc.finish()));
    }

    adapter_rc
        .renderer
        .render_to_texture(&texture)
        .expect("render_to_texture (this branch is based on master, which has no sRGB guard)");

    // --- Raw readback via copy_texture_to_buffer --------------------------------------------
    let bytes_per_row = W * 4; // 256 for W=64 (already 256-aligned)
    let buffer_size = (bytes_per_row * H) as u64;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("oracle readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    // Same queue that Skia shares (see make_metal_context) => this copy is ordered after the render.
    queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map_async"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    let data = slice.get_mapped_range();

    let is_bgra =
        matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
    let sample = |x: u32, y: u32| -> (u8, u8, u8, u8) {
        let off = (y * bytes_per_row + x * 4) as usize;
        let p = &data[off..off + 4];
        if is_bgra { (p[2], p[1], p[0], p[3]) } else { (p[0], p[1], p[2], p[3]) }
    };

    // Solid checks drive PASS/FAIL: (sample point, label, expected-correct literal). bg is sampled
    // at (40,40) to stay clear of both rects.
    let checks = [
        ((40u32, 40u32), "window bg  #090f18", (9u8, 15u8, 24u8)),
        ((16u32, 16u32), "gray fill  #808080", (128u8, 128u8, 128u8)),
    ];

    println!();
    let mut all_ok = true;
    for ((x, y), label, (er, eg, eb)) in checks {
        let (r, g, b, a) = sample(x, y);
        let correct = r == er && g == eg && b == eb;
        let bug_expect = (enc8(er), enc8(eg), enc8(eb));
        let is_bug = (r, g, b) == bug_expect;
        let verdict = if correct {
            "CORRECT (literal)"
        } else if is_bug {
            "BUG (extra sRGB encode)"
        } else {
            "OTHER (unexpected)"
        };
        all_ok &= correct;
        println!(
            "  {label} @({x:2},{y:2}): got ({r:3},{g:3},{b:3},{a:3})  expect-literal ({er:3},{eg:3},{eb:3})  expect-if-bug ({:3},{:3},{:3})  -> {verdict}",
            bug_expect.0, bug_expect.1, bug_expect.2,
        );
    }

    // Gradient midpoint is a blend-space probe, not a literal check — report it separately.
    // It's only a clean probe when the solids are correct: ~128 == sRGB-space blend (what the
    // software renderer / unmanaged paths do), 188 == linear-space blend. When the solids are
    // double-encoding, the sRGB-space midpoint (128) itself encodes to 188, so 188 is ambiguous
    // and the probe says nothing about blend space.
    let (gr, gg, gb, _) = sample(32, 56);
    let blend = if !all_ok {
        "blend space indeterminate here (solids are double-encoding; 188 could be either)"
    } else if gr.abs_diff(128) <= 2 {
        "sRGB-space blend (matches software / unmanaged paths)"
    } else if gr.abs_diff(188) <= 2 {
        "LINEAR-space blend (gamma-correct; diverges from software)"
    } else {
        "unexpected"
    };
    println!("  gradient mid (blend) @(32,56): got ({gr:3},{gg:3},{gb:3})  -> {blend}");

    println!();
    println!("RESULT[{format:?}] solids: {}", if all_ok { "PASS" } else { "FAIL" });

    drop(data);
    readback.unmap();
}
