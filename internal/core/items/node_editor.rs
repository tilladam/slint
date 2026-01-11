// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! Node Editor items for building visual node graph editors.
//!
//! This module provides two native items that work together in a three-layer architecture:
//!
//! 1. **NodeEditorBackground** (bottom layer)
//!    - Provides properties for viewport state (pan, zoom)
//!    - Grid rendering is the application's responsibility via Slint Path component
//!    - Links are rendered using Slint Path components placed in this layer
//!
//! 2. **Node children** (middle layer, Slint components)
//!    - Application defines node components with custom content
//!    - Nodes handle their own drag behavior and report selection to overlay
//!
//! 3. **NodeEditorOverlay** (top layer)
//!    - Handles input: pan (middle-mouse), zoom (scroll), box selection (ctrl+drag)
//!    - Exposes state properties for Slint to render overlays:
//!      - Box selection: `is-selecting`, `selection-x/y/width/height`
//!      - Link preview: `is-creating-link`, `link-start-x/y`, `link-end-x/y`
//!    - Fires callbacks: `viewport-changed`, `selection-changed`, `delete-selected`, etc.
//!
//! **Rendering Philosophy**: The native items handle input and state management.
//! Visual rendering (grid, links, selection box, link preview) is done using
//! Slint components (Rectangle, Path) that bind to the overlay's properties.

