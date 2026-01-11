# Testing Expansion Plan: Node Editor Hybrid Architecture

This document outlines the required test cases to verify the generic node editor's core logic, focusing on the hybrid architecture and registry systems described in `IMPLEMENTATION_PLAN.md`.

## Phase 1: Registry & Reporting (Pin/Node Positions)

The "UI-reports-to-Core" strategy is critical for dynamic layouts. We must verify that reporting properties correctly update the internal maps.

### 1.1 Pin Position Registration
- **Test:** Set `reporting_pin_id`, `reporting_pin_x`, and `reporting_pin_y` on `NodeEditorOverlay`.
- **Verify:** Internal `pin_positions` map in `NodeEditorState` is updated.
- **Verify:** `reporting_pin_id` is reset to `0` by core after processing (acknowledgment).

### 1.2 Node Rectangle Registration
- **Test:** Set `reporting_node_id`, `reporting_node_x`, `reporting_node_y`, `reporting_node_width`, and `reporting_node_height`.
- **Verify:** Internal `node_rects` map is updated with the correct dimensions.
- **Verify:** `reporting_node_id` is reset to `0`.

---

## Phase 2: Link Creation State Machine

Verify the transition between idle, dragging, and completion/cancellation.

### 2.1 Triggering Link Creation
- **Test:** Set `pending_link_pin_id` and `pending_link_x/y`.
- **Verify:** `is_creating_link` becomes true, and `link_start_pin_id` matches the input.
- **Verify:** `pending_link_pin_id` is reset to `0`.

### 2.2 Link Completion (Valid Target)
- **Test:** Initiate link creation -> set `target_pin_id` -> set `complete_link_creation = true`.
- **Verify:** `link_created` callback fires.
- **Verify:** `created_link_start_pin` and `created_link_end_pin` properties are populated correctly.
- **Verify:** `is_creating_link` becomes false.

### 2.3 Link Dropped (Empty Space)
- **Test:** Initiate link creation -> set `target_pin_id = 0` -> set `complete_link_creation = true`.
- **Verify:** `link_dropped` callback fires.
- **Verify:** `is_creating_link` becomes false.

### 2.4 Cancellation via Escape
- **Test:** Initiate link creation -> Dispatch `Escape` key event.
- **Verify:** `link_cancelled` callback fires.
- **Verify:** `is_creating_link` becomes false.

---

## Phase 3: Selection & Intersection Logic

The core should handle the math for selection box intersections using the node registry.

### 3.1 Box Selection Intersections
- **Test:** Register 3 nodes at specific coordinates -> Perform a Ctrl+drag selection box that covers 2 of them.
- **Verify:** `selected_node_ids_str` contains the comma-separated IDs of the covered nodes (e.g., `"1,2"`).
- **Verify:** `selection_changed` callback fires.

### 3.2 Native Selection Management
- **Test:** Use `node_clicked` callback mechanism with `clicked_node_id` and `clicked_shift_held`.
- **Verify:** `current_selected_ids` (SharedString) updates correctly for single select and multi-select (Shift).

---

## Phase 4: Native Grid Generation

Verify the "Rust SVG -> Slint Path" bridge in `NodeEditorBackground`.

### 4.1 Grid Command Reactivity
- **Test:** Change `pan_x`, `pan_y`, or `zoom` on `NodeEditorBackground`.
- **Verify:** `grid_commands` string property changes.
- **Verify:** `grid_commands` is non-empty only when `grid_spacing > 0`.

### 4.2 Infinite Grid Effect
- **Test:** Set `pan_x` to a multiple of `grid_spacing * zoom`.
- **Verify:** The generated path commands are identical to `pan_x = 0` (verifying `rem_euclid` logic).

---

## Phase 5: Global Shortcuts

### 5.1 Keyboard Events
- **Test:** Focus overlay -> Dispatch `Delete` or `Backspace`.
- **Verify:** `delete_selected` callback fires.
- **Test:** Focus overlay -> Dispatch `Ctrl+N`.
- **Verify:** `add_node_requested` callback fires.

## Implementation Notes for Tests

Since many core properties are "trigger-based" (set property -> core reacts -> core resets property), Slint tests should use `init` or `Timer` logic to simulate reporting, or drive them via the Rust `slint-testing` crate to inspect internal `RefCell` state where possible.
