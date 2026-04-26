// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

//! Headless platform implementation for FemtoVG WGPU rendering without an OS window.
//!
//! Provides [`HeadlessPlatform`] and [`HeadlessWindowAdapter`] that wrap
//! [`FemtoVGWGPURenderer`] for offscreen rendering to wgpu textures.

use slint::platform::femtovg_renderer::FemtoVGWGPURenderer;
use slint::platform::{
    ChannelEventLoopProxy, EventLoopProxy, Platform, WindowAdapter, WindowEvent,
};
use slint::{PhysicalSize, Window};
use std::cell::Cell;
use std::rc::{Rc, Weak};
use wgpu_28 as wgpu;

thread_local! {
    /// Stores a weak reference to the last created adapter for retrieval after
    /// `Scene::new()` calls `create_window_adapter()` internally.
    static LAST_ADAPTER: std::cell::RefCell<Option<Weak<HeadlessWindowAdapter>>> =
        std::cell::RefCell::new(None);
}

/// Retrieve the most recently created [`HeadlessWindowAdapter`].
pub fn last_adapter() -> Option<Rc<HeadlessWindowAdapter>> {
    LAST_ADAPTER.with(|a| a.borrow().as_ref().and_then(|w| w.upgrade()))
}

/// A Slint window adapter that renders via FemtoVG to wgpu textures (no OS window).
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
        self.size.set(size);
        self.scale_factor.set(scale_factor);
        self.window
            .dispatch_event(WindowEvent::Resized { size: size.to_logical(scale_factor) });
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

/// A Slint platform that creates headless window adapters with FemtoVG WGPU rendering.
pub struct HeadlessPlatform {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    proxy: Option<ChannelEventLoopProxy>,
}

impl HeadlessPlatform {
    pub fn new(instance: wgpu::Instance, device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self { instance, device, queue, proxy: None }
    }

    pub fn with_proxy(mut self, proxy: ChannelEventLoopProxy) -> Self {
        self.proxy = Some(proxy);
        self
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

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        self.proxy.as_ref().map(|p| Box::new(p.clone()) as Box<dyn EventLoopProxy>)
    }

    fn duration_since_start(&self) -> core::time::Duration {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed()
    }
}
