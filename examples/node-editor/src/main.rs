// Node Editor Example
//
// Demonstrates the NodeEditorBackground and NodeEditorOverlay components
// for building visual node graph editors.

slint::include_modules!();

fn main() {
    let window = MainWindow::new().unwrap();
    window.run().unwrap();
}
