// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! Node Editor items for building visual node graph editors.
//!
//! This module provides two native items that work together in a three-layer architecture:
//!
//! 1. **NodeEditorBackground** (bottom layer)
//!    - Provides properties for viewport state (pan, zoom)
//!    - Generates grid path commands based on pan/zoom/size
//!    - Contains a child Path component that renders the grid
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
//! **Rendering Philosophy**: The background generates grid commands that a child Path
//! component renders. The overlay handles input and state management. Selection box
//! and link preview are rendered by Slint components bound to overlay properties.

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
use crate::{Callback, Coord, Property, SharedString};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::string::String;
use const_field_offset::FieldOffsets;
use core::cell::RefCell;
use core::pin::Pin;
use i_slint_core_macros::*;

// Note: Complex callbacks with multiple parameters are not yet supported by the RTTI system.
// For now, we use simple void callbacks. The actual event data can be retrieved via properties.
// TODO: Add proper callback argument types once builtin_structs integration is done.

/// Internal state for the node editor background (grid caching)
#[derive(Default)]
struct BackgroundState {
    /// Cached grid parameters to detect changes
    last_width: f32,
    last_height: f32,
    last_pan_x: f32,
    last_pan_y: f32,
    last_zoom: f32,
    last_spacing: f32,
}

/// Wraps the background internal state properly with RefCell
#[repr(C)]
pub struct BackgroundData {
    state: RefCell<BackgroundState>,
}

impl Default for BackgroundData {
    fn default() -> Self {
        Self {
            state: RefCell::new(BackgroundState::default()),
        }
    }
}

#[repr(C)]
pub struct BackgroundDataBox(core::ptr::NonNull<BackgroundData>);

impl Default for BackgroundDataBox {
    fn default() -> Self {
        BackgroundDataBox(Box::leak(Box::<BackgroundData>::default()).into())
    }
}

