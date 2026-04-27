// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

//! Headless platform for FemtoVG WGPU rendering without an OS window.

use slint::platform::femtovg_renderer::FemtoVGWGPURenderer;
use slint::platform::{Platform, WindowAdapter};
use slint::{PhysicalSize, Window};
use std::cell::Cell;
use std::rc::{Rc, Weak};
use wgpu_28 as wgpu;

thread_local! {
    static LAST_ADAPTER: std::cell::RefCell<Option<Weak<HeadlessWindowAdapter>>> =
        std::cell::RefCell::new(None);
}

pub fn last_adapter() -> Option<Rc<HeadlessWindowAdapter>> {
    LAST_ADAPTER.with(|a| a.borrow().as_ref().and_then(|w| w.upgrade()))
}

pub struct HeadlessWindowAdapter {
    size: Cell<PhysicalSize>,
    scale_factor: Cell<f32>,
    window: Window,
    pub renderer: FemtoVGWGPURenderer,
}

impl HeadlessWindowAdapter {
    pub fn new(instance: wgpu::Instance, device: wgpu::Device, queue: wgpu::Queue) -> Rc<Self> {
        let renderer =
            FemtoVGWGPURenderer::new(instance, device, queue).expect("Failed to create renderer");
        Rc::new_cyclic(|self_weak: &Weak<Self>| Self {
            size: Cell::new(PhysicalSize::new(800, 600)),
            scale_factor: Cell::new(1.0),
            window: Window::new(self_weak.clone()),
            renderer,
        })
    }

    pub fn resize(&self, size: PhysicalSize, scale_factor: f32) {
        use slint::platform::WindowEvent;
        self.size.set(size);
        self.scale_factor.set(scale_factor);
        self.window.dispatch_event(WindowEvent::Resized { size: size.to_logical(scale_factor) });
        self.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor });
    }
}

impl WindowAdapter for HeadlessWindowAdapter {
    fn window(&self) -> &Window {
        &self.window
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

pub struct HeadlessPlatform {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl HeadlessPlatform {
    pub fn new(instance: wgpu::Instance, device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self { instance, device, queue }
    }
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let adapter = HeadlessWindowAdapter::new(
            self.instance.clone(),
            self.device.clone(),
            self.queue.clone(),
        );
        LAST_ADAPTER.with(|a| {
            *a.borrow_mut() = Some(Rc::downgrade(&adapter));
        });
        Ok(adapter)
    }

    fn duration_since_start(&self) -> core::time::Duration {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed()
    }
}
