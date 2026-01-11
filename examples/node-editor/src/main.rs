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

/// Compute screen position for a pin given node position, zoom, and pan
fn compute_pin_position(
    pin_id: i32,
    nodes: &Rc<VecModel<NodeData>>,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
) -> (f32, f32) {
    let node_id = pin_id / 10;
    let pin_type = pin_id % 10; // 1 = input, 2 = output

    // Pin dimensions scaled by zoom (must match ui.slint)
    let pin_size = BASE_PIN_SIZE * zoom;
    let pin_radius = pin_size / 2.0;

    // Find the node
    let mut world_x = 0.0;
    let mut world_y = 0.0;
    for i in 0..nodes.row_count() {
        if let Some(node) = nodes.row_data(i) {
            if node.id == node_id {
                world_x = node.world_x;
                world_y = node.world_y;
                break;
            }
        }
    }

    // Compute screen position based on pin type
    let (x, y) = if pin_type == 1 {
        // Input pin: left side
        let x = world_x * zoom + pan_x + 8.0 * zoom + pin_radius;
        let y = world_y * zoom + pan_y + PIN_Y_OFFSET * zoom + pin_radius;
        (x, y)
    } else {
        // Output pin: right side
        let x = world_x * zoom + pan_x + NODE_BASE_WIDTH * zoom - 8.0 * zoom - pin_size + pin_radius;
        let y = world_y * zoom + pan_y + PIN_Y_OFFSET * zoom + pin_radius;
        (x, y)
    };

    (x, y)
}