impl Drop for BackgroundDataBox {
    fn drop(&mut self) {
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

impl core::ops::Deref for BackgroundDataBox {
    type Target = BackgroundData;
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

/// Generate SVG path commands for grid lines
fn generate_grid_commands(width: f32, height: f32, zoom: f32, pan_x: f32, pan_y: f32, spacing: f32) -> String {
    let effective_spacing = spacing * zoom;

    // Skip if spacing is too small to be visible
    if effective_spacing < 4.0 {
        return String::new();
    }

    // Calculate grid offset based on pan (modulo spacing for infinite grid effect)
    let offset_x = pan_x.rem_euclid(effective_spacing);
    let offset_y = pan_y.rem_euclid(effective_spacing);

    let mut commands = String::with_capacity(10000);

    // Generate vertical lines
    let mut x = offset_x;
    while x < width + effective_spacing {
        if !commands.is_empty() {
            commands.push(' ');
        }
        commands.push_str(&alloc::format!("M {} 0 L {} {}", x, x, height));
        x += effective_spacing;
    }

    // Generate horizontal lines
    let mut y = offset_y;
    while y < height + effective_spacing {
        commands.push(' ');
        commands.push_str(&alloc::format!("M 0 {} L {} {}", y, width, y));
        y += effective_spacing;
    }

    commands
}

// Pin dimensions for computing pin positions (must match ui.slint)
const BASE_PIN_SIZE: f32 = 12.0;
const PIN_Y_OFFSET: f32 = 8.0 + 24.0 + 8.0; // Margin + title height + margin
const PIN_MARGIN: f32 = 8.0; // Horizontal margin from node edge

/// Cubic bezier curve control points
struct CubicBezier {
    p0: (f32, f32), // Start point
    p1: (f32, f32), // Control point 1
    p2: (f32, f32), // Control point 2
    p3: (f32, f32), // End point
}

impl CubicBezier {
    /// Evaluate the bezier curve at parameter t (0.0 to 1.0)
    fn eval(&self, t: f32) -> (f32, f32) {
        let t2 = t * t;
        let t3 = t2 * t;
        let mt = 1.0 - t;
        let mt2 = mt * mt;
        let mt3 = mt2 * mt;

        let x = mt3 * self.p0.0
            + 3.0 * mt2 * t * self.p1.0
            + 3.0 * mt * t2 * self.p2.0
            + t3 * self.p3.0;
        let y = mt3 * self.p0.1
            + 3.0 * mt2 * t * self.p1.1
            + 3.0 * mt * t2 * self.p2.1
            + t3 * self.p3.1;

        (x, y)
    }
}

/// Calculate the minimum distance from a point to a cubic bezier curve
/// Uses subdivision approach: sample curve at regular intervals and find closest point
fn distance_to_bezier(point: (f32, f32), bezier: &CubicBezier) -> f32 {
    const NUM_SAMPLES: usize = 20; // Number of line segments to approximate curve

    let mut min_dist_sq = f32::MAX;

    // Sample the curve and check distance to each line segment
    let mut prev_point = bezier.eval(0.0);

    for i in 1..=NUM_SAMPLES {
        let t = i as f32 / NUM_SAMPLES as f32;
        let curr_point = bezier.eval(t);

        // Find distance from point to line segment (prev_point, curr_point)
        let dist_sq = distance_to_line_segment_sq(point, prev_point, curr_point);
        if dist_sq < min_dist_sq {
            min_dist_sq = dist_sq;
        }

        prev_point = curr_point;
    }

    min_dist_sq.sqrt()
}

/// Calculate squared distance from a point to a line segment
fn distance_to_line_segment_sq(point: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let ab = (b.0 - a.0, b.1 - a.1);
    let ap = (point.0 - a.0, point.1 - a.1);

    let ab_len_sq = ab.0 * ab.0 + ab.1 * ab.1;

    if ab_len_sq < f32::EPSILON {
        // Degenerate segment (a == b)
        return ap.0 * ap.0 + ap.1 * ap.1;
    }

    // Project point onto line, clamped to segment
    let t = ((ap.0 * ab.0 + ap.1 * ab.1) / ab_len_sq).clamp(0.0, 1.0);

    // Closest point on segment
    let closest = (a.0 + t * ab.0, a.1 + t * ab.1);

    // Distance squared from point to closest point
    let dx = point.0 - closest.0;
    let dy = point.1 - closest.1;
    dx * dx + dy * dy
}

/// Generate SVG path command for a bezier link between two pin positions
/// Uses horizontal control points for a smooth curve
/// Returns an SVG path string like "M x0 y0 C x1 y1 x2 y2 x3 y3"
fn generate_bezier_path_command(
    start_x: f32,
    start_y: f32,
    end_x: f32,
    end_y: f32,
    zoom: f32,
) -> String {
    // Calculate control point offset (horizontal bezier, matching ui.slint logic)
    let dx = (end_x - start_x).abs();
    let offset = (dx * 0.5).max(50.0 * zoom);

    // Control points extend horizontally from start and end
    let ctrl1_x = start_x + offset;
    let ctrl1_y = start_y;
    let ctrl2_x = end_x - offset;
    let ctrl2_y = end_y;

    // Generate SVG path: M (move to), C (cubic bezier)
    alloc::format!(
        "M {} {} C {} {} {} {} {} {}",
        start_x, start_y,
        ctrl1_x, ctrl1_y,
        ctrl2_x, ctrl2_y,
        end_x, end_y
    )
}

/// Create a CubicBezier from start/end points using the same logic as generate_bezier_path_command
fn create_bezier_from_endpoints(
    start_x: f32,
    start_y: f32,
    end_x: f32,
    end_y: f32,
    zoom: f32,
) -> CubicBezier {
    let dx = (end_x - start_x).abs();
    let offset = (dx * 0.5).max(50.0 * zoom);

    CubicBezier {
        p0: (start_x, start_y),
        p1: (start_x + offset, start_y),
        p2: (end_x - offset, end_y),
        p3: (end_x, end_y),
    }
}

/// Compute screen position for a pin given its ID, node rect, and zoom
/// Pin ID format: node_id * 10 + pin_type (1 = input, 2 = output)
/// Returns (x, y) in screen coordinates (center of pin circle)
fn compute_pin_screen_position(pin_id: i32, node_rect: &NodeRect, zoom: f32) -> (f32, f32) {
    let pin_type = pin_id % 10; // 1 = input, 2 = output
    let pin_size = BASE_PIN_SIZE * zoom;
    let pin_radius = pin_size / 2.0;

    let (x, y) = if pin_type == 1 {
        // Input pin: left side
        let x = node_rect.x + PIN_MARGIN * zoom + pin_radius;
        let y = node_rect.y + PIN_Y_OFFSET * zoom + pin_radius;
        (x, y)
    } else {
        // Output pin: right side
        let x = node_rect.x + node_rect.width - PIN_MARGIN * zoom - pin_size + pin_radius;
        let y = node_rect.y + PIN_Y_OFFSET * zoom + pin_radius;
        (x, y)
    };

    (x, y)
}

/// Node rectangle info for hit-testing and box selection
#[derive(Clone, Copy, Debug, Default)]
struct NodeRect {
    /// Screen X position
    x: f32,
    /// Screen Y position
    y: f32,
    /// Width
    width: f32,
    /// Height
    height: f32,
}

impl NodeRect {
    /// Check if this rect intersects with the given selection box
    fn intersects(&self, sel_x: f32, sel_y: f32, sel_width: f32, sel_height: f32) -> bool {
        self.x < sel_x + sel_width
            && self.x + self.width > sel_x
            && self.y < sel_y + sel_height
            && self.y + self.height > sel_y
    }

    /// Check if a point is inside this rect
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

/// A link between two pins
#[derive(Clone, Copy, Debug)]
struct LinkRecord {
    start_pin_id: i32,
    end_pin_id: i32,
    color: Color,
}

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
    /// Known node rectangles for hit-testing and box selection (node_id -> screen rect)
    node_rects: BTreeMap<i32, NodeRect>,
    /// Set of selected node IDs (owned by core)
    selected_node_ids: BTreeSet<i32>,
    /// Set of selected link IDs (owned by core)
    selected_link_ids: BTreeSet<i32>,
    /// Known links (link_id -> link record)
    links: BTreeMap<i32, LinkRecord>,
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
/// This provides viewport properties (pan, zoom) and generates grid path commands
/// that a child Path component can render. The grid commands are automatically
/// regenerated when pan, zoom, or size changes.
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

    /// Generated SVG path commands for grid lines (output, bind Path.commands to this)
    pub grid_commands: Property<SharedString>,

    /// Internal state for grid caching
    data: BackgroundDataBox,

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
        FocusEventResult::FocusAccepted
    }

    fn render(
        self: Pin<&Self>,
        _backend: &mut ItemRendererRef,
        _self_rc: &ItemRc,
        size: LogicalSize,
    ) -> RenderingResult {
        // Get current values
        let width = size.width;
        let height = size.height;
        let pan_x = self.pan_x().get();
        let pan_y = self.pan_y().get();
        let zoom = self.zoom();
        let spacing = self.grid_spacing().get();

        // Check if we need to regenerate grid commands
        let mut state = self.data.state.borrow_mut();
        let needs_update = state.last_width != width
            || state.last_height != height
            || state.last_pan_x != pan_x
            || state.last_pan_y != pan_y
            || state.last_zoom != zoom
            || state.last_spacing != spacing;

        if needs_update {
            // Update cached values
            state.last_width = width;
            state.last_height = height;
            state.last_pan_x = pan_x;
            state.last_pan_y = pan_y;
            state.last_zoom = zoom;
            state.last_spacing = spacing;

            // Generate new grid commands
            let commands = generate_grid_commands(width, height, zoom, pan_x, pan_y, spacing);

            // Drop the borrow before setting the property to avoid potential issues
            drop(state);

            // Update the grid_commands property
            Self::FIELD_OFFSETS.grid_commands.apply_pin(self).set(SharedString::from(&commands));
        }

        // Continue rendering children (which includes the grid Path)
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

    // === Node drag state (for applying offset to link positions) ===
    /// Whether nodes are currently being dragged
    pub is_dragging: Property<bool>,
    /// X offset of dragged nodes (in screen coordinates, applied to selected nodes)
    pub drag_offset_x: Property<LogicalLength>,
    /// Y offset of dragged nodes (in screen coordinates, applied to selected nodes)
    pub drag_offset_y: Property<LogicalLength>,

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
    /// Core-generated SVG bezier path command for link preview
    /// Format: "M x0 y0 C x1 y1 x2 y2 x3 y3"
    /// Updated during link creation when link_end_x/y changes
    pub link_preview_path_commands: Property<SharedString>,

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
    /// Batch of pending pin positions to register (format: "id,x,y;...")
    /// Used when multiple pins report positions at once (e.g., on viewport change)
    pub pending_pins_batch: Property<SharedString>,

    // === Link creation result (output when link_created is fired) ===
    /// Start pin ID of the created link (output pin)
    pub created_link_start_pin: Property<i32>,
    /// End pin ID of the created link (input pin)
    pub created_link_end_pin: Property<i32>,

    // === Node rectangle reporting (for hit-testing and box selection) ===
    /// Node ID being reported (set before triggering node_rect_changed)
    pub reporting_node_id: Property<i32>,
    /// X position of node being reported (in screen coordinates)
    pub reporting_node_x: Property<LogicalLength>,
    /// Y position of node being reported (in screen coordinates)
    pub reporting_node_y: Property<LogicalLength>,
    /// Width of node being reported
    pub reporting_node_width: Property<LogicalLength>,
    /// Height of node being reported
    pub reporting_node_height: Property<LogicalLength>,
    /// Callback when a node reports its rectangle
    pub node_rect_changed: Callback<()>,
    /// Batch of pending node rects to register (format: "id,x,y,w,h;...")
    /// Used when multiple nodes report rects at once
    pub pending_node_rects_batch: Property<SharedString>,

    // === Link reporting (for core-based link rendering) ===
    /// Link ID being reported (set before calling link_reported)
    pub reporting_link_id: Property<i32>,
    /// Start pin ID of link being reported
    pub reporting_link_start_pin_id: Property<i32>,
    /// End pin ID of link being reported
    pub reporting_link_end_pin_id: Property<i32>,
    /// Color of link being reported (ARGB format)
    pub reporting_link_color: Property<Color>,
    /// Callback when a link is reported
    pub link_reported: Callback<()>,
    /// Batch of pending links to register (format: "id,start_pin,end_pin,color_argb;...")
    /// Used when multiple links need to be reported at once
    pub pending_links_batch: Property<SharedString>,
    /// Batch of link IDs to delete (format: "id1,id2,id3,...")
    /// Used when links are removed from the model
    pub pending_deleted_link_ids: Property<SharedString>,

    // === Link position data (output - regenerated when viewport/nodes change) ===
    /// Formatted string containing link position data for all registered links
    /// Format: "id,start_x,start_y,end_x,end_y,color_argb;id,start_x,start_y,end_x,end_y,color_argb;..."
    /// Empty string if no links
    pub link_positions_data: Property<SharedString>,

    // === Link bezier path commands (output - generated by core for rendering) ===
    /// Formatted string containing bezier path commands for all registered links
    /// Format: "id|path_commands|color_argb;id|path_commands|color_argb;..."
    /// Uses | separator within entries since path commands contain spaces
    /// Each path_commands is an SVG path string like "M x0 y0 C x1 y1 x2 y2 x3 y3"
    /// Empty string if no links
    pub link_bezier_paths: Property<SharedString>,

    /// Callback when link positions have been updated (fired after regenerate_link_positions)
    pub link_positions_changed: Callback<()>,

    // === Box selection result (output when selection_changed fires) ===
    /// Comma-separated list of node IDs that intersect with the selection box
    /// Format: "1,2,3" or empty string if none
    pub selected_node_ids_str: Property<SharedString>,

    // === Node selection management ===
    /// Node ID being clicked (set before calling node-clicked)
    pub clicked_node_id: Property<i32>,
    /// Whether shift was held during node click
    pub clicked_shift_held: Property<bool>,
    /// Callback when a node is clicked - set clicked-node-id and clicked-shift-held first
    pub node_clicked: Callback<()>,

    /// Comma-separated list of currently selected node IDs (output)
    /// Format: "1,2,3" or empty string if none
    pub current_selected_ids: Property<SharedString>,

    // === Link hover state ===
    /// ID of the link currently being hovered, or -1 if none
    pub hovered_link_id: Property<i32>,
    /// Hit distance threshold for link hover detection (in screen pixels)
    pub link_hover_distance: Property<LogicalLength>,
    /// Callback when hovered link changes (check hovered_link_id property for value)
    pub link_hovered: Callback<()>,

    // === Link selection state ===
    /// Comma-separated list of currently selected link IDs (output)
    /// Format: "1,2,3" or empty string if none
    pub current_selected_link_ids: Property<SharedString>,
    /// Callback when link selection changes
    pub link_selection_changed: Callback<()>,

    // === Debug properties ===
    /// Number of registered pins (for debugging)
    pub debug_pin_count: Property<i32>,
    /// Number of registered links (for debugging)
    pub debug_link_count: Property<i32>,

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
        self.check_pending_link_start();

        // Process any pending reports from pins, nodes, links, and clicks
        self.process_pending_reports();

        // Check if a Pin component is requesting link completion (pin has mouse capture)
        self.check_pending_link_complete();

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
                    // Check if clicking on a link - if so, intercept for link selection
                    // BUT: pins have priority over links (so you can drag from connected pins)
                    if let MouseEvent::Pressed { position, .. } = _event {
                        drop(state); // Release borrow before calling find_pin_at/find_link_at
                        // Only check for link hit if NOT clicking on a pin
                        // (pins have priority so you can drag from connected pins)
                        let pin_at = self.find_pin_at(*position);
                        if pin_at == 0 && self.find_link_at(*position) >= 0 {
                            return InputEventFilterResult::Intercept;
                        }
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
        // Check if a Pin component wants to start or complete link creation
        // (This is checked here because Pin's TouchArea may have mouse capture,
        // preventing input_event_filter_before_children from being called)
        self.check_pending_link_start();
        self.check_pending_link_complete();

        // Process any pending reports from pins, nodes, and links
        // This ensures reports are processed even without input events
        self.process_pending_reports();

        // Update link preview path if we're creating a link
        // (Pin's TouchArea updates link-end-x/y directly, so we regenerate the path here)
        if self.is_creating_link() {
            self.update_link_preview_path();
        }

        // Regenerate link positions during drag (drag offset changes every frame)
        // This ensures link bezier paths follow the dragged nodes
        if self.is_dragging() {
            self.regenerate_link_positions();
        }

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
    /// Update the link preview path command based on current start/end positions
    fn update_link_preview_path(self: Pin<&Self>) {
        let start_x = self.link_start_x().get();
        let start_y = self.link_start_y().get();
        let end_x = self.link_end_x().get();
        let end_y = self.link_end_y().get();
        let zoom = self.zoom();

        let path_cmd = generate_bezier_path_command(start_x, start_y, end_x, end_y, zoom);
        Self::FIELD_OFFSETS.link_preview_path_commands.apply_pin(self).set(SharedString::from(&path_cmd));
    }

    /// Check if a Pin component wants to start link creation
    /// Called from both input_event_filter_before_children and render
    fn check_pending_link_start(self: Pin<&Self>) {
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

                // Generate initial link preview path
                self.update_link_preview_path();

                // Clear the pending trigger
                Self::FIELD_OFFSETS.pending_link_pin_id.apply_pin(self).set(0);
            }
        }
    }

    /// Check if a Pin component is requesting link completion
    /// Called from both input_event_filter_before_children and render
    fn check_pending_link_complete(self: Pin<&Self>) {
        if self.complete_link_creation() {
            // If target_pin_id is set, use it (backward compatibility / explicit target)
            // Otherwise, use core's find_pin_at with the current link end position
            let explicit_target = self.target_pin_id();
            let end_pin = if explicit_target != 0 {
                explicit_target
            } else {
                let end_pos = LogicalPoint::new(
                    self.link_end_x().get(),
                    self.link_end_y().get(),
                );
                self.find_pin_at(end_pos)
            };

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

                    // Check if link already exists between these pins
                    let state = self.data.state.borrow();
                    let already_exists = Self::link_exists(&state, output_pin, input_pin);
                    drop(state);

                    if already_exists {
                        // Don't create duplicate link
                        self.link_dropped.call(&());
                    } else {
                        // Set the created link properties
                        Self::FIELD_OFFSETS.created_link_start_pin.apply_pin(self).set(output_pin);
                        Self::FIELD_OFFSETS.created_link_end_pin.apply_pin(self).set(input_pin);

                        // Emit link_created callback
                        self.link_created.call(&());
                    }
                } else {
                    // Dropped on empty space or incompatible pin
                    self.link_dropped.call(&());
                }
            }
        }
    }

