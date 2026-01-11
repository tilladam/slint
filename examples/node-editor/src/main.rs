// Node Editor Example
//
// Demonstrates the NodeEditorBackground and NodeEditorOverlay components
// for building visual node graph editors.

use slint::{Model, ModelRc, SharedString, VecModel};
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
