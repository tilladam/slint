// Node Editor Example
//
// Demonstrates the NodeEditorBackground and NodeEditorOverlay components
// for building visual node graph editors.

use slint::{Color, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

slint::include_modules!();

/// Generate SVG path commands for the grid lines
fn generate_grid_commands(width: f32, height: f32, zoom: f32, pan_x: f32, pan_y: f32) -> String {
    let grid_spacing = 24.0;
    let effective_spacing = grid_spacing * zoom;

    // Skip if spacing is too small
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
        commands.push_str(&format!("M {} 0 L {} {}", x, x, height));
        x += effective_spacing;
    }

    // Generate horizontal lines
    let mut y = offset_y;
    while y < height + effective_spacing {
        commands.push(' ');
        commands.push_str(&format!("M 0 {} L {} {}", y, width, y));
        y += effective_spacing;
    }

    commands
}

fn main() {
    let window = MainWindow::new().unwrap();

    // Set initial grid
    let initial_grid = generate_grid_commands(1200.0, 800.0, 1.0, 0.0, 0.0);
    window.set_grid_commands(SharedString::from(&initial_grid));

    // Handle grid update requests
    let window_weak = window.as_weak();
    window.on_update_grid(move |width, height, zoom, pan_x, pan_y| {
        if let Some(window) = window_weak.upgrade() {
            let commands = generate_grid_commands(width, height, zoom, pan_x, pan_y);
            window.set_grid_commands(SharedString::from(&commands));
        }
    });

    // Create the node model
    let nodes: Rc<VecModel<NodeData>> = Rc::new(VecModel::from(vec![
        NodeData {
            id: 1,
            title: SharedString::from("Input"),
            world_x: 100.0,
            world_y: 200.0,
            selected: false,
        },
        NodeData {
            id: 2,
            title: SharedString::from("Process"),
            world_x: 350.0,
            world_y: 150.0,
            selected: false,
        },
        NodeData {
            id: 3,
            title: SharedString::from("Output"),
            world_x: 600.0,
            world_y: 200.0,
            selected: false,
        },
    ]));

    // Set the model on the window
    window.set_nodes(ModelRc::from(nodes.clone()));

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
        },
        // Process (pin 22) -> Output (pin 31)
        LinkData {
            id: 2,
            start_pin_id: 22,
            end_pin_id: 31,
            color: link_colors[1],
        },
    ]));
    window.set_links(ModelRc::from(links.clone()));

    // Handle link creation
    let links_for_create = links.clone();
    let next_link_id_for_create = next_link_id.clone();
    let color_index_for_create = color_index.clone();
    window.on_create_link(move |start_pin, end_pin| {
        let id = *next_link_id_for_create.borrow();
        *next_link_id_for_create.borrow_mut() += 1;

        let idx = *color_index_for_create.borrow();
        *color_index_for_create.borrow_mut() = (idx + 1) % link_colors.len();

        links_for_create.push(LinkData {
            id,
            start_pin_id: start_pin,
            end_pin_id: end_pin,
            color: link_colors[idx],
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
    window.on_commit_drag(move |delta_x, delta_y| {
        // Apply delta to all selected nodes
        for i in 0..nodes_for_drag.row_count() {
            if let Some(mut node) = nodes_for_drag.row_data(i) {
                if node.selected {
                    node.world_x += delta_x;
                    node.world_y += delta_y;
                    nodes_for_drag.set_row_data(i, node);
                }
            }
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
