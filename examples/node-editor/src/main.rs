// Node Editor Example
//
// Demonstrates the NodeEditorBackground and NodeEditorOverlay components
// for building visual node graph editors.

use slint::{Color, LinkData, Model, ModelRc, NodeData, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

slint::include_modules!();

/// Pin type constants (must match PinTypes global in Slint)
mod pin_types {
    pub const OUTPUT: i32 = 2;
}

/// Pin validation utilities (matches PinId global in Slint)
fn is_output_pin(pin_id: i32) -> bool {
    (pin_id % 10) == pin_types::OUTPUT
}

fn are_pins_compatible(start_pin: i32, end_pin: i32) -> bool {
    if start_pin == end_pin {
        return false;
    }
    let start_node = start_pin / 10;
    let end_node = end_pin / 10;
    if start_node == end_node {
        return false;
    }
    is_output_pin(start_pin) != is_output_pin(end_pin)
}

/// Compute graph bounds from all nodes
fn compute_graph_bounds(
    nodes: &VecModel<NodeData>,
    filter_nodes: &VecModel<FilterNodeData>,
    window: &MainWindow,
) -> (f32, f32, f32, f32) {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;

    // Get node dimensions from Slint
    let node_width = window.invoke_compute_node_screen_width();
    let node_height = window.invoke_compute_node_screen_height();
    let filter_width = window.invoke_compute_filter_screen_width();
    let filter_height = window.invoke_compute_filter_screen_height();

    // Process simple nodes
    for i in 0..nodes.row_count() {
        if let Some(node) = nodes.row_data(i) {
            min_x = min_x.min(node.world_x);
            min_y = min_y.min(node.world_y);
            max_x = max_x.max(node.world_x + node_width);
            max_y = max_y.max(node.world_y + node_height);
        }
    }

    // Process filter nodes
    for i in 0..filter_nodes.row_count() {
        if let Some(node) = filter_nodes.row_data(i) {
            min_x = min_x.min(node.world_x);
            min_y = min_y.min(node.world_y);
            max_x = max_x.max(node.world_x + filter_width);
            max_y = max_y.max(node.world_y + filter_height);
        }
    }

    // Return sensible defaults if no nodes
    if min_x == f32::MAX {
        (0.0, 0.0, 1600.0, 1200.0)
    } else {
        // Add some padding to bounds
        (min_x - 50.0, min_y - 50.0, max_x + 50.0, max_y + 50.0)
    }
}

/// Build minimap nodes from all nodes
fn build_minimap_nodes(
    nodes: &VecModel<NodeData>,
    filter_nodes: &VecModel<FilterNodeData>,
    window: &MainWindow,
) -> ModelRc<MinimapNode> {
    let mut minimap_nodes = Vec::new();

    let node_width = window.invoke_compute_node_screen_width();
    let node_height = window.invoke_compute_node_screen_height();
    let filter_width = window.invoke_compute_filter_screen_width();
    let filter_height = window.invoke_compute_filter_screen_height();

    // Add simple nodes
    for i in 0..nodes.row_count() {
        if let Some(node) = nodes.row_data(i) {
            minimap_nodes.push(MinimapNode {
                id: node.id,
                x: node.world_x,
                y: node.world_y,
                width: node_width,
                height: node_height,
                color: Color::from_rgb_u8(80, 120, 200), // Blue for simple nodes
            });
        }
    }

    // Add filter nodes
    for i in 0..filter_nodes.row_count() {
        if let Some(node) = filter_nodes.row_data(i) {
            minimap_nodes.push(MinimapNode {
                id: node.id,
                x: node.world_x,
                y: node.world_y,
                width: filter_width,
                height: filter_height,
                color: Color::from_rgb_u8(200, 120, 80), // Orange for filter nodes
            });
        }
    }

    Rc::new(VecModel::from(minimap_nodes)).into()
}

/// Update minimap data (nodes and bounds)
fn update_minimap_data(
    window: &MainWindow,
    nodes: &VecModel<NodeData>,
    filter_nodes: &VecModel<FilterNodeData>,
) {
    // Build minimap nodes
    let minimap_nodes = build_minimap_nodes(nodes, filter_nodes, window);
    window.set_minimap_nodes(minimap_nodes);

    // Compute and set graph bounds
    let (min_x, min_y, max_x, max_y) = compute_graph_bounds(nodes, filter_nodes, window);
    window.set_graph_min_x(min_x);
    window.set_graph_min_y(min_y);
    window.set_graph_max_x(max_x);
    window.set_graph_max_y(max_y);
}

fn main() {
    let window = MainWindow::new().unwrap();

    // Create the node model
    let nodes: Rc<VecModel<NodeData>> = Rc::new(VecModel::from(vec![
        NodeData {
            id: 1,
            title: SharedString::from("Input"),
            world_x: 144.0,  // Grid-aligned (6 * 24)
            world_y: 264.0,  // Grid-aligned (11 * 24)
        },
        NodeData {
            id: 2,
            title: SharedString::from("Process"),
            world_x: 408.0, // Grid-aligned (17 * 24)
            world_y: 216.0, // Grid-aligned (9 * 24)
        },
        NodeData {
            id: 3,
            title: SharedString::from("Output"),
            world_x: 648.0, // Grid-aligned (27 * 24)
            world_y: 264.0, // Grid-aligned (11 * 24)
        },
    ]));

    // Set the model on the window
    window.set_nodes(ModelRc::from(nodes.clone()));

    // Create the filter nodes model (complex nodes with widgets)
    let filter_nodes: Rc<VecModel<FilterNodeData>> = Rc::new(VecModel::from(vec![
        FilterNodeData {
            id: 100,  // Use higher IDs to avoid conflicts with simple nodes
            title: SharedString::from("Filter"),
            world_x: 408.0,  // Between Input and Output nodes
            world_y: 384.0,  // Below the main chain
            filter_type_index: 0,
            enabled: true,
            processed_count: 42,
        },
    ]));
    window.set_filter_nodes(ModelRc::from(filter_nodes.clone()));

    // Track next node ID for creating new nodes
    let next_node_id = Rc::new(RefCell::new(4)); // Start after initial simple nodes (1, 2, 3)

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
        },
        // Process (pin 22) -> Output (pin 31)
        LinkData {
            id: 2,
            start_pin_id: 22,
            end_pin_id: 31,
            color: link_colors[1],
            path_commands: SharedString::default(), // Will be computed by core
        },
    ]));
    window.set_links(ModelRc::from(links.clone()));

    // Note: Link path-commands are computed by core after pins are reported

    // Enable minimap
    window.set_minimap_enabled(true);

    // Set initial minimap data
    update_minimap_data(&window, &nodes, &filter_nodes);

    // Implement selection checking callbacks
    // These parse the comma-separated string properties maintained by the editor
    let window_for_node_selection = window.as_weak();
    window.on_is_node_selected(move |node_id| {
        if let Some(window) = window_for_node_selection.upgrade() {
            let selected_ids_str = window.get_selected_node_ids_str();
            if selected_ids_str.is_empty() {
                return false;
            }
            // Parse comma-separated IDs and check if node_id is in the list
            selected_ids_str
                .split(',')
                .filter_map(|s| s.trim().parse::<i32>().ok())
                .any(|id| id == node_id)
        } else {
            false
        }
    });

    let window_for_link_selection = window.as_weak();
    window.on_is_link_selected(move |link_id| {
        if let Some(window) = window_for_link_selection.upgrade() {
            let selected_ids_str = window.get_selected_link_ids_str();
            if selected_ids_str.is_empty() {
                return false;
            }
            // Parse comma-separated IDs and check if link_id is in the list
            selected_ids_str
                .split(',')
                .filter_map(|s| s.trim().parse::<i32>().ok())
                .any(|id| id == link_id)
        } else {
            false
        }
    });

    // Handle viewport changes - update node rects and pin positions when pan/zoom changes
    // Positions computed by Slint using globals
    window.on_update_viewport(move |_zoom, _pan_x, _pan_y| {
        // Redundant batch updates removed - nodes and pins report themselves automatically via callbacks
    });

    // Handle link creation - validates compatibility and checks for duplicates
    let links_for_create = links.clone();
    let next_link_id_for_create = next_link_id.clone();
    let color_index_for_create = color_index.clone();
    let window_for_create = window.as_weak();
    window.on_create_link(move |start_pin, end_pin| {
        println!("[MAIN] on_create_link called: start={}, end={}", start_pin, end_pin);

        let _window = match window_for_create.upgrade() {
            Some(w) => w,
            None => {
                println!("[MAIN] Window upgrade failed");
                return;
            }
        };

        // Validate pin compatibility (application-layer validation)
        if !are_pins_compatible(start_pin, end_pin) {
            println!("[MAIN] Pins not compatible");
            return; // Incompatible pins, silently ignore
        }

        // Normalize to (output, input) for consistent storage
        let (output_pin, input_pin) = if is_output_pin(start_pin) {
            (start_pin, end_pin)
        } else {
            (end_pin, start_pin)
        };
        println!("[MAIN] Normalized: output={}, input={}", output_pin, input_pin);

        // Check for duplicate links
        for i in 0..links_for_create.row_count() {
            if let Some(link) = links_for_create.row_data(i) {
                if link.start_pin_id == output_pin && link.end_pin_id == input_pin {
                    println!("[MAIN] Duplicate link detected");
                    return; // Duplicate link, ignore
                }
            }
        }

        // Valid and unique - create the link
        let id = *next_link_id_for_create.borrow();
        *next_link_id_for_create.borrow_mut() += 1;

        let idx = *color_index_for_create.borrow();
        *color_index_for_create.borrow_mut() = (idx + 1) % link_colors.len();

        let color = link_colors[idx];

        println!("[MAIN] Creating link with id={}, color={:?}", id, color);

        // Add link to model (path_commands will be computed by core)
        links_for_create.push(LinkData {
            id,
            start_pin_id: output_pin,
            end_pin_id: input_pin,
            color,
            path_commands: SharedString::default(), // Will be computed by core
        });

        println!("[MAIN] Link added to model, total links: {}", links_for_create.row_count());
    });

    // Node selection is now handled by the overlay (overlay.clicked-node-id)

    // Handle drag commit - apply delta to all selected nodes when drag ends
    // Delta is already snapped if grid-snapping is enabled
    let nodes_for_drag = nodes.clone();
    let filter_nodes_for_drag = filter_nodes.clone();
    let window_for_drag = window.as_weak();
    window.on_commit_drag(move |delta_x, delta_y| {
        // Get window reference and selected node IDs from overlay
        let window = match window_for_drag.upgrade() {
            Some(w) => w,
            None => return,
        };

        let selected_ids: std::collections::HashSet<i32> = window.get_selected_node_ids_str()
            .split(',')
            .filter_map(|s| s.trim().parse::<i32>().ok())
            .collect();

        // Apply delta to all selected simple nodes (delta is already snapped)
        for i in 0..nodes_for_drag.row_count() {
            if let Some(mut node) = nodes_for_drag.row_data(i) {
                if selected_ids.contains(&node.id) {
                    node.world_x += delta_x;
                    node.world_y += delta_y;
                    nodes_for_drag.set_row_data(i, node);
                }
            }
        }

        // Apply delta to all selected filter nodes (delta is already snapped)
        for i in 0..filter_nodes_for_drag.row_count() {
            if let Some(mut node) = filter_nodes_for_drag.row_data(i) {
                if selected_ids.contains(&node.id) {
                    node.world_x += delta_x;
                    node.world_y += delta_y;
                    filter_nodes_for_drag.set_row_data(i, node);
                }
            }
        }

        // Update minimap data
        update_minimap_data(&window, &nodes_for_drag, &filter_nodes_for_drag);
    });

    // Handle deleting selected nodes
    let nodes_for_delete = nodes.clone();
    let filter_nodes_for_delete = filter_nodes.clone();
    let links_for_delete = links.clone();
    let window_for_delete = window.as_weak();
    window.on_delete_selected_nodes(move || {
        let window = match window_for_delete.upgrade() {
            Some(w) => w,
            None => return,
        };

        // Get selected node IDs from overlay
        let selected_ids: std::collections::HashSet<i32> = window
            .get_selected_node_ids_str()
            .split(',')
            .filter_map(|s| s.trim().parse::<i32>().ok())
            .collect();

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

        // Also check and delete from filter nodes
        let mut filter_indices_to_remove: Vec<usize> = Vec::new();
        for i in 0..filter_nodes_for_delete.row_count() {
            if let Some(filter_node) = filter_nodes_for_delete.row_data(i) {
                if selected_ids.contains(&filter_node.id) {
                    filter_indices_to_remove.push(i);
                    deleted_node_ids.push(filter_node.id);
                }
            }
        }

        // Remove filter nodes in reverse order
        for &i in filter_indices_to_remove.iter().rev() {
            filter_nodes_for_delete.remove(i);
        }

        // Also remove any links connected to deleted nodes
        // Pin IDs encode node ID: pin_id = node_id * 10 + pin_type (see PinId global)
        let mut link_indices_to_remove: Vec<usize> = Vec::new();
        let mut deleted_link_ids: Vec<i32> = Vec::new();
        for i in 0..links_for_delete.row_count() {
            if let Some(link) = links_for_delete.row_data(i) {
                let start_node_id = link.start_pin_id / 10;  // PinId.get-node-id()
                let end_node_id = link.end_pin_id / 10;      // PinId.get-node-id()
                if deleted_node_ids.contains(&start_node_id)
                    || deleted_node_ids.contains(&end_node_id)
                {
                    link_indices_to_remove.push(i);
                    deleted_link_ids.push(link.id);
                }
            }
        }

        // Remove links in reverse order
        for &i in link_indices_to_remove.iter().rev() {
            links_for_delete.remove(i);
        }

        // Update minimap data
        update_minimap_data(&window, &nodes_for_delete, &filter_nodes_for_delete);
    });

    // Handle deleting selected links
    let links_for_link_delete = links.clone();
    let window_for_link_delete = window.as_weak();
    window.on_delete_selected_links(move || {
        let window = match window_for_link_delete.upgrade() {
            Some(w) => w,
            None => return,
        };

        // Get selected link IDs from overlay
        let selected_link_ids: std::collections::HashSet<i32> = window
            .get_selected_link_ids_str()
            .split(',')
            .filter_map(|s| s.trim().parse::<i32>().ok())
            .collect();

        if selected_link_ids.is_empty() {
            return;
        }

        // Collect indices and IDs of selected links (in reverse order for safe removal)
        let mut indices_to_remove: Vec<usize> = Vec::new();
        let mut ids_to_delete: Vec<i32> = Vec::new();

        for i in 0..links_for_link_delete.row_count() {
            if let Some(link) = links_for_link_delete.row_data(i) {
                if selected_link_ids.contains(&link.id) {
                    indices_to_remove.push(i);
                    ids_to_delete.push(link.id);
                }
            }
        }

        // Remove links in reverse order
        for &i in indices_to_remove.iter().rev() {
            links_for_link_delete.remove(i);
        }
    });


    // Handle adding new nodes (Ctrl+N)
    let nodes_for_add = nodes.clone();
    let filter_nodes_for_add = filter_nodes.clone();
    let next_node_id_for_add = next_node_id.clone();
    let window_for_add = window.as_weak();
    window.on_add_node(move || {
        let window = match window_for_add.upgrade() {
            Some(w) => w,
            None => return,
        };

        let id = *next_node_id_for_add.borrow();
        *next_node_id_for_add.borrow_mut() += 1;

        // Add new node at a grid-snapped position
        // Offset each new node slightly to avoid stacking
        nodes_for_add.push(NodeData {
            id,
            title: SharedString::from(format!("Node {}", id)),
            world_x: window.invoke_snap_to_grid(192.0 + (id as f32 * 48.0) % 384.0),
            world_y: window.invoke_snap_to_grid(192.0 + (id as f32 * 24.0) % 288.0),
        });

        // Update minimap data
        update_minimap_data(&window, &nodes_for_add, &filter_nodes_for_add);
    });

    // Box selection is now fully handled by the overlay
    // (overlay computes intersecting nodes and updates current-selected-ids)

    // Link positions are synced automatically via link-positions-changed callback
    // when nodes report their rects during initialization

    // Filter node callbacks
    let filter_nodes_for_type = filter_nodes.clone();
    window.on_filter_type_changed(move |node_id, new_index| {
        for i in 0..filter_nodes_for_type.row_count() {
            if let Some(mut node) = filter_nodes_for_type.row_data(i) {
                if node.id == node_id {
                    node.filter_type_index = new_index;
                    filter_nodes_for_type.set_row_data(i, node);
                    println!("Filter {} type changed to: {}", node_id, new_index);
                    break;
                }
            }
        }
    });

    let filter_nodes_for_enable = filter_nodes.clone();
    window.on_filter_toggle_enabled(move |node_id| {
        for i in 0..filter_nodes_for_enable.row_count() {
            if let Some(mut node) = filter_nodes_for_enable.row_data(i) {
                if node.id == node_id {
                    node.enabled = !node.enabled;
                    let enabled = node.enabled;
                    filter_nodes_for_enable.set_row_data(i, node);
                    println!("Filter {} enabled: {}", node_id, enabled);
                    break;
                }
            }
        }
    });

    let filter_nodes_for_reset = filter_nodes.clone();
    window.on_filter_reset(move |node_id| {
        for i in 0..filter_nodes_for_reset.row_count() {
            if let Some(mut node) = filter_nodes_for_reset.row_data(i) {
                if node.id == node_id {
                    node.processed_count = 0;
                    node.filter_type_index = 0;
                    node.enabled = true;
                    filter_nodes_for_reset.set_row_data(i, node);
                    println!("Filter {} reset", node_id);
                    break;
                }
            }
        }
    });

    // Request a redraw to ensure initial link positions are computed
    // (the overlay processes batches in render(), which needs to be triggered)
    window.window().request_redraw();

    window.run().unwrap();
}