    /// Process pending reports from pins, nodes, and links
    /// This is called from render() to ensure reports are processed even without input events
    fn process_pending_reports(self: Pin<&Self>) {
        // Track whether we need to regenerate link positions at the end
        let mut needs_link_regeneration = false;

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

        // Check for batch pin position reports (format: "id,x,y;...")
        let pins_batch = self.pending_pins_batch();
        if !pins_batch.is_empty() {
            let mut state = self.data.state.borrow_mut();

            for pin_str in pins_batch.split(';') {
                if pin_str.is_empty() {
                    continue;
                }
                let parts: alloc::vec::Vec<&str> = pin_str.split(',').collect();
                if parts.len() >= 3 {
                    if let (Ok(id), Ok(x), Ok(y)) = (
                        parts[0].parse::<i32>(),
                        parts[1].parse::<f32>(),
                        parts[2].parse::<f32>(),
                    ) {
                        state.pin_positions.insert(id, LogicalPoint::new(x, y));
                    }
                }
            }

            // Update debug count
            let pin_count = state.pin_positions.len() as i32;
            drop(state);

            Self::FIELD_OFFSETS.debug_pin_count.apply_pin(self).set(pin_count);

            // Clear the batch
            Self::FIELD_OFFSETS.pending_pins_batch.apply_pin(self).set(SharedString::default());
        }

        // Check if a Node component is reporting its rectangle (single)
        let reporting_node = self.reporting_node_id();
        if reporting_node > 0 {
            let node_x = self.reporting_node_x().get();
            let node_y = self.reporting_node_y().get();
            let node_width = self.reporting_node_width().get();
            let node_height = self.reporting_node_height().get();

            // Store the node rectangle for hit-testing and box selection
            let mut state = self.data.state.borrow_mut();
            state.node_rects.insert(
                reporting_node,
                NodeRect {
                    x: node_x,
                    y: node_y,
                    width: node_width,
                    height: node_height,
                },
            );
            drop(state);

            // Clear the reporting trigger
            Self::FIELD_OFFSETS.reporting_node_id.apply_pin(self).set(0);

            // Mark for link regeneration since node moved
            needs_link_regeneration = true;
        }

        // Check for batch node rect reports (format: "id,x,y,w,h;...")
        let node_batch = self.pending_node_rects_batch();
        if !node_batch.is_empty() {
            let mut state = self.data.state.borrow_mut();

            for rect_str in node_batch.split(';') {
                if rect_str.is_empty() {
                    continue;
                }
                let parts: alloc::vec::Vec<&str> = rect_str.split(',').collect();
                if parts.len() >= 5 {
                    if let (Ok(id), Ok(x), Ok(y), Ok(w), Ok(h)) = (
                        parts[0].parse::<i32>(),
                        parts[1].parse::<f32>(),
                        parts[2].parse::<f32>(),
                        parts[3].parse::<f32>(),
                        parts[4].parse::<f32>(),
                    ) {
                        state.node_rects.insert(
                            id,
                            NodeRect {
                                x,
                                y,
                                width: w,
                                height: h,
                            },
                        );
                        needs_link_regeneration = true;
                    }
                }
            }

            drop(state);

            // Clear the batch
            Self::FIELD_OFFSETS.pending_node_rects_batch.apply_pin(self).set(SharedString::default());
        }

        // Check if a Link is being reported (single link)
        let reporting_link = self.reporting_link_id();
        if reporting_link > 0 {
            let start_pin_id = self.reporting_link_start_pin_id();
            let end_pin_id = self.reporting_link_end_pin_id();
            let color = self.reporting_link_color();

            // Store the link in the registry
            let mut state = self.data.state.borrow_mut();
            state.links.insert(
                reporting_link,
                LinkRecord {
                    start_pin_id,
                    end_pin_id,
                    color,
                },
            );
            drop(state);

            // Clear the reporting trigger
            Self::FIELD_OFFSETS.reporting_link_id.apply_pin(self).set(0);

            // Mark for link regeneration now that we have a new link
            needs_link_regeneration = true;
        }

        // Check for batch link reports (format: "id,start_pin,end_pin,color_argb;...")
        let batch = self.pending_links_batch();
        if !batch.is_empty() {
            let mut state = self.data.state.borrow_mut();

            for link_str in batch.split(';') {
                if link_str.is_empty() {
                    continue;
                }
                let parts: alloc::vec::Vec<&str> = link_str.split(',').collect();
                if parts.len() >= 4 {
                    if let (Ok(id), Ok(start_pin), Ok(end_pin), Ok(color_argb)) = (
                        parts[0].parse::<i32>(),
                        parts[1].parse::<i32>(),
                        parts[2].parse::<i32>(),
                        parts[3].parse::<u32>(),
                    ) {
                        let color = Color::from_argb_encoded(color_argb);
                        state.links.insert(
                            id,
                            LinkRecord {
                                start_pin_id: start_pin,
                                end_pin_id: end_pin,
                                color,
                            },
                        );
                        needs_link_regeneration = true;
                    }
                }
            }

            // Update debug count
            let link_count = state.links.len() as i32;
            drop(state);

            Self::FIELD_OFFSETS.debug_link_count.apply_pin(self).set(link_count);

            // Clear the batch
            Self::FIELD_OFFSETS.pending_links_batch.apply_pin(self).set(SharedString::default());
        }

        // Check for deleted link IDs (format: "id1,id2,id3,...")
        let deleted_ids = self.pending_deleted_link_ids();
        if !deleted_ids.is_empty() {
            let mut state = self.data.state.borrow_mut();

            for id_str in deleted_ids.split(',') {
                if let Ok(id) = id_str.trim().parse::<i32>() {
                    state.links.remove(&id);
                    state.selected_link_ids.remove(&id);
                }
            }

            // Update debug count
            let link_count = state.links.len() as i32;
            drop(state);

            Self::FIELD_OFFSETS.debug_link_count.apply_pin(self).set(link_count);

            // Clear the pending deletions
            Self::FIELD_OFFSETS.pending_deleted_link_ids.apply_pin(self).set(SharedString::default());

            needs_link_regeneration = true;
        }

        // Check if a node is being clicked (for selection)
        let clicked_node = self.clicked_node_id();
        if clicked_node > 0 {
            let shift_held = self.clicked_shift_held();

            // Clear link selection when clicking on a node (unless shift is held)
            if !shift_held {
                self.clear_link_selection();
            }

            let mut state = self.data.state.borrow_mut();

            if shift_held {
                // Multi-select: toggle the clicked node
                if state.selected_node_ids.contains(&clicked_node) {
                    state.selected_node_ids.remove(&clicked_node);
                } else {
                    state.selected_node_ids.insert(clicked_node);
                }
            } else {
                // Single select: clear all and select only clicked node
                state.selected_node_ids.clear();
                state.selected_node_ids.insert(clicked_node);
            }

            // Update the current selected IDs output property
            let ids_str = state
                .selected_node_ids
                .iter()
                .map(|id| alloc::format!("{}", id))
                .collect::<alloc::vec::Vec<_>>()
                .join(",");
            drop(state);

            Self::FIELD_OFFSETS
                .current_selected_ids
                .apply_pin(self)
                .set(SharedString::from(&ids_str));

            // Clear the click trigger
            Self::FIELD_OFFSETS.clicked_node_id.apply_pin(self).set(0);

            // Notify that selection changed
            self.selection_changed.call(&());
        }

        // Regenerate link positions once at the end if any relevant data changed
        // This ensures all batches (pins, node rects, links) are processed before regeneration
        if needs_link_regeneration {
            self.regenerate_link_positions();
        }
    }

