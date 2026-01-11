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
const BASE_PIN_SIZE: f32 = 12.0;
const PIN_Y_OFFSET: f32 = 8.0 + 24.0 + 8.0; // Margin + title height + margin
const GRID_SPACING: f32 = 24.0;

/// Snap a value to the nearest grid position
fn snap_to_grid(value: f32) -> f32 {
    (value / GRID_SPACING).round() * GRID_SPACING
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
            start_x: 0.0, start_y: 0.0, end_x: 0.0, end_y: 0.0, // Will be computed below
        },
        // Process (pin 22) -> Output (pin 31)
        LinkData {
            id: 2,
            start_pin_id: 22,
            end_pin_id: 31,
            color: link_colors[1],
            start_x: 0.0, start_y: 0.0, end_x: 0.0, end_y: 0.0, // Will be computed below
        },
    ]));
    window.set_links(ModelRc::from(links.clone()));

    // Report links to overlay for core-based position computation
    for i in 0..links.row_count() {
        if let Some(link) = links.row_data(i) {
            window.invoke_report_link(link.id, link.start_pin_id, link.end_pin_id, link.color);
        }
    }

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

    // Handle link position updates when viewport changes
    // Core computes positions, we just read and update the model
    let links_for_viewport = links.clone();
    let window_for_viewport = window.as_weak();
    window.on_update_viewport(move |_zoom, _pan_x, _pan_y| {
        if let Some(window) = window_for_viewport.upgrade() {
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
                        for i in 0..links_for_viewport.row_count() {
                            if let Some(mut link) = links_for_viewport.row_data(i) {
                                if link.id == id {
                                    link.start_x = start_x;
                                    link.start_y = start_y;
                                    link.end_x = end_x;
                                    link.end_y = end_y;
                                    links_for_viewport.set_row_data(i, link);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
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
            start_x: 0.0,
            start_y: 0.0,
            end_x: 0.0,
            end_y: 0.0,
        });

        // Report link to overlay for position computation
        if let Some(window) = window_for_create.upgrade() {
            window.invoke_report_link(id, start_pin, end_pin, color);
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

        // Link positions will be automatically updated by core when nodes move
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

    window.run().unwrap();
}
