// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! Node Editor items for building visual node graph editors.
//!
//! This module provides two native items that work together:
//! - `NodeEditorBackground`: Renders grid and static links (bezier curves)
//! - `NodeEditorOverlay`: Renders selection box, active link preview, handles input
//!
//! These are designed to be used in a three-layer architecture:
//! 1. NodeEditorBackground (bottom layer)
//! 2. Node children (Slint components, middle layer)
//! 3. NodeEditorOverlay (top layer)

use super::{
    Item, ItemConsts, ItemRc, ItemRendererRef, KeyEventResult, RenderingResult,
};
use crate::graphics::{Brush, Color};
use crate::input::{
    FocusEvent, FocusEventResult, InputEventFilterResult, InputEventResult, KeyEvent, MouseEvent,
    PointerEventButton,
};
use crate::item_rendering::CachedRenderingData;
use crate::layout::{LayoutInfo, Orientation};
use crate::lengths::{
    LogicalLength, LogicalPoint, LogicalRect, LogicalSize, LogicalVector,
};
#[cfg(feature = "rtti")]
use crate::rtti::*;
use crate::window::WindowAdapter;
use crate::{Callback, Coord, Property};
use alloc::boxed::Box;
use alloc::rc::Rc;
use const_field_offset::FieldOffsets;
use core::cell::RefCell;
use core::pin::Pin;
use i_slint_core_macros::*;

// Note: Complex callbacks with multiple parameters are not yet supported by the RTTI system.
// For now, we use simple void callbacks. The actual event data can be retrieved via properties.
// TODO: Add proper callback argument types once builtin_structs integration is done.

/// Internal state for the node editor overlay
#[derive(Default, Debug)]
struct NodeEditorState {
    /// Whether we're currently panning
    is_panning: bool,
    /// Position where panning started (in screen coordinates)
    pan_start_pos: LogicalPoint,
    /// Pan offset when panning started
    pan_start_offset: LogicalVector,
    /// Whether we're currently creating a link
    is_creating_link: bool,
    /// Pin ID from which link creation started
    link_start_pin: i32,
    /// Current mouse position during link creation
    link_current_pos: LogicalPoint,
    /// Whether we're box selecting
    is_box_selecting: bool,
    /// Start position of box selection
    box_select_start: LogicalPoint,
    /// Current position of box selection
    box_select_current: LogicalPoint,
}

/// Wraps the internal state properly with RefCell
#[repr(C)]
pub struct NodeEditorData {
    state: RefCell<NodeEditorState>,
}

impl Default for NodeEditorData {
    fn default() -> Self {
        Self {
            state: RefCell::new(NodeEditorState::default()),
        }
    }
}

#[repr(C)]
pub struct NodeEditorDataBox(core::ptr::NonNull<NodeEditorData>);

impl Default for NodeEditorDataBox {
    fn default() -> Self {
        NodeEditorDataBox(Box::leak(Box::<NodeEditorData>::default()).into())
    }
}