    /// Regenerate all link positions and bezier paths based on current node rects and viewport state
    /// Updates both link_positions_data (for backwards compat) and link_bezier_paths (for core-based rendering)
    /// Applies drag offset to selected nodes when is_dragging is true
    fn regenerate_link_positions(self: Pin<&Self>) {
        let state = self.data.state.borrow();
        let zoom = self.zoom();

        // Get drag state for applying offset to selected nodes
        let is_dragging = self.is_dragging();
        let drag_offset_x = if is_dragging { self.drag_offset_x().get() } else { 0.0 };
        let drag_offset_y = if is_dragging { self.drag_offset_y().get() } else { 0.0 };

        // Build formatted strings for both position data and bezier paths
        let mut position_results = alloc::vec::Vec::new();
        let mut bezier_results = alloc::vec::Vec::new();

        for (link_id, link) in state.links.iter() {
            // Get node IDs from pin IDs
            let start_node_id = link.start_pin_id / 10;
            let end_node_id = link.end_pin_id / 10;

            // Find node rects
            let start_node_rect = state.node_rects.get(&start_node_id);
            let end_node_rect = state.node_rects.get(&end_node_id);

            if let (Some(start_rect), Some(end_rect)) = (start_node_rect, end_node_rect) {
                // Compute base pin positions
                let (mut start_x, mut start_y) = compute_pin_screen_position(link.start_pin_id, start_rect, zoom);
                let (mut end_x, mut end_y) = compute_pin_screen_position(link.end_pin_id, end_rect, zoom);

                // Apply drag offset to selected nodes
                // Note: drag_offset is in world coordinates, pin positions are in screen coordinates
                // So we multiply by zoom to convert drag offset to screen space
                if is_dragging {
                    if state.selected_node_ids.contains(&start_node_id) {
                        start_x += drag_offset_x * zoom;
                        start_y += drag_offset_y * zoom;
                    }
                    if state.selected_node_ids.contains(&end_node_id) {
                        end_x += drag_offset_x * zoom;
                        end_y += drag_offset_y * zoom;
                    }
                }

                let color_argb = link.color.as_argb_encoded();

                // Format for position data: id,start_x,start_y,end_x,end_y,color_argb
                position_results.push(alloc::format!(
                    "{},{},{},{},{},{}",
                    link_id, start_x, start_y, end_x, end_y, color_argb
                ));

                // Generate bezier path command and add to bezier results
                // Format: id|path_commands|color_argb (using | since path contains spaces)
                let path_cmd = generate_bezier_path_command(start_x, start_y, end_x, end_y, zoom);
                bezier_results.push(alloc::format!("{}|{}|{}", link_id, path_cmd, color_argb));
            }
        }

        drop(state);

        // Update both properties
        let position_data_str = position_results.join(";");
        Self::FIELD_OFFSETS.link_positions_data.apply_pin(self).set(SharedString::from(&position_data_str));

        let bezier_data_str = bezier_results.join(";");
        Self::FIELD_OFFSETS.link_bezier_paths.apply_pin(self).set(SharedString::from(&bezier_data_str));

        // Notify that link positions have changed
        self.link_positions_changed.call(&());
    }

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