use super::{
    Item, ItemConsts, ItemRc, ItemRendererRef, KeyEventResult, RenderingResult,
};
use crate::graphics::{Brush, Color};
use crate::input::{
    FocusEvent, FocusEventResult, FocusReason, InputEventFilterResult, InputEventResult, KeyEvent,
    KeyEventType, MouseEvent, PointerEventButton,
};
use crate::item_rendering::CachedRenderingData;
use crate::layout::{LayoutInfo, Orientation};
use crate::lengths::{
    LogicalLength, LogicalPoint, LogicalRect, LogicalSize, LogicalVector,
};
#[cfg(feature = "rtti")]
use crate::rtti::*;
use crate::window::{WindowAdapter, WindowInner};
use crate::{Callback, Coord, Property};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
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
    /// Known pin positions for hit-testing (pin_id -> screen position)
    pin_positions: BTreeMap<i32, LogicalPoint>,
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
///
/// This provides viewport properties (pan, zoom) and serves as the container
/// for grid and link rendering via Slint Path components. The grid and links
/// are NOT rendered natively - applications should use Path components bound
/// to callbacks that generate SVG path data (see example's `generate_grid_commands`).
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
        _backend: &mut ItemRendererRef,
        _self_rc: &ItemRc,
        _size: LogicalSize,
    ) -> RenderingResult {
        // Grid rendering is the application's responsibility.
        // Use a Path component in Slint bound to grid commands generated
        // in Rust (see example's generate_grid_commands function).
        //
        // Links are also rendered using Path components in Slint,
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
///
/// Handles user input (pan, zoom, box selection, link creation) and exposes
/// state properties for Slint to render visual feedback:
///
/// - **Box selection**: `is-selecting`, `selection-x/y/width/height`
/// - **Link preview**: `is-creating-link`, `link-start-x/y`, `link-end-x/y`
///
/// Applications should use Slint Rectangle and Path components bound to these
/// properties to render the selection box and active link preview.
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
    /// Callback when delete key is pressed (Delete or Backspace)
    pub delete_selected: Callback<()>,
    /// Callback when add node shortcut is pressed (Ctrl+N)
    pub add_node_requested: Callback<()>,

    // === Link creation trigger (set properties then call callback) ===
    /// Pin ID to start link from (set by Pin component before calling start-link)
    pub pending_link_pin_id: Property<i32>,
    /// X position to start link from (set by Pin component before calling start-link)
    pub pending_link_x: Property<LogicalLength>,
    /// Y position to start link from (set by Pin component before calling start-link)
    pub pending_link_y: Property<LogicalLength>,
    /// Callback to start link creation - set pending_link_* properties first
    pub start_link: Callback<()>,

    // === Link completion trigger (for when pin TouchArea has mouse capture) ===
    /// Set to true to complete link creation
    pub complete_link_creation: Property<bool>,
    /// Target pin ID computed by Slint (0 if dropped on empty space)
    pub target_pin_id: Property<i32>,

    // === Pin position reporting (for hit-testing during link creation) ===
    /// Pin ID being reported (set before calling pin-position-changed)
    pub reporting_pin_id: Property<i32>,
    /// X position of pin being reported (in screen coordinates)
    pub reporting_pin_x: Property<LogicalLength>,
    /// Y position of pin being reported (in screen coordinates)
    pub reporting_pin_y: Property<LogicalLength>,
    /// Hit radius for pin detection
    pub pin_hit_radius: Property<LogicalLength>,
    /// Callback when a pin reports its position
    pub pin_position_changed: Callback<()>,

    // === Link creation result (output when link_created is fired) ===
    /// Start pin ID of the created link (output pin)
    pub created_link_start_pin: Property<i32>,
    /// End pin ID of the created link (input pin)
    pub created_link_end_pin: Property<i32>,

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

        // Check if a Pin component is reporting its position
        let reporting_pin = self.reporting_pin_id();
        if reporting_pin > 0 {
            let pin_x = self.reporting_pin_x().get();
            let pin_y = self.reporting_pin_y().get();

            // Store the pin position for hit-testing
            let mut state = self.data.state.borrow_mut();
            state.pin_positions.insert(reporting_pin, LogicalPoint::new(pin_x, pin_y));
            drop(state);

            // Clear the reporting trigger
            Self::FIELD_OFFSETS.reporting_pin_id.apply_pin(self).set(0);
        }

        // Check if a Pin component is requesting link completion (pin has mouse capture)
        if self.complete_link_creation() {
            // Get the target pin ID (pre-computed by Slint's find-pin-at function)
            let end_pin = self.target_pin_id();

            // Get the start pin from state
            let mut state = self.data.state.borrow_mut();
            let start_pin = state.link_start_pin;
            let was_creating = state.is_creating_link;
            state.is_creating_link = false;
            state.link_start_pin = -1;
            drop(state);

            // Clear the completion trigger and target pin
            Self::FIELD_OFFSETS.complete_link_creation.apply_pin(self).set(false);
            Self::FIELD_OFFSETS.target_pin_id.apply_pin(self).set(0);

            if was_creating {
                // Clear link creation visual
                Self::FIELD_OFFSETS.is_creating_link.apply_pin(self).set(false);

                if end_pin != 0 && Self::pins_compatible(start_pin, end_pin) {
                    // Normalize: output pin first, input pin second
                    let (output_pin, input_pin) = if start_pin % 10 == 2 {
                        (start_pin, end_pin)
                    } else {
                        (end_pin, start_pin)
                    };

                    // Set the created link properties
                    Self::FIELD_OFFSETS.created_link_start_pin.apply_pin(self).set(output_pin);
                    Self::FIELD_OFFSETS.created_link_end_pin.apply_pin(self).set(input_pin);

                    // Emit link_created callback
                    self.link_created.call(&());
                } else {
                    // Dropped on empty space or incompatible pin
                    self.link_dropped.call(&());
                }
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

        // Forward to children but still receive the event afterward to grab focus
        InputEventFilterResult::ForwardEvent
    }

    fn input_event(
        self: Pin<&Self>,
        event: &MouseEvent,
        window_adapter: &Rc<dyn WindowAdapter>,
        self_rc: &ItemRc,
    ) -> InputEventResult {
        match event {
            MouseEvent::Pressed { position, button, .. } => {
                self.handle_mouse_pressed(*position, *button, window_adapter, self_rc)
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
        // Only handle KeyPressed events
        if event.event_type != KeyEventType::KeyPressed {
            return KeyEventResult::EventIgnored;
        }

        // Handle Escape to cancel link creation
        let state = self.data.state.borrow();
        if state.is_creating_link {
            if event.text.starts_with(crate::input::key_codes::Escape) {
                return KeyEventResult::EventAccepted;
            }
        }

        // Handle Delete/Backspace for deletion - must be in capture_key_event
        // because returning EventAccepted here prevents key_event from being called
        if event.text.starts_with(crate::input::key_codes::Delete)
            || event.text.starts_with(crate::input::key_codes::Backspace)
        {
            self.delete_selected.call(&());
            return KeyEventResult::EventAccepted;
        }

        // Handle Ctrl+N for adding a new node
        if event.modifiers.control && event.text.eq_ignore_ascii_case("n") {
            self.add_node_requested.call(&());
            return KeyEventResult::EventAccepted;
        }

        KeyEventResult::EventIgnored
    }

    fn key_event(
        self: Pin<&Self>,
        event: &KeyEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> KeyEventResult {
        // Only handle KeyPressed events
        if event.event_type != KeyEventType::KeyPressed {
            return KeyEventResult::EventIgnored;
        }

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

        // Handle Delete or Backspace to delete selected items
        if event.text.starts_with(crate::input::key_codes::Delete)
            || event.text.starts_with(crate::input::key_codes::Backspace)
        {
            self.delete_selected.call(&());
            return KeyEventResult::EventAccepted;
        }

        KeyEventResult::EventIgnored
    }

    fn focus_event(
        self: Pin<&Self>,
        _: &FocusEvent,
        _window_adapter: &Rc<dyn WindowAdapter>,
        _self_rc: &ItemRc,
    ) -> FocusEventResult {
        // Accept focus so we can receive key events (Delete, Escape, etc.)
        FocusEventResult::FocusAccepted
    }

    fn render(
        self: Pin<&Self>,
        _backend: &mut ItemRendererRef,
        _self_rc: &ItemRc,
        _size: LogicalSize,
    ) -> RenderingResult {
        // Selection box, active link preview, and minimap are rendered
        // in Slint using Rectangle and Path components bound to this
        // overlay's properties (is-selecting, selection-x/y/width/height,
        // is-creating-link, link-start-x/y, link-end-x/y, etc.).
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
        self_rc: &ItemRc,
    ) -> InputEventResult {
        // Grab focus so we can receive key events (Delete, Escape, etc.)
        WindowInner::from_pub(window_adapter.window()).set_focus_item(
            self_rc,
            true,
            FocusReason::PointerClick,
        );

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
        position: LogicalPoint,
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
                    let start_pin = state.link_start_pin;
                    state.is_creating_link = false;
                    state.link_start_pin = -1;
                    drop(state);

                    // Check if we're over a valid pin
                    let end_pin = self.find_pin_at(position);

                    // Clear link creation visual
                    Self::FIELD_OFFSETS.is_creating_link.apply_pin(self).set(false);

                    if end_pin != 0 && Self::pins_compatible(start_pin, end_pin) {
                        // Normalize: output pin first, input pin second
                        let (output_pin, input_pin) = if start_pin % 10 == 2 {
                            (start_pin, end_pin)
                        } else {
                            (end_pin, start_pin)
                        };

                        // Set the created link properties
                        Self::FIELD_OFFSETS.created_link_start_pin.apply_pin(self).set(output_pin);
                        Self::FIELD_OFFSETS.created_link_end_pin.apply_pin(self).set(input_pin);

                        // Emit link_created callback
                        self.link_created.call(&());
                    } else {
                        // Dropped on empty space or incompatible pin
                        self.link_dropped.call(&());
                    }
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

    /// Start creating a link from a pin
    pub fn start_link_creation(self: Pin<&Self>, pin_id: i32, position: LogicalPoint) {
        let mut state = self.data.state.borrow_mut();
        state.is_creating_link = true;
        state.link_start_pin = pin_id;
        state.link_current_pos = position;
    }

    /// Find a pin at the given position, returns pin ID or 0 if no pin found
    fn find_pin_at(self: Pin<&Self>, position: LogicalPoint) -> i32 {
        let hit_radius = self.pin_hit_radius().get();
        let hit_radius_sq = hit_radius * hit_radius;
        let state = self.data.state.borrow();

        for (&pin_id, &pin_pos) in state.pin_positions.iter() {
            let dx = position.x - pin_pos.x;
            let dy = position.y - pin_pos.y;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq <= hit_radius_sq {
                return pin_id;
            }
        }

        0 // No pin found
    }

    /// Check if two pins are compatible for linking (one input, one output)
    fn pins_compatible(start_pin: i32, end_pin: i32) -> bool {
        if start_pin == end_pin {
            return false;
        }
        // Pin ID convention: id % 10 == 1 for input, == 2 for output
        let start_type = start_pin % 10;
        let end_type = end_pin % 10;
        // One must be input (1) and one must be output (2)
        (start_type == 1 && end_type == 2) || (start_type == 2 && end_type == 1)
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
