// Node Editor Example
//
// Demonstrates the NodeEditorBackground and NodeEditorOverlay components
// for building visual node graph editors.

use slint::{Color, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

slint::include_modules!();

/// Build node rects batch string from model data using Slint-computed positions
/// Format: "id,screen_x,screen_y,width,height;..."
/// Slint computes positions using globals - Rust just queries
fn build_node_rects_batch(window: &MainWindow, nodes: &VecModel<NodeData>) -> String {
    (0..nodes.row_count())
        .filter_map(|i| nodes.row_data(i))
        .map(|node| {
            // Call Slint pure functions to compute positions using globals
            let screen_x = window.invoke_compute_node_screen_x(node.world_x);
            let screen_y = window.invoke_compute_node_screen_y(node.world_y);
            let width = window.invoke_compute_node_screen_width();
            let height = window.invoke_compute_node_screen_height();
            format!("{},{},{},{},{}",
                node.id,
                screen_x,
                screen_y,
                width,
                height)
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Get pin screen position for a given pin ID using Slint-computed positions
fn get_pin_position(window: &MainWindow, nodes: &VecModel<NodeData>, pin_id: i32) -> Option<(f32, f32)> {
    let node_id = pin_id / 10;
    let pin_type = pin_id % 10;

    for i in 0..nodes.row_count() {
        if let Some(node) = nodes.row_data(i) {
            if node.id == node_id {
                let (x, y) = if pin_type == 1 {
                    // Input pin
                    (window.invoke_compute_input_pin_x(node.world_x),
                     window.invoke_compute_input_pin_y(node.world_y))
                } else {
                    // Output pin
                    (window.invoke_compute_output_pin_x(node.world_x),
                     window.invoke_compute_output_pin_y(node.world_y))
                };
                return Some((x, y));
            }
        }
    }
    None
}

/// Build bezier path commands for all links using Slint-computed positions
/// Format: "id|path_commands|color_argb;..."
fn build_link_bezier_paths(
    window: &MainWindow,
    nodes: &VecModel<NodeData>,
    links: &VecModel<LinkData>,
) -> String {
    let zoom = window.get_zoom();
    links
        .iter()
        .filter_map(|link| {
            let start_pos = get_pin_position(window, nodes, link.start_pin_id)?;
            let end_pos = get_pin_position(window, nodes, link.end_pin_id)?;

            // Generate bezier path command
            let (start_x, start_y) = start_pos;
            let (end_x, end_y) = end_pos;

            // Horizontal distance determines control point offset
            let dx = (end_x - start_x).abs();
            let offset = (dx * 0.5).max(50.0 * zoom);

            // Output pin (start) curves right, input pin (end) curves left
            let ctrl1_x = start_x + offset;
            let ctrl1_y = start_y;
            let ctrl2_x = end_x - offset;
            let ctrl2_y = end_y;

            let path_cmd = format!(
                "M {} {} C {} {} {} {} {} {}",
                start_x, start_y, ctrl1_x, ctrl1_y, ctrl2_x, ctrl2_y, end_x, end_y
            );

            let color_argb = link.color.as_argb_encoded();
            Some(format!("{}|{}|{}", link.id, path_cmd, color_argb))
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Build filter node rects batch string using Slint-computed positions
/// Format: "id,screen_x,screen_y,width,height;..."
fn build_filter_node_rects_batch(window: &MainWindow, filter_nodes: &VecModel<FilterNodeData>) -> String {
    (0..filter_nodes.row_count())
        .filter_map(|i| filter_nodes.row_data(i))
        .map(|node| {
            let screen_x = window.invoke_compute_node_screen_x(node.world_x);
            let screen_y = window.invoke_compute_node_screen_y(node.world_y);
            let width = window.invoke_compute_filter_screen_width();
            let height = window.invoke_compute_filter_screen_height();
            format!("{},{},{},{},{}", node.id, screen_x, screen_y, width, height)
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Build filter node pins batch string using Slint-computed positions
/// Filter nodes have 3 pins: data-input (1), data-output (2), control-input (3)
///
/// NOTE: Slint also provides relative offset callbacks that could eliminate core constants.
fn build_filter_pins_batch(window: &MainWindow, filter_nodes: &VecModel<FilterNodeData>) -> String {
    (0..filter_nodes.row_count())
        .filter_map(|i| filter_nodes.row_data(i))
        .flat_map(|node| {
            // Data input pin (pin 1): left side, row 1
            let data_input_pin_id = node.id * 10 + 1;
            let data_input_rel_x = window.invoke_compute_filter_data_input_pin_relative_x();
            let data_input_rel_y = window.invoke_compute_filter_data_input_pin_relative_y();

            // Data output pin (pin 2): right side, row 1
            let data_output_pin_id = node.id * 10 + 2;
            let data_output_rel_x = window.invoke_compute_filter_data_output_pin_relative_x();
            let data_output_rel_y = window.invoke_compute_filter_data_output_pin_relative_y();

            // Control input pin (pin 3): left side, row 2
            let control_input_pin_id = node.id * 10 + 3;
            let control_input_rel_x = window.invoke_compute_filter_control_input_pin_relative_x();
            let control_input_rel_y = window.invoke_compute_filter_control_input_pin_relative_y();

            vec![
                format!("{},{},{}", data_input_pin_id, data_input_rel_x, data_input_rel_y),
                format!("{},{},{}", data_output_pin_id, data_output_rel_x, data_output_rel_y),
                format!("{},{},{}", control_input_pin_id, control_input_rel_x, control_input_rel_y),
            ]
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Build pin relative offsets batch string from Slint-computed offsets
/// Format: "pin_id,rel_x,rel_y;..." where rel_x/rel_y are unscaled offsets from node top-left
/// Pin IDs: node_id * 10 + 1 for input, node_id * 10 + 2 for output
///
/// The core computes absolute positions on-demand as: node_rect.pos + rel_offset * zoom
/// This eliminates hardcoded layout constants from the core.
fn build_pins_batch(window: &MainWindow, nodes: &VecModel<NodeData>) -> String {
    (0..nodes.row_count())
        .filter_map(|i| nodes.row_data(i))
        .flat_map(|node| {
            // Call Slint pure callbacks to get pin relative offsets
            let input_pin_id = node.id * 10 + 1;
            let input_rel_x = window.invoke_compute_input_pin_relative_x();
            let input_rel_y = window.invoke_compute_input_pin_relative_y();

            let output_pin_id = node.id * 10 + 2;
            let output_rel_x = window.invoke_compute_output_pin_relative_x();
            let output_rel_y = window.invoke_compute_output_pin_relative_y();

            vec![
                format!("{},{},{}", input_pin_id, input_rel_x, input_rel_y),
                format!("{},{},{}", output_pin_id, output_rel_x, output_rel_y),
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

    // Report initial node rects, pin positions, and link bezier paths
    // Positions computed by Slint using globals
    // Build combined node rects batch (simple nodes + filter nodes)
    // Positions computed by Slint using globals
    let simple_node_rects = build_node_rects_batch(&window, &nodes);
    let filter_node_rects = build_filter_node_rects_batch(&window, &filter_nodes);
    let node_rects_batch = if simple_node_rects.is_empty() {
        filter_node_rects
    } else if filter_node_rects.is_empty() {
        simple_node_rects
    } else {
        format!("{};{}", simple_node_rects, filter_node_rects)
    };
    window.set_pending_node_rects_batch(SharedString::from(node_rects_batch.as_str()));

    // Set initial bezier paths directly so links are visible on first render
    // (The overlay processes batches during render, which is too late for initial display)
    let bezier_paths = build_link_bezier_paths(&window, &nodes, &links);
    window.set_link_bezier_paths(SharedString::from(bezier_paths.as_str()));

    // Build combined pins batch (simple nodes + filter nodes)
    let simple_pins = build_pins_batch(&window, &nodes);
    let filter_pins = build_filter_pins_batch(&window, &filter_nodes);
    let pins_batch = if simple_pins.is_empty() {
        filter_pins
    } else if filter_pins.is_empty() {
        simple_pins
    } else {
        format!("{};{}", simple_pins, filter_pins)
    };
    window.set_pending_pins_batch(SharedString::from(pins_batch.as_str()));

    // Pure callback to check if a node is selected (queries core's selection state)
    // This is called by Slint during rendering to determine node highlight state
    let window_for_selection = window.as_weak();
    window.on_is_node_selected(move |node_id| {
        if let Some(window) = window_for_selection.upgrade() {
            let selected_ids_str = window.get_current_selected_ids();
            // Parse the comma-separated string and check if node_id is present
            selected_ids_str
                .split(',')
                .filter_map(|s| s.trim().parse::<i32>().ok())
                .any(|id| id == node_id)
        } else {
            false
        }
    });

    // Pure callback to check if a link is selected (queries core's selection state)
    // This is called by Slint during rendering to determine link highlight state
    let window_for_link_selection = window.as_weak();
    window.on_is_link_selected(move |link_id| {
        if let Some(window) = window_for_link_selection.upgrade() {
            let selected_ids_str = window.get_current_selected_link_ids();
            // Parse the comma-separated string and check if link_id is present
            selected_ids_str
                .split(',')
                .filter_map(|s| s.trim().parse::<i32>().ok())
                .any(|id| id == link_id)
        } else {
            false
        }
    });

    // Pure callback to get link path commands from core's link-bezier-paths
    // This is called during rendering to get bezier paths directly from core
    // Format of link-bezier-paths: "id|path_commands|color_argb;..."
    let window_for_path = window.as_weak();
    window.on_get_link_path_commands(move |link_id| {
        if let Some(window) = window_for_path.upgrade() {
            let bezier_paths_str = window.get_link_bezier_paths();
            for link_str in bezier_paths_str.split(';') {
                if link_str.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = link_str.split('|').collect();
                if parts.len() >= 2 {
                    if let Ok(id) = parts[0].parse::<i32>() {
                        if id == link_id {
                            return SharedString::from(parts[1]);
                        }
                    }
                }
            }
        }
        SharedString::default()
    });

    // Handle link position updates from core
    // This is called whenever the core regenerates link positions (viewport changes, node moves, etc.)
    // Now uses link-bezier-paths which contains core-generated SVG path commands
    let links_for_sync = links.clone();
    let window_for_sync = window.as_weak();
    window.on_link_positions_updated(move || {
        if let Some(window) = window_for_sync.upgrade() {
            // Parse core-generated bezier paths
            // Format: "id|path_commands|color_argb;..."
            let bezier_paths_str = window.get_link_bezier_paths();

            for link_str in bezier_paths_str.split(';') {
                if link_str.is_empty() {
                    continue;
                }

                // Split by '|' since path_commands contains spaces
                let parts: Vec<&str> = link_str.split('|').collect();
                if parts.len() >= 2 {
                    if let Ok(id) = parts[0].parse::<i32>() {
                        let path_commands = parts[1];

                        // Find and update the link in the model
                        for i in 0..links_for_sync.row_count() {
                            if let Some(mut link) = links_for_sync.row_data(i) {
                                if link.id == id {
                                    link.path_commands = SharedString::from(path_commands);
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
    // Positions computed by Slint using globals
    let nodes_for_viewport = nodes.clone();
    let filter_nodes_for_viewport = filter_nodes.clone();
    let window_for_viewport = window.as_weak();
    window.on_update_viewport(move |_zoom, _pan_x, _pan_y| {
        // Rebuild node rects and pin positions using Slint-computed values
        if let Some(window) = window_for_viewport.upgrade() {
            // Combine simple nodes and filter nodes
            let simple_rects = build_node_rects_batch(&window, &nodes_for_viewport);
            let filter_rects = build_filter_node_rects_batch(&window, &filter_nodes_for_viewport);
            let node_batch = if simple_rects.is_empty() {
                filter_rects
            } else if filter_rects.is_empty() {
                simple_rects
            } else {
                format!("{};{}", simple_rects, filter_rects)
            };
            window.set_pending_node_rects_batch(SharedString::from(node_batch.as_str()));

            let simple_pins = build_pins_batch(&window, &nodes_for_viewport);
            let filter_pins = build_filter_pins_batch(&window, &filter_nodes_for_viewport);
            let pins_batch = if simple_pins.is_empty() {
                filter_pins
            } else if filter_pins.is_empty() {
                simple_pins
            } else {
                format!("{};{}", simple_pins, filter_pins)
            };
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

        // Add link to model (path_commands will be computed by core)
        links_for_create.push(LinkData {
            id,
            start_pin_id: start_pin,
            end_pin_id: end_pin,
            color,
            path_commands: SharedString::default(), // Will be computed by core
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

        let selected_ids: std::collections::HashSet<i32> = window.get_current_selected_ids()
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

        // Update node rects and pin positions in core so link positions are recomputed
        // Positions computed by Slint using globals
        let simple_rects = build_node_rects_batch(&window, &nodes_for_drag);
        let filter_rects = build_filter_node_rects_batch(&window, &filter_nodes_for_drag);
        let node_batch = if simple_rects.is_empty() {
            filter_rects
        } else if filter_rects.is_empty() {
            simple_rects
        } else {
            format!("{};{}", simple_rects, filter_rects)
        };
        window.set_pending_node_rects_batch(SharedString::from(node_batch.as_str()));

        let simple_pins = build_pins_batch(&window, &nodes_for_drag);
        let filter_pins = build_filter_pins_batch(&window, &filter_nodes_for_drag);
        let pins_batch = if simple_pins.is_empty() {
            filter_pins
        } else if filter_pins.is_empty() {
            simple_pins
        } else {
            format!("{};{}", simple_pins, filter_pins)
        };
        window.set_pending_pins_batch(SharedString::from(pins_batch.as_str()));
    });

    // Handle deleting selected nodes
    let nodes_for_delete = nodes.clone();
    let links_for_delete = links.clone();
    let window_for_delete = window.as_weak();
    window.on_delete_selected_nodes(move || {
        let window = match window_for_delete.upgrade() {
            Some(w) => w,
            None => return,
        };

        // Get selected node IDs from overlay
        let selected_ids: std::collections::HashSet<i32> = window
            .get_current_selected_ids()
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

        // Also remove any links connected to deleted nodes
        // Pin IDs are node_id * 10 + pin_type, so we check if pin's node is deleted
        let mut link_indices_to_remove: Vec<usize> = Vec::new();
        let mut deleted_link_ids: Vec<i32> = Vec::new();
        for i in 0..links_for_delete.row_count() {
            if let Some(link) = links_for_delete.row_data(i) {
                let start_node_id = link.start_pin_id / 10;
                let end_node_id = link.end_pin_id / 10;
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

        // Report deleted link IDs to core so it can update its registry
        if !deleted_link_ids.is_empty() {
            let deleted_ids_str = deleted_link_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            window.set_pending_deleted_link_ids(SharedString::from(deleted_ids_str.as_str()));
        }
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
            .get_current_selected_link_ids()
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

        // Remove links in reverse order to maintain valid indices
        for &i in indices_to_remove.iter().rev() {
            links_for_link_delete.remove(i);
        }

        // Report deleted link IDs to core so it can update its registry
        let deleted_ids_str = ids_to_delete
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        window.set_pending_deleted_link_ids(SharedString::from(deleted_ids_str.as_str()));
    });

    // Handle adding new nodes (Ctrl+N)
    let nodes_for_add = nodes.clone();
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