/// Update positions for all links in the model
fn update_all_link_positions(
    links: &Rc<VecModel<LinkData>>,
    nodes: &Rc<VecModel<NodeData>>,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
) {
    for i in 0..links.row_count() {
        if let Some(mut link) = links.row_data(i) {
            let (start_x, start_y) = compute_pin_position(link.start_pin_id, nodes, zoom, pan_x, pan_y);
            let (end_x, end_y) = compute_pin_position(link.end_pin_id, nodes, zoom, pan_x, pan_y);
            link.start_x = start_x;
            link.start_y = start_y;
            link.end_x = end_x;
            link.end_y = end_y;
            links.set_row_data(i, link);
        }
    }
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

    // Compute initial link positions (zoom=1.0, pan=0,0)
    update_all_link_positions(&links, &nodes, 1.0, 0.0, 0.0);

    // Handle link position updates when viewport changes
    // (Grid rendering is now handled internally by NodeEditorBackground)
    let links_for_viewport = links.clone();
    let nodes_for_viewport = nodes.clone();
    window.on_update_viewport(move |zoom, pan_x, pan_y| {
        update_all_link_positions(&links_for_viewport, &nodes_for_viewport, zoom, pan_x, pan_y);
    });

    // Handle link creation
    let links_for_create = links.clone();
    let nodes_for_create = nodes.clone();
    let next_link_id_for_create = next_link_id.clone();
    let color_index_for_create = color_index.clone();
    let window_for_create = window.as_weak();
    window.on_create_link(move |start_pin, end_pin| {
        let id = *next_link_id_for_create.borrow();
        *next_link_id_for_create.borrow_mut() += 1;

        let idx = *color_index_for_create.borrow();
        *color_index_for_create.borrow_mut() = (idx + 1) % link_colors.len();

        // Get current viewport values
        let (zoom, pan_x, pan_y) = if let Some(window) = window_for_create.upgrade() {
            (window.get_zoom(), window.get_pan_x() / 1.0, window.get_pan_y() / 1.0)
        } else {
            (1.0, 0.0, 0.0)
        };

        // Compute positions for the new link
        let (start_x, start_y) = compute_pin_position(start_pin, &nodes_for_create, zoom, pan_x, pan_y);
        let (end_x, end_y) = compute_pin_position(end_pin, &nodes_for_create, zoom, pan_x, pan_y);

        links_for_create.push(LinkData {
            id,
            start_pin_id: start_pin,
            end_pin_id: end_pin,
            color: link_colors[idx],
            start_x, start_y, end_x, end_y,
        });
    });

    // Handle node selection
    let nodes_for_select = nodes.clone();
    window.on_select_node(move |node_id, shift_held| {
        for i in 0..nodes_for_select.row_count() {
            if let Some(mut node) = nodes_for_select.row_data(i) {
                if shift_held {
                    // Shift+click: toggle only the clicked node
                    if node.id == node_id {
                        node.selected = !node.selected;
                        nodes_for_select.set_row_data(i, node);
                    }
                } else {
                    // Normal click: select only clicked, deselect others
                    let should_select = node.id == node_id;
                    if node.selected != should_select {
                        node.selected = should_select;
                        nodes_for_select.set_row_data(i, node);
                    }
                }
            }
        }
    });

    // Handle drag commit - apply delta to all selected nodes when drag ends
    let nodes_for_drag = nodes.clone();
    let links_for_drag = links.clone();
    let window_for_drag = window.as_weak();
    window.on_commit_drag(move |delta_x, delta_y, snap_enabled| {
        // Apply delta to all selected nodes, optionally snapping to grid
        for i in 0..nodes_for_drag.row_count() {
            if let Some(mut node) = nodes_for_drag.row_data(i) {
                if node.selected {
                    let new_x = node.world_x + delta_x;
                    let new_y = node.world_y + delta_y;
                    node.world_x = if snap_enabled { snap_to_grid(new_x) } else { new_x };
                    node.world_y = if snap_enabled { snap_to_grid(new_y) } else { new_y };
                    nodes_for_drag.set_row_data(i, node);
                }
            }
        }

        // Update link positions after nodes have moved
        if let Some(window) = window_for_drag.upgrade() {
            let zoom = window.get_zoom();
            let pan_x = window.get_pan_x() / 1.0;
            let pan_y = window.get_pan_y() / 1.0;
            update_all_link_positions(&links_for_drag, &nodes_for_drag, zoom, pan_x, pan_y);
        }
    });

    // Handle deleting selected nodes
    let nodes_for_delete = nodes.clone();
    let links_for_delete = links.clone();
    window.on_delete_selected_nodes(move || {
        // Collect indices of selected nodes (in reverse order for safe removal)
        let mut indices_to_remove: Vec<usize> = Vec::new();
        let mut deleted_node_ids: Vec<i32> = Vec::new();

        for i in 0..nodes_for_delete.row_count() {
            if let Some(node) = nodes_for_delete.row_data(i) {
                if node.selected {
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

    // Handle box selection
    let nodes_for_box = nodes.clone();
    window.on_box_select(move |sel_x, sel_y, sel_width, sel_height, zoom, pan_x, pan_y| {
        // Node dimensions (must match ui.slint)
        let node_base_width = 150.0 * zoom;
        let node_base_height = 80.0 * zoom;

        // Convert selection box to f32 for comparison
        let sel_x = sel_x as f32;
        let sel_y = sel_y as f32;
        let sel_width = sel_width as f32;
        let sel_height = sel_height as f32;
        let pan_x = pan_x as f32;
        let pan_y = pan_y as f32;

        for i in 0..nodes_for_box.row_count() {
            if let Some(mut node) = nodes_for_box.row_data(i) {
                // Compute node screen position
                let node_screen_x = node.world_x * zoom + pan_x;
                let node_screen_y = node.world_y * zoom + pan_y;

                // Check if node intersects with selection box
                let intersects = node_screen_x < sel_x + sel_width
                    && node_screen_x + node_base_width > sel_x
                    && node_screen_y < sel_y + sel_height
                    && node_screen_y + node_base_height > sel_y;

                if node.selected != intersects {
                    node.selected = intersects;
                    nodes_for_box.set_row_data(i, node);
                }
            }
        }
    });

    window.run().unwrap();
}
