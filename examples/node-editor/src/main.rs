// Node Editor Example
//
// Demonstrates the NodeEditorBackground and NodeEditorOverlay components
// for building visual node graph editors.

use slint::{Color, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

slint::include_modules!();

// Node dimensions (must match ui.slint)
const NODE_BASE_WIDTH: f32 = 150.0;
const NODE_BASE_HEIGHT: f32 = 80.0;
const GRID_SPACING: f32 = 24.0;

// Pin dimensions for computing pin positions (must match ui.slint and core)
const BASE_PIN_SIZE: f32 = 12.0;
const PIN_Y_OFFSET: f32 = 8.0 + 24.0 + 8.0; // Margin + title height + margin
const PIN_MARGIN: f32 = 8.0;

/// Snap a value to the nearest grid position
fn snap_to_grid(value: f32) -> f32 {
    (value / GRID_SPACING).round() * GRID_SPACING
}

/// Compute screen position for a pin given node world position and viewport
/// Pin ID format: node_id * 10 + pin_type (1 = input, 2 = output)
fn compute_pin_position(
    pin_id: i32,
    node_world_x: f32,
    node_world_y: f32,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
) -> (f32, f32) {
    let pin_type = pin_id % 10;
    let pin_size = BASE_PIN_SIZE * zoom;
    let pin_radius = pin_size / 2.0;

    // Node screen position
    let node_x = node_world_x * zoom + pan_x;
    let node_y = node_world_y * zoom + pan_y;
    let node_width = NODE_BASE_WIDTH * zoom;

    if pin_type == 1 {
        // Input pin: left side
        let x = node_x + PIN_MARGIN * zoom + pin_radius;
        let y = node_y + PIN_Y_OFFSET * zoom + pin_radius;
        (x, y)
    } else {
        // Output pin: right side
        let x = node_x + node_width - PIN_MARGIN * zoom - pin_size + pin_radius;
        let y = node_y + PIN_Y_OFFSET * zoom + pin_radius;
        (x, y)
    }
}

/// Compute link positions from node data
fn compute_link_positions(
    links: &VecModel<LinkData>,
    nodes: &VecModel<NodeData>,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
) {
    // Build a map of node_id -> (world_x, world_y)
    let node_positions: std::collections::HashMap<i32, (f32, f32)> = (0..nodes.row_count())
        .filter_map(|i| nodes.row_data(i))
        .map(|n| (n.id, (n.world_x, n.world_y)))
        .collect();

    // Update each link's positions
    for i in 0..links.row_count() {
        if let Some(mut link) = links.row_data(i) {
            let start_node_id = link.start_pin_id / 10;
            let end_node_id = link.end_pin_id / 10;

            if let (Some(&(start_wx, start_wy)), Some(&(end_wx, end_wy))) =
                (node_positions.get(&start_node_id), node_positions.get(&end_node_id))
            {
                let (start_x, start_y) =
                    compute_pin_position(link.start_pin_id, start_wx, start_wy, zoom, pan_x, pan_y);
                let (end_x, end_y) =
                    compute_pin_position(link.end_pin_id, end_wx, end_wy, zoom, pan_x, pan_y);

                link.start_x = start_x;
                link.start_y = start_y;
                link.end_x = end_x;
                link.end_y = end_y;
                links.set_row_data(i, link);
            }
        }
    }
}

/// Build node rects batch string from model data and current viewport
/// Format: "id,screen_x,screen_y,width,height;..."
fn build_node_rects_batch(nodes: &VecModel<NodeData>, zoom: f32, pan_x: f32, pan_y: f32) -> String {
    (0..nodes.row_count())
        .filter_map(|i| nodes.row_data(i))
        .map(|node| {
            // Compute screen position: (world_pos) * zoom + pan
            let screen_x = node.world_x * zoom + pan_x;
            let screen_y = node.world_y * zoom + pan_y;
            let width = NODE_BASE_WIDTH * zoom;
            let height = NODE_BASE_HEIGHT * zoom;
            format!("{},{},{},{},{}", node.id, screen_x, screen_y, width, height)
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Build pin positions batch string from model data and current viewport
/// Format: "pin_id,screen_x,screen_y;..."
/// Pin IDs: node_id * 10 + 1 for input, node_id * 10 + 2 for output
fn build_pins_batch(nodes: &VecModel<NodeData>, zoom: f32, pan_x: f32, pan_y: f32) -> String {
    // Pin layout constants (must match ui.slint)
    const PIN_MARGIN: f32 = 8.0;
    const PIN_SIZE: f32 = 12.0;
    const TITLE_HEIGHT: f32 = 24.0;

    let pin_radius = PIN_SIZE / 2.0;
    let pin_y_offset = PIN_MARGIN + TITLE_HEIGHT + PIN_MARGIN + pin_radius;

    (0..nodes.row_count())
        .filter_map(|i| nodes.row_data(i))
        .flat_map(|node| {
            let node_screen_x = node.world_x * zoom + pan_x;
            let node_screen_y = node.world_y * zoom + pan_y;

            // Input pin: left side
            let input_pin_id = node.id * 10 + 1;
            let input_x = node_screen_x + (PIN_MARGIN + pin_radius) * zoom;
            let input_y = node_screen_y + pin_y_offset * zoom;

            // Output pin: right side
            let output_pin_id = node.id * 10 + 2;
            let output_x = node_screen_x + (NODE_BASE_WIDTH - PIN_MARGIN - PIN_SIZE + pin_radius) * zoom;
            let output_y = node_screen_y + pin_y_offset * zoom;

            vec![
                format!("{},{},{}", input_pin_id, input_x, input_y),
                format!("{},{},{}", output_pin_id, output_x, output_y),
            ]
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn main() {
    let window = MainWindow::new().unwrap();

    // Create the node model
    let nodes: Rc<VecModel<NodeData>> = Rc::new(VecModel::from(vec![
        NodeData {
            id: 1,
            title: SharedString::from("Input"),
            world_x: 96.0,  // Grid-aligned (4 * 24)
            world_y: 192.0, // Grid-aligned (8 * 24)
            selected: false,
        },
        NodeData {
            id: 2,
            title: SharedString::from("Process"),
            world_x: 360.0, // Grid-aligned (15 * 24)
            world_y: 144.0, // Grid-aligned (6 * 24)
            selected: false,
        },
        NodeData {
            id: 3,
            title: SharedString::from("Output"),
            world_x: 600.0, // Grid-aligned (25 * 24)
            world_y: 192.0, // Grid-aligned (8 * 24)
            selected: false,
        },
    ]));

    // Set the model on the window
    window.set_nodes(ModelRc::from(nodes.clone()));

    // Track next node ID for creating new nodes
    let next_node_id = Rc::new(RefCell::new(4)); // Start after initial nodes (1, 2, 3)

    // Create the links model with initial connections
    // Link colors for variety
    let link_colors = [
        Color::from_argb_u8(255, 255, 152, 0),   // Orange
        Color::from_argb_u8(255, 33, 150, 243),  // Blue
        Color::from_argb_u8(255, 76, 175, 80),   // Green
        Color::from_argb_u8(255, 156, 39, 176),  // Purple
        Color::from_argb_u8(255, 233, 30, 99),   // Pink
    ];
    let next_link_id = Rc::new(RefCell::new(3)); // Start after initial links
    let color_index = Rc::new(RefCell::new(2)); // Start after initial colors

    let links: Rc<VecModel<LinkData>> = Rc::new(VecModel::from(vec![
        // Input (pin 12) -> Process (pin 21)
        LinkData {
            id: 1,
            start_pin_id: 12,
            end_pin_id: 21,
            color: link_colors[0],
            path_commands: SharedString::default(), // Will be computed by core
            start_x: 0.0, start_y: 0.0, end_x: 0.0, end_y: 0.0, // Will be computed below
        },
        // Process (pin 22) -> Output (pin 31)
        LinkData {
            id: 2,
            start_pin_id: 22,
            end_pin_id: 31,
            color: link_colors[1],
            path_commands: SharedString::default(), // Will be computed by core
            start_x: 0.0, start_y: 0.0, end_x: 0.0, end_y: 0.0, // Will be computed below
        },
    ]));
    window.set_links(ModelRc::from(links.clone()));

    // Compute initial link positions so they're visible immediately
    // (don't wait for the callback chain which doesn't work on the first frame)
    compute_link_positions(&links, &nodes, 1.0, 0.0, 0.0);

    // Report links to overlay for core-based position computation (using batch)
    // Format: "id,start_pin,end_pin,color_argb;..."
    let batch: Vec<String> = (0..links.row_count())
        .filter_map(|i| links.row_data(i))
        .map(|link| {
            format!(
                "{},{},{},{}",
                link.id,
                link.start_pin_id,
                link.end_pin_id,
                link.color.as_argb_encoded()
            )
        })
        .collect();
    window.set_pending_links_batch(SharedString::from(batch.join(";").as_str()));

    // Report initial node rects and pin positions to overlay (using batch)
    // Initial zoom=1.0, pan_x=0, pan_y=0
    let initial_zoom = 1.0f32;
    let initial_pan_x = 0.0f32;
    let initial_pan_y = 0.0f32;
    let node_rects_batch = build_node_rects_batch(&nodes, initial_zoom, initial_pan_x, initial_pan_y);
    window.set_pending_node_rects_batch(SharedString::from(node_rects_batch.as_str()));
    let pins_batch = build_pins_batch(&nodes, initial_zoom, initial_pan_x, initial_pan_y);
    window.set_pending_pins_batch(SharedString::from(pins_batch.as_str()));

    // Handle selection changes - sync overlay's selection state to NodeData model
    let nodes_for_selection = nodes.clone();
    window.on_selection_changed(move |selected_ids_str| {
        let selected_ids: std::collections::HashSet<i32> = selected_ids_str
            .split(',')
            .filter_map(|s| s.trim().parse::<i32>().ok())
            .collect();

        // Update the selected field for all nodes
        for i in 0..nodes_for_selection.row_count() {
            if let Some(mut node) = nodes_for_selection.row_data(i) {
                let should_select = selected_ids.contains(&node.id);
                if node.selected != should_select {
                    node.selected = should_select;
                    nodes_for_selection.set_row_data(i, node);
                }
            }
        }
    });

    // Handle link position updates from core
    // This is called whenever the core regenerates link positions (viewport changes, node moves, etc.)
    let links_for_sync = links.clone();
    let window_for_sync = window.as_weak();
    window.on_link_positions_updated(move || {
        if let Some(window) = window_for_sync.upgrade() {
            let link_data_str = window.get_link_positions_data();

            // Parse format: "id,start_x,start_y,end_x,end_y,color_argb;..."
            for link_str in link_data_str.split(';') {
                if link_str.is_empty() {
                    continue;
                }

                let parts: Vec<&str> = link_str.split(',').collect();
                if parts.len() >= 5 {
                    if let (Ok(id), Ok(start_x), Ok(start_y), Ok(end_x), Ok(end_y)) = (
                        parts[0].parse::<i32>(),
                        parts[1].parse::<f32>(),
                        parts[2].parse::<f32>(),
                        parts[3].parse::<f32>(),
                        parts[4].parse::<f32>(),
                    ) {
                        // Find and update the link in the model
                        for i in 0..links_for_sync.row_count() {
                            if let Some(mut link) = links_for_sync.row_data(i) {
                                if link.id == id {
                                    link.start_x = start_x;
                                    link.start_y = start_y;
                                    link.end_x = end_x;
                                    link.end_y = end_y;
                                    links_for_sync.set_row_data(i, link);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    // Handle viewport changes - update node rects and pin positions when pan/zoom changes
    let nodes_for_viewport = nodes.clone();
    let window_for_viewport = window.as_weak();
    window.on_update_viewport(move |zoom, pan_x, pan_y| {
        // Rebuild node rects and pin positions with new viewport parameters
        if let Some(window) = window_for_viewport.upgrade() {
            let node_batch = build_node_rects_batch(&nodes_for_viewport, zoom, pan_x, pan_y);
            window.set_pending_node_rects_batch(SharedString::from(node_batch.as_str()));

            let pins_batch = build_pins_batch(&nodes_for_viewport, zoom, pan_x, pan_y);
            window.set_pending_pins_batch(SharedString::from(pins_batch.as_str()));
        }
    });

    // Handle link creation
    let links_for_create = links.clone();
    let next_link_id_for_create = next_link_id.clone();
    let color_index_for_create = color_index.clone();
    let window_for_create = window.as_weak();
    window.on_create_link(move |start_pin, end_pin| {
        let id = *next_link_id_for_create.borrow();
        *next_link_id_for_create.borrow_mut() += 1;

        let idx = *color_index_for_create.borrow();
        *color_index_for_create.borrow_mut() = (idx + 1) % link_colors.len();

        let color = link_colors[idx];

        // Add link to model (positions will be computed by core)
        links_for_create.push(LinkData {
            id,
            start_pin_id: start_pin,
            end_pin_id: end_pin,
            color,
            path_commands: SharedString::default(), // Will be computed by core
            start_x: 0.0,
            start_y: 0.0,
            end_x: 0.0,
            end_y: 0.0,
        });

        // Report link to overlay for position computation (append to batch)
        if let Some(window) = window_for_create.upgrade() {
            let current_batch = window.get_pending_links_batch();
            let new_entry = format!("{},{},{},{}", id, start_pin, end_pin, color.as_argb_encoded());
            let new_batch = if current_batch.is_empty() {
                new_entry
            } else {
                format!("{};{}", current_batch, new_entry)
            };
            window.set_pending_links_batch(SharedString::from(new_batch.as_str()));
        }
    });

    // Node selection is now handled by the overlay (overlay.clicked-node-id)

    // Handle drag commit - apply delta to all selected nodes when drag ends
    let nodes_for_drag = nodes.clone();
    let window_for_drag = window.as_weak();
    window.on_commit_drag(move |delta_x, delta_y, snap_enabled| {
        // Get selected node IDs from overlay
        let selected_ids: std::collections::HashSet<i32> = if let Some(window) = window_for_drag.upgrade() {
            window.get_current_selected_ids()
                .split(',')
                .filter_map(|s| s.trim().parse::<i32>().ok())
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        // Apply delta to all selected nodes, optionally snapping to grid
        for i in 0..nodes_for_drag.row_count() {
            if let Some(mut node) = nodes_for_drag.row_data(i) {
                if selected_ids.contains(&node.id) {
                    let new_x = node.world_x + delta_x;
                    let new_y = node.world_y + delta_y;
                    node.world_x = if snap_enabled { snap_to_grid(new_x) } else { new_x };
                    node.world_y = if snap_enabled { snap_to_grid(new_y) } else { new_y };
                    nodes_for_drag.set_row_data(i, node);
                }
            }
        }

        // Update node rects and pin positions in core so link positions are recomputed
        if let Some(window) = window_for_drag.upgrade() {
            let zoom = window.get_zoom();
            let pan_x = window.get_pan_x();
            let pan_y = window.get_pan_y();
            let node_batch = build_node_rects_batch(&nodes_for_drag, zoom, pan_x, pan_y);
            window.set_pending_node_rects_batch(SharedString::from(node_batch.as_str()));
            let pins_batch = build_pins_batch(&nodes_for_drag, zoom, pan_x, pan_y);
            window.set_pending_pins_batch(SharedString::from(pins_batch.as_str()));
        }
    });

    // Handle deleting selected nodes
    let nodes_for_delete = nodes.clone();
    let links_for_delete = links.clone();
    let window_for_delete = window.as_weak();
    window.on_delete_selected_nodes(move || {
        // Get selected node IDs from overlay
        let selected_ids: std::collections::HashSet<i32> = if let Some(window) = window_for_delete.upgrade() {
            window.get_current_selected_ids()
                .split(',')
                .filter_map(|s| s.trim().parse::<i32>().ok())
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        // Collect indices of selected nodes (in reverse order for safe removal)
        let mut indices_to_remove: Vec<usize> = Vec::new();
        let mut deleted_node_ids: Vec<i32> = Vec::new();

        for i in 0..nodes_for_delete.row_count() {
            if let Some(node) = nodes_for_delete.row_data(i) {
                if selected_ids.contains(&node.id) {
                    indices_to_remove.push(i);
                    deleted_node_ids.push(node.id);
                }
            }
        }

        // Remove nodes in reverse order to maintain valid indices
        for &i in indices_to_remove.iter().rev() {
            nodes_for_delete.remove(i);
        }

        // Also remove any links connected to deleted nodes
        // Pin IDs are node_id * 10 + pin_type, so we check if pin's node is deleted
        let mut link_indices_to_remove: Vec<usize> = Vec::new();
        for i in 0..links_for_delete.row_count() {
            if let Some(link) = links_for_delete.row_data(i) {
                let start_node_id = link.start_pin_id / 10;
                let end_node_id = link.end_pin_id / 10;
                if deleted_node_ids.contains(&start_node_id)
                    || deleted_node_ids.contains(&end_node_id)
                {
                    link_indices_to_remove.push(i);
                }
            }
        }

        // Remove links in reverse order
        for &i in link_indices_to_remove.iter().rev() {
            links_for_delete.remove(i);
        }
    });

    // Handle adding new nodes (Ctrl+N)
    let nodes_for_add = nodes.clone();
    let next_node_id_for_add = next_node_id.clone();
    window.on_add_node(move || {
        let id = *next_node_id_for_add.borrow();
        *next_node_id_for_add.borrow_mut() += 1;

        // Add new node at a grid-snapped position
        // Offset each new node slightly to avoid stacking
        nodes_for_add.push(NodeData {
            id,
            title: SharedString::from(format!("Node {}", id)),
            world_x: snap_to_grid(192.0 + (id as f32 * 48.0) % 384.0),
            world_y: snap_to_grid(192.0 + (id as f32 * 24.0) % 288.0),
            selected: false,
        });
    });

    // Box selection is now fully handled by the overlay
    // (overlay computes intersecting nodes and updates current-selected-ids)

    // Link positions are synced automatically via link-positions-changed callback
    // when nodes report their rects during initialization

    window.run().unwrap();
}