                    // Clear link selection when starting box selection
                    self.clear_link_selection();

                    // Update selection properties for Slint rendering
                    Self::FIELD_OFFSETS.is_selecting.apply_pin(self).set(true);
                    Self::FIELD_OFFSETS.selection_x.apply_pin(self).set(LogicalLength::new(position.x));
                    Self::FIELD_OFFSETS.selection_y.apply_pin(self).set(LogicalLength::new(position.y));
                    Self::FIELD_OFFSETS.selection_width.apply_pin(self).set(LogicalLength::new(0.0));
                    Self::FIELD_OFFSETS.selection_height.apply_pin(self).set(LogicalLength::new(0.0));

                    InputEventResult::GrabMouse
                } else {
                    // Check if clicking on a pin first - pins have priority over links
                    // so users can drag from connected pins
                    let pin_at = self.find_pin_at(position);
                    if pin_at != 0 {
                        // Pass through to allow pin interaction
                        InputEventResult::EventIgnored
                    } else {
                        // Check if clicking on a link
                        let clicked_link = self.find_link_at(position);
                        if clicked_link >= 0 {
                            // Handle link selection
                            self.handle_link_click(clicked_link, modifiers.shift());
                            InputEventResult::EventAccepted
                        } else {
                            // Pass through left clicks to allow node interaction
                            // Background click deselection is handled in mouse released
                            InputEventResult::EventIgnored
                        }
                    }
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
                    // Get selection box bounds
                    let sel_start = state.box_select_start;
                    let sel_current = state.box_select_current;
                    let sel_x = sel_start.x.min(sel_current.x);
                    let sel_y = sel_start.y.min(sel_current.y);
                    let sel_width = (sel_current.x - sel_start.x).abs();
                    let sel_height = (sel_current.y - sel_start.y).abs();

                    // Find all nodes that intersect with the selection box
                    let intersecting_ids: BTreeSet<i32> = state
                        .node_rects
                        .iter()
                        .filter(|(_, rect)| rect.intersects(sel_x, sel_y, sel_width, sel_height))
                        .map(|(id, _)| *id)
                        .collect();

                    // Update selection state (replace with intersecting nodes)
                    state.selected_node_ids = intersecting_ids;

                    // Build output string from selection state
                    let ids_str = state
                        .selected_node_ids
                        .iter()
                        .map(|id| alloc::format!("{}", id))
                        .collect::<alloc::vec::Vec<_>>()
                        .join(",");

                    state.is_box_selecting = false;
                    drop(state);

                    // Update both output properties
                    Self::FIELD_OFFSETS
                        .selected_node_ids_str
                        .apply_pin(self)
                        .set(SharedString::from(&ids_str));
                    Self::FIELD_OFFSETS
                        .current_selected_ids
                        .apply_pin(self)
                        .set(SharedString::from(&ids_str));

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

                        // Check if link already exists between these pins
                        let state = self.data.state.borrow();
                        let already_exists = Self::link_exists(&state, output_pin, input_pin);
                        drop(state);

                        if already_exists {
                            // Don't create duplicate link
                            self.link_dropped.call(&());
                        } else {
                            // Set the created link properties
                            Self::FIELD_OFFSETS.created_link_start_pin.apply_pin(self).set(output_pin);
                            Self::FIELD_OFFSETS.created_link_end_pin.apply_pin(self).set(input_pin);

                            // Emit link_created callback
                            self.link_created.call(&());
                        }
                    } else {
                        // Dropped on empty space or incompatible pin
                        self.link_dropped.call(&());
                    }
                    return InputEventResult::EventAccepted;
                }

                // If we reach here, it's a left button release with no ongoing operation
                // Check if it's a background click (not on a node, pin, or link)
                // This is called from input_event (AFTER children), so if the click reached here,
                // children passed it through. We check if the click hit any interactive element.
                drop(state);

                // Check if clicking on a pin or link
                let pin_at = self.find_pin_at(position);
                let link_at = self.find_link_at(position);

                if pin_at == 0 && link_at < 0 {
                    // Not on a pin or link - check if on a node
                    let state = self.data.state.borrow();
                    let on_node = state.node_rects.values().any(|rect| rect.contains(position.x, position.y));
                    drop(state);

                    if !on_node {
                        // True background click - clear all selections
                        let mut state = self.data.state.borrow_mut();
                        let had_node_selection = !state.selected_node_ids.is_empty();
                        let had_link_selection = !state.selected_link_ids.is_empty();

                        state.selected_node_ids.clear();
                        state.selected_link_ids.clear();
                        drop(state);

                        // Update properties if anything changed
                        if had_node_selection {
                            Self::FIELD_OFFSETS
                                .current_selected_ids
                                .apply_pin(self)
                                .set(SharedString::default());
                        }
                        if had_link_selection {
                            Self::FIELD_OFFSETS
                                .current_selected_link_ids
                                .apply_pin(self)
                                .set(SharedString::default());
                            self.link_selection_changed.call(&());
                        }
                        if had_node_selection || had_link_selection {
                            self.selection_changed.call(&());
                        }
                    }
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

            // Regenerate link positions with new pan
            self.regenerate_link_positions();

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

            // Regenerate the bezier path command for the link preview
            self.update_link_preview_path();

            return InputEventResult::GrabMouse;
        }

        drop(state);

        // When not in any interaction mode, check for link hover
        self.update_hovered_link(position);

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

            // Regenerate link positions with new zoom/pan
            self.regenerate_link_positions();

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

        // Clear hovered link on exit
        let current_hovered = self.hovered_link_id();
        if current_hovered != -1 {
            Self::FIELD_OFFSETS.hovered_link_id.apply_pin(self).set(-1);
            self.link_hovered.call(&());
        }

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
    /// Uses pin positions from state.pin_positions to support arbitrary pin layouts
    fn find_pin_at(self: Pin<&Self>, position: LogicalPoint) -> i32 {
        let zoom = self.zoom();
        // Use explicit hit radius if set, otherwise default to ~66% of pin diameter
        let explicit_radius = self.pin_hit_radius().get();
        let hit_radius = if explicit_radius > 0.0 {
            explicit_radius
        } else {
            BASE_PIN_SIZE * zoom * 0.66
        };
        let hit_radius_sq = hit_radius * hit_radius;
        let state = self.data.state.borrow();

        // Check all pins from pin_positions (supports arbitrary pin counts per node)
        for (&pin_id, pin_pos) in state.pin_positions.iter() {
            let dx = position.x - pin_pos.x;
            let dy = position.y - pin_pos.y;
            if dx * dx + dy * dy <= hit_radius_sq {
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
        // Pin ID convention: id % 10 == 1 for input, == 2 for output, == 3+ for additional inputs
        let start_type = start_pin % 10;
        let end_type = end_pin % 10;
        // Input types: 1, 3, 4, 5, ... (odd numbers or >= 3)
        // Output types: 2
        let start_is_output = start_type == 2;
        let end_is_output = end_type == 2;
        // One must be output and one must be input (not output)
        start_is_output != end_is_output
    }

    /// Check if a link already exists between two pins
    /// Takes normalized pin IDs (output_pin, input_pin)
    fn link_exists(state: &NodeEditorState, output_pin: i32, input_pin: i32) -> bool {
        state.links.values().any(|link| {
            link.start_pin_id == output_pin && link.end_pin_id == input_pin
        })
    }

    /// Find a link at the given position, returns link ID or -1 if no link found
    /// Uses distance-to-bezier algorithm to check proximity to link curves
    fn find_link_at(self: Pin<&Self>, position: LogicalPoint) -> i32 {
        let hover_distance = self.link_hover_distance().get();
        let zoom = self.zoom();
        let state = self.data.state.borrow();

        // Get drag state for applying offset to selected nodes
        let is_dragging = self.is_dragging();
        let drag_offset_x = if is_dragging { self.drag_offset_x().get() } else { 0.0 };
        let drag_offset_y = if is_dragging { self.drag_offset_y().get() } else { 0.0 };

        let mut closest_link_id: i32 = -1;
        let mut closest_distance = hover_distance;

        for (&link_id, link) in state.links.iter() {
            // Get node IDs from pin IDs
            let start_node_id = link.start_pin_id / 10;
            let end_node_id = link.end_pin_id / 10;

            // Find node rects
            let start_node_rect = state.node_rects.get(&start_node_id);
            let end_node_rect = state.node_rects.get(&end_node_id);

            if let (Some(start_rect), Some(end_rect)) = (start_node_rect, end_node_rect) {
                // Compute pin positions
                let (mut start_x, mut start_y) =
                    compute_pin_screen_position(link.start_pin_id, start_rect, zoom);
                let (mut end_x, mut end_y) =
                    compute_pin_screen_position(link.end_pin_id, end_rect, zoom);

                // Apply drag offset to selected nodes
                // Note: drag_offset is in world coordinates, pin positions are in screen coordinates
                if is_dragging {
                    if state.selected_node_ids.contains(&start_node_id) {
                        start_x += drag_offset_x * zoom;
                        start_y += drag_offset_y * zoom;
                    }
                    if state.selected_node_ids.contains(&end_node_id) {
                        end_x += drag_offset_x * zoom;
                        end_y += drag_offset_y * zoom;
                    }
                }

                // Create bezier curve
                let bezier = create_bezier_from_endpoints(start_x, start_y, end_x, end_y, zoom);

                // Calculate distance from mouse to bezier
                let distance = distance_to_bezier((position.x, position.y), &bezier);

                if distance < closest_distance {
                    closest_distance = distance;
                    closest_link_id = link_id;
                }
            }
        }

        closest_link_id
    }

    /// Update the hovered link state and fire callback if changed
    fn update_hovered_link(self: Pin<&Self>, position: LogicalPoint) {
        let new_hovered = self.find_link_at(position);
        let current_hovered = self.hovered_link_id();

        if new_hovered != current_hovered {
            Self::FIELD_OFFSETS.hovered_link_id.apply_pin(self).set(new_hovered);
            self.link_hovered.call(&());
        }
    }

    /// Handle a click on a link (selection logic)
    fn handle_link_click(self: Pin<&Self>, link_id: i32, shift_held: bool) {
        let mut state = self.data.state.borrow_mut();

        // Clear node selection when clicking a link (unless shift is held)
        if !shift_held {
            state.selected_node_ids.clear();
        }

        if shift_held {
            // Multi-select: toggle the clicked link
            if state.selected_link_ids.contains(&link_id) {
                state.selected_link_ids.remove(&link_id);
            } else {
                state.selected_link_ids.insert(link_id);
            }
        } else {
            // Single select: toggle if clicking on the only selected link, otherwise select only this link
            if state.selected_link_ids.len() == 1 && state.selected_link_ids.contains(&link_id) {
                // Deselect if clicking on the only selected link
                state.selected_link_ids.clear();
            } else {
                // Select only the clicked link
                state.selected_link_ids.clear();
                state.selected_link_ids.insert(link_id);
            }
        }

        // Build output strings
        let link_ids_str = state
            .selected_link_ids
            .iter()
            .map(|id| alloc::format!("{}", id))
            .collect::<alloc::vec::Vec<_>>()
            .join(",");

        let node_ids_str = state
            .selected_node_ids
            .iter()
            .map(|id| alloc::format!("{}", id))
            .collect::<alloc::vec::Vec<_>>()
            .join(",");

        drop(state);

        // Update output properties
        Self::FIELD_OFFSETS
            .current_selected_link_ids
            .apply_pin(self)
            .set(SharedString::from(&link_ids_str));
        Self::FIELD_OFFSETS
            .current_selected_ids
            .apply_pin(self)
            .set(SharedString::from(&node_ids_str));

        // Notify that selection changed
        self.link_selection_changed.call(&());
        self.selection_changed.call(&());
    }

    /// Clear link selection (called when clicking on nodes or background)
    fn clear_link_selection(self: Pin<&Self>) {
        let mut state = self.data.state.borrow_mut();
        if state.selected_link_ids.is_empty() {
            return;
        }
        state.selected_link_ids.clear();
        drop(state);

        Self::FIELD_OFFSETS
            .current_selected_link_ids
            .apply_pin(self)
            .set(SharedString::default());
        self.link_selection_changed.call(&());
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