impl Drop for NodeEditorDataBox {
    fn drop(&mut self) {
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

impl core::ops::Deref for NodeEditorDataBox {
    type Target = NodeEditorData;
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

// ============================================================================
// NodeEditorBackground
// ============================================================================

/// The background layer of a node editor.
/// Renders the grid and static (established) links.
#[repr(C)]
#[derive(FieldOffsets, Default, SlintElement)]
#[pin]
pub struct NodeEditorBackground {
    /// Spacing between grid lines
    pub grid_spacing: Property<LogicalLength>,
    /// Color of the grid lines
    pub grid_color: Property<Color>,
    /// Background color of the canvas
    pub background_color: Property<Brush>,
    /// Current pan offset X
    pub pan_x: Property<LogicalLength>,
    /// Current pan offset Y
    pub pan_y: Property<LogicalLength>,
    /// Current zoom level
    pub zoom: Property<f32>,

    pub cached_rendering_data: CachedRenderingData,
}

impl Item for NodeEditorBackground {
    fn init(self: Pin<&Self>, _self_rc: &ItemRc) {}

    fn layout_info(
        self: Pin<&Self>,
        _orientation: Orientation,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> LayoutInfo {
        LayoutInfo { stretch: 1., ..LayoutInfo::default() }
    }

    fn input_event_filter_before_children(
        self: Pin<&Self>,
        _: &MouseEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> InputEventFilterResult {
        // Background doesn't handle input - let it pass through to children
        InputEventFilterResult::ForwardAndIgnore
    }

    fn input_event(
        self: Pin<&Self>,
        _: &MouseEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> InputEventResult {
        InputEventResult::EventIgnored
    }

    fn capture_key_event(
        self: Pin<&Self>,
        _: &KeyEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        KeyEventResult::EventIgnored
    }

    fn key_event(
        self: Pin<&Self>,
        _: &KeyEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        KeyEventResult::EventIgnored
    }

    fn focus_event(
        self: Pin<&Self>,
        _: &FocusEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> FocusEventResult {
        FocusEventResult::FocusIgnored
    }

    fn render(
        self: Pin<&Self>,
        backend: &mut ItemRendererRef,
        _self_rc: &ItemRc,
        size: LogicalSize,
    ) -> RenderingResult {
        // Draw background
        let background = self.background_color();
        if !background.is_transparent() {
            // We'll use the item renderer to draw rectangles
            // For now, just continue rendering children
        }

        // Draw grid
        self.render_grid(backend, size);

        // Note: Links are rendered using Path components in Slint,
        // placed between the background and overlay layers.

        RenderingResult::ContinueRenderingChildren
    }

    fn bounding_rect(
        self: core::pin::Pin<&Self>,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
        geometry: LogicalRect,
    ) -> LogicalRect {
        geometry
    }

    fn clips_children(self: core::pin::Pin<&Self>) -> bool {
        false
    }
}

impl NodeEditorBackground {
    fn render_grid(self: Pin<&Self>, _backend: &mut ItemRendererRef, size: LogicalSize) {
        let grid_spacing = self.grid_spacing();
        let grid_color = self.grid_color();
        let pan_x = self.pan_x();
        let pan_y = self.pan_y();
        let zoom = self.zoom().max(0.1); // Prevent division by zero

        if grid_spacing.get() <= 0.0 || grid_color.alpha() == 0 {
            return;
        }

        // Calculate effective grid spacing with zoom
        let effective_spacing = LogicalLength::new(grid_spacing.get() * zoom);

        // Skip if spacing is too small to be visible
        if effective_spacing.get() < 4.0 {
            return;
        }

        // Calculate grid offset based on pan
        let offset_x = pan_x.get() % effective_spacing.get();
        let offset_y = pan_y.get() % effective_spacing.get();

        // TODO: Implement actual grid line rendering
        // This requires adding a draw_line method to ItemRenderer or using draw_rectangle
        // with very thin rectangles. For now, we'll add this in Phase 1 completion.

        // The grid will be rendered as a series of thin rectangles
        // Vertical lines
        let mut x = offset_x;
        let _line_width = LogicalLength::new(1.0);
        while x < size.width {
            // Draw vertical line at x
            // backend.draw_rectangle(...)
            x += effective_spacing.get();
        }

        // Horizontal lines
        let mut y = offset_y;
        while y < size.height {
            // Draw horizontal line at y
            // backend.draw_rectangle(...)
            y += effective_spacing.get();
        }
    }
}

impl ItemConsts for NodeEditorBackground {
    const cached_rendering_data_offset: const_field_offset::FieldOffset<
        NodeEditorBackground,
        CachedRenderingData,
    > = NodeEditorBackground::FIELD_OFFSETS.cached_rendering_data.as_unpinned_projection();
}

// ============================================================================
// NodeEditorOverlay
// ============================================================================

/// The overlay layer of a node editor.
/// Handles input and exposes state for Slint-rendered overlays (selection box, etc.).
#[repr(C)]
#[derive(FieldOffsets, Default, SlintElement)]
#[pin]
pub struct NodeEditorOverlay {
    /// Current pan offset X (bidirectional with Background)
    pub pan_x: Property<LogicalLength>,
    /// Current pan offset Y (bidirectional with Background)
    pub pan_y: Property<LogicalLength>,
    /// Current zoom level
    pub zoom: Property<f32>,
    /// Minimum zoom level
    pub min_zoom: Property<f32>,
    /// Maximum zoom level
    pub max_zoom: Property<f32>,

    /// Selection box color (deprecated - use Slint Rectangle instead)
    pub selection_box_color: Property<Color>,
    /// Active link color during creation (deprecated - use Slint Path instead)
    pub active_link_color: Property<Color>,

    /// Enable minimap
    pub minimap_enabled: Property<bool>,

    // === Selection box state (for Slint rendering) ===
    /// Whether a box selection is currently active
    pub is_selecting: Property<bool>,
    /// Selection box X coordinate (min of start and current)
    pub selection_x: Property<LogicalLength>,
    /// Selection box Y coordinate (min of start and current)
    pub selection_y: Property<LogicalLength>,
    /// Selection box width
    pub selection_width: Property<LogicalLength>,
    /// Selection box height
    pub selection_height: Property<LogicalLength>,

    // === Context menu state ===
    /// X coordinate where context menu was requested
    pub context_menu_x: Property<LogicalLength>,
    /// Y coordinate where context menu was requested
    pub context_menu_y: Property<LogicalLength>,

    // === Active link creation state (for Slint rendering) ===
    /// Whether a link is currently being created
    pub is_creating_link: Property<bool>,
    /// X coordinate of link start (where drag started)
    pub link_start_x: Property<LogicalLength>,
    /// Y coordinate of link start
    pub link_start_y: Property<LogicalLength>,
    /// Current X coordinate of link end (mouse position)
    pub link_end_x: Property<LogicalLength>,
    /// Current Y coordinate of link end (mouse position)
    pub link_end_y: Property<LogicalLength>,
    /// ID of the pin from which link creation started
    pub link_start_pin_id: Property<i32>,

    /// Callback when a link is created (use properties to get event data)
    pub link_created: Callback<()>,
    /// Callback when a link is dropped on empty space (use properties to get event data)
    pub link_dropped: Callback<()>,
    /// Callback when link creation is cancelled (use properties to get event data)
    pub link_cancelled: Callback<()>,
    /// Callback for context menu (use properties to get event data)
    pub context_menu_requested: Callback<()>,
    /// Callback when box selection completes
    pub selection_changed: Callback<()>,
    /// Callback when viewport changes (pan or zoom)
    pub viewport_changed: Callback<()>,

    // === Link creation trigger (set properties then call callback) ===
    /// Pin ID to start link from (set by Pin component before calling start-link)
    pub pending_link_pin_id: Property<i32>,
    /// X position to start link from (set by Pin component before calling start-link)
    pub pending_link_x: Property<LogicalLength>,
    /// Y position to start link from (set by Pin component before calling start-link)
    pub pending_link_y: Property<LogicalLength>,
    /// Callback to start link creation - set pending_link_* properties first
    pub start_link: Callback<()>,

    /// Internal state
    data: NodeEditorDataBox,

    pub cached_rendering_data: CachedRenderingData,
}

impl Item for NodeEditorOverlay {
    fn init(self: Pin<&Self>, _self_rc: &ItemRc) {}

    fn layout_info(
        self: Pin<&Self>,
        _orientation: Orientation,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> LayoutInfo {
        LayoutInfo { stretch: 1., ..LayoutInfo::default() }
    }

    fn input_event_filter_before_children(
        self: Pin<&Self>,
        _event: &MouseEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> InputEventFilterResult {
        // Check if a Pin component wants to start link creation
        // (Pin sets pending_link_pin_id > 0 then we start link creation)
        let pending_pin = self.pending_link_pin_id();
        if pending_pin > 0 {
            let mut state = self.data.state.borrow_mut();
            if !state.is_creating_link {
                // Start link creation from the pending properties
                let start_x = self.pending_link_x().get();
                let start_y = self.pending_link_y().get();

                state.is_creating_link = true;
                state.link_start_pin = pending_pin;
                state.link_current_pos = LogicalPoint::new(start_x, start_y);
                drop(state);

                // Update link creation properties for Slint rendering
                Self::FIELD_OFFSETS.is_creating_link.apply_pin(self).set(true);
                Self::FIELD_OFFSETS.link_start_pin_id.apply_pin(self).set(pending_pin);
                Self::FIELD_OFFSETS.link_start_x.apply_pin(self).set(LogicalLength::new(start_x));
                Self::FIELD_OFFSETS.link_start_y.apply_pin(self).set(LogicalLength::new(start_y));
                Self::FIELD_OFFSETS.link_end_x.apply_pin(self).set(LogicalLength::new(start_x));
                Self::FIELD_OFFSETS.link_end_y.apply_pin(self).set(LogicalLength::new(start_y));

                // Clear the pending trigger
                Self::FIELD_OFFSETS.pending_link_pin_id.apply_pin(self).set(0);
            }
        }

        // The overlay is on top, so we need to decide whether to handle the event
        // or pass it through to the nodes below
        let state = self.data.state.borrow();

        // If we're in the middle of an interaction, intercept
        if state.is_panning || state.is_creating_link || state.is_box_selecting {
            return InputEventFilterResult::Intercept;
        }

        // Check if this is a Ctrl+left click (box selection) or middle mouse (pan)
        // These should be intercepted by the overlay
        if let MouseEvent::Pressed { button, .. } = _event {
            match button {
                PointerEventButton::Middle => {
                    return InputEventFilterResult::Intercept;
                }
                PointerEventButton::Left => {
                    let modifiers = _window_adapter.window().0.modifiers.get();
                    if modifiers.control() {
                        return InputEventFilterResult::Intercept;
                    }
                }
                PointerEventButton::Right => {
                    return InputEventFilterResult::Intercept;
                }
                _ => {}
            }
        }

        // For scroll events, intercept for zoom
        if let MouseEvent::Wheel { .. } = _event {
            return InputEventFilterResult::Intercept;
        }

        // Otherwise, let events pass through to nodes behind the overlay
        InputEventFilterResult::ForwardAndIgnore
    }

    fn input_event(
        self: Pin<&Self>,
        event: &MouseEvent,
        window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> InputEventResult {
        match event {
            MouseEvent::Pressed { position, button, .. } => {
                self.handle_mouse_pressed(*position, *button, window_adapter)
            }
            MouseEvent::Released { position, button, .. } => {
                self.handle_mouse_released(*position, *button)
            }
            MouseEvent::Moved { position, .. } => {
                self.handle_mouse_moved(*position)
            }
            MouseEvent::Wheel { position, delta_x, delta_y, .. } => {
                self.handle_mouse_wheel(*position, *delta_x, *delta_y)
            }
            MouseEvent::Exit => {
                self.handle_mouse_exit()
            }
            _ => InputEventResult::EventIgnored,
        }
    }

    fn capture_key_event(
        self: Pin<&Self>,
        event: &KeyEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        // Handle Escape to cancel link creation
        let state = self.data.state.borrow();
        if state.is_creating_link {
            if event.text.starts_with(crate::input::key_codes::Escape) {
                return KeyEventResult::EventAccepted;
            }
        }
        KeyEventResult::EventIgnored
    }

    fn key_event(
        self: Pin<&Self>,
        event: &KeyEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        // Handle Escape to cancel link creation
        if event.text.starts_with(crate::input::key_codes::Escape) {
            let mut state = self.data.state.borrow_mut();
            if state.is_creating_link {
                state.is_creating_link = false;
                state.link_start_pin = -1;
                drop(state);

                // Clear link creation properties
                Self::FIELD_OFFSETS.is_creating_link.apply_pin(self).set(false);
                self.link_cancelled.call(&());
                return KeyEventResult::EventAccepted;
            }
        }
        KeyEventResult::EventIgnored
    }

    fn focus_event(
        self: Pin<&Self>,
        _: &FocusEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> FocusEventResult {
        FocusEventResult::FocusIgnored
    }

    fn render(
        self: Pin<&Self>,
        backend: &mut ItemRendererRef,
        _self_rc: &ItemRc,
        size: LogicalSize,
    ) -> RenderingResult {
        let state = self.data.state.borrow();

        // Render selection box if active
        if state.is_box_selecting {
            self.render_selection_box(backend, &state, size);
        }

        // Render active link preview if creating a link
        if state.is_creating_link {
            self.render_active_link(backend, &state, size);
        }

        // Render minimap if enabled
        if self.minimap_enabled() {
            self.render_minimap(backend, size);
        }

        RenderingResult::ContinueRenderingChildren
    }

    fn bounding_rect(
        self: core::pin::Pin<&Self>,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
        geometry: LogicalRect,
    ) -> LogicalRect {
        geometry
    }

    fn clips_children(self: core::pin::Pin<&Self>) -> bool {
        false
    }
}

impl NodeEditorOverlay {
    fn handle_mouse_pressed(
        self: Pin<&Self>,
        position: LogicalPoint,
        button: PointerEventButton,
        window_adapter: &Rc<dyn WindowAdapter>,
    ) -> InputEventResult {
        match button {
            PointerEventButton::Middle => {
                // Start panning
                let mut state = self.data.state.borrow_mut();
                state.is_panning = true;
                state.pan_start_pos = position;
                state.pan_start_offset = LogicalVector::from_lengths(
                    self.pan_x(),
                    self.pan_y(),
                );
                InputEventResult::GrabMouse
            }
            PointerEventButton::Right => {
                // Request context menu - store position in properties
                Self::FIELD_OFFSETS.context_menu_x.apply_pin(self).set(LogicalLength::new(position.x));
                Self::FIELD_OFFSETS.context_menu_y.apply_pin(self).set(LogicalLength::new(position.y));
                self.context_menu_requested.call(&());
                InputEventResult::EventAccepted
            }
            PointerEventButton::Left => {
                // Check if Ctrl is held for box selection (Shift is reserved for extend-selection)
                let modifiers = window_adapter.window().0.modifiers.get();
                if modifiers.control() {
                    // Ctrl+Left click starts box selection
                    let mut state = self.data.state.borrow_mut();
                    state.is_box_selecting = true;
                    state.box_select_start = position;
                    state.box_select_current = position;
                    drop(state);

                    // Update selection properties for Slint rendering
                    Self::FIELD_OFFSETS.is_selecting.apply_pin(self).set(true);
                    Self::FIELD_OFFSETS.selection_x.apply_pin(self).set(LogicalLength::new(position.x));
                    Self::FIELD_OFFSETS.selection_y.apply_pin(self).set(LogicalLength::new(position.y));
                    Self::FIELD_OFFSETS.selection_width.apply_pin(self).set(LogicalLength::new(0.0));
                    Self::FIELD_OFFSETS.selection_height.apply_pin(self).set(LogicalLength::new(0.0));

                    InputEventResult::GrabMouse
                } else {
                    // TODO: Implement proper hit testing for pins and nodes
                    // For now, pass through left clicks to allow node interaction.
                    // Without node registration, we can't distinguish background from node clicks.
                    InputEventResult::EventIgnored
                }
            }
            _ => InputEventResult::EventIgnored,
        }
    }

    fn handle_mouse_released(
        self: Pin<&Self>,
        _position: LogicalPoint, // TODO: Use for pin hit testing
        button: PointerEventButton,
    ) -> InputEventResult {
        let mut state = self.data.state.borrow_mut();

        match button {
            PointerEventButton::Middle => {
                if state.is_panning {
                    state.is_panning = false;
                    return InputEventResult::EventAccepted;
                }
            }
            PointerEventButton::Left => {
                if state.is_box_selecting {
                    state.is_box_selecting = false;
                    drop(state);

                    // Clear selection visual and emit callback
                    Self::FIELD_OFFSETS.is_selecting.apply_pin(self).set(false);
                    self.selection_changed.call(&());
                    return InputEventResult::EventAccepted;
                }
                if state.is_creating_link {
                    // TODO: Check if we're over a valid pin
                    // For now, emit link_dropped
                    state.is_creating_link = false;
                    state.link_start_pin = -1;
                    drop(state);

                    // Clear link creation visual
                    Self::FIELD_OFFSETS.is_creating_link.apply_pin(self).set(false);
                    self.link_dropped.call(&());
                    return InputEventResult::EventAccepted;
                }
            }
            _ => {}
        }

        InputEventResult::EventIgnored
    }

    fn handle_mouse_moved(self: Pin<&Self>, position: LogicalPoint) -> InputEventResult {
        let mut state = self.data.state.borrow_mut();

        if state.is_panning {
            // Update pan offset
            let delta = position - state.pan_start_pos;
            let new_pan = state.pan_start_offset + delta.cast();

            drop(state);

            // Update pan properties
            Self::FIELD_OFFSETS.pan_x.apply_pin(self).set(LogicalLength::new(new_pan.x));
            Self::FIELD_OFFSETS.pan_y.apply_pin(self).set(LogicalLength::new(new_pan.y));

            // Notify viewport change
            self.viewport_changed.call(&());

            return InputEventResult::GrabMouse;
        }

        if state.is_box_selecting {
            let start = state.box_select_start;
            state.box_select_current = position;
            drop(state);

            // Calculate selection box bounds (handle any drag direction)
            let min_x = start.x.min(position.x);
            let min_y = start.y.min(position.y);
            let max_x = start.x.max(position.x);
            let max_y = start.y.max(position.y);

            // Update selection properties for Slint rendering
            Self::FIELD_OFFSETS.selection_x.apply_pin(self).set(LogicalLength::new(min_x));
            Self::FIELD_OFFSETS.selection_y.apply_pin(self).set(LogicalLength::new(min_y));
            Self::FIELD_OFFSETS.selection_width.apply_pin(self).set(LogicalLength::new(max_x - min_x));
            Self::FIELD_OFFSETS.selection_height.apply_pin(self).set(LogicalLength::new(max_y - min_y));

            return InputEventResult::GrabMouse;
        }

        if state.is_creating_link {
            state.link_current_pos = position;
            drop(state);

            // Update link end position for Slint rendering
            Self::FIELD_OFFSETS.link_end_x.apply_pin(self).set(LogicalLength::new(position.x));
            Self::FIELD_OFFSETS.link_end_y.apply_pin(self).set(LogicalLength::new(position.y));

            return InputEventResult::GrabMouse;
        }

        InputEventResult::EventIgnored
    }

    fn handle_mouse_wheel(
        self: Pin<&Self>,
        position: LogicalPoint,
        _delta_x: Coord,
        delta_y: Coord,
    ) -> InputEventResult {
        // Zoom centered on mouse position
        let current_zoom = self.zoom();
        let min_zoom = self.min_zoom();
        let max_zoom = self.max_zoom();

        // Calculate zoom factor
        let zoom_factor = if delta_y > 0.0 { 1.1 } else { 0.9 };
        let new_zoom = (current_zoom * zoom_factor).clamp(min_zoom, max_zoom);

        if (new_zoom - current_zoom).abs() > f32::EPSILON {
            // Adjust pan to zoom around mouse position
            let pan_x = self.pan_x().get();
            let pan_y = self.pan_y().get();

            // Convert mouse position to graph space before zoom
            let graph_x = (position.x - pan_x) / current_zoom;
            let graph_y = (position.y - pan_y) / current_zoom;

            // Calculate new pan to keep the point under the mouse
            let new_pan_x = position.x - graph_x * new_zoom;
            let new_pan_y = position.y - graph_y * new_zoom;

            Self::FIELD_OFFSETS.zoom.apply_pin(self).set(new_zoom);
            Self::FIELD_OFFSETS.pan_x.apply_pin(self).set(LogicalLength::new(new_pan_x));
            Self::FIELD_OFFSETS.pan_y.apply_pin(self).set(LogicalLength::new(new_pan_y));

            // Notify viewport change
            self.viewport_changed.call(&());

            return InputEventResult::EventAccepted;
        }

        InputEventResult::EventIgnored
    }

    fn handle_mouse_exit(self: Pin<&Self>) -> InputEventResult {
        let mut state = self.data.state.borrow_mut();

        // Cancel any ongoing interactions
        let was_panning = state.is_panning;
        let was_box_selecting = state.is_box_selecting;
        let was_creating_link = state.is_creating_link;

        if was_panning {
            state.is_panning = false;
        }
        if was_box_selecting {
            state.is_box_selecting = false;
        }
        if was_creating_link {
            state.is_creating_link = false;
            state.link_start_pin = -1;
        }
        drop(state);

        // Update properties and call callbacks after releasing the borrow
        if was_box_selecting {
            Self::FIELD_OFFSETS.is_selecting.apply_pin(self).set(false);
            return InputEventResult::EventAccepted;
        }
        if was_creating_link {
            Self::FIELD_OFFSETS.is_creating_link.apply_pin(self).set(false);
            self.link_cancelled.call(&());
            return InputEventResult::EventAccepted;
        }

        InputEventResult::EventIgnored
    }

    fn render_selection_box(
        self: Pin<&Self>,
        _backend: &mut ItemRendererRef,
        state: &NodeEditorState,
        _size: LogicalSize,
    ) {
        let _start = state.box_select_start;
        let _current = state.box_select_current;
        let _color = self.selection_box_color();

        // TODO: Draw selection rectangle
        // Calculate min/max to handle any drag direction
        // let rect = LogicalRect::from_points(start, current);
        // Use backend to draw a semi-transparent rectangle with border
    }

    fn render_active_link(
        self: Pin<&Self>,
        _backend: &mut ItemRendererRef,
        state: &NodeEditorState,
        _size: LogicalSize,
    ) {
        let _current_pos = state.link_current_pos;
        let _color = self.active_link_color();

        // TODO: Draw bezier curve from link start pin to current mouse position
        // This requires knowing the start pin position, which comes from
        // the pin-position-changed callback
    }

    fn render_minimap(
        self: Pin<&Self>,
        _backend: &mut ItemRendererRef,
        _size: LogicalSize,
    ) {
        // TODO: Render simplified minimap showing node positions as rectangles
        // Position in bottom-right corner
    }

    /// Start creating a link from a pin
    pub fn start_link_creation(self: Pin<&Self>, pin_id: i32, position: LogicalPoint) {
        let mut state = self.data.state.borrow_mut();
        state.is_creating_link = true;
        state.link_start_pin = pin_id;
        state.link_current_pos = position;
    }
}

impl ItemConsts for NodeEditorOverlay {
    const cached_rendering_data_offset: const_field_offset::FieldOffset<
        NodeEditorOverlay,
        CachedRenderingData,
    > = NodeEditorOverlay::FIELD_OFFSETS.cached_rendering_data.as_unpinned_projection();
}

// ============================================================================
// FFI functions for NodeEditorDataBox
// ============================================================================

/// # Safety
/// This must be called using a non-null pointer pointing to a chunk of memory big enough to
/// hold a NodeEditorDataBox
#[cfg(feature = "ffi")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_node_editor_data_init(data: *mut NodeEditorDataBox) {
    unsafe { core::ptr::write(data, NodeEditorDataBox::default()) };
}

/// # Safety
/// This must be called using a non-null pointer pointing to an initialized NodeEditorDataBox
#[cfg(feature = "ffi")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slint_node_editor_data_free(data: *mut NodeEditorDataBox) {
    unsafe {
        core::ptr::drop_in_place(data);
    }
}
