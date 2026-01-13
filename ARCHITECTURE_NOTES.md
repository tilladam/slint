# Node Editor Architecture Notes

## Known Limitations and Future Improvements

### Selection State: String vs Array Issue

**Current Implementation:**
- Selection state is stored as comma-separated strings: `"1,2,3"`
- Properties: `current-selected-ids: string`, `current-selected-link-ids: string`
- Core maintains proper data structures (`BTreeSet<i32>`) but converts to strings for Slint

**Why This Is Wrong:**
- Requires string parsing in every application
- Not type-safe
- Performance overhead from string operations
- Violates principle of least surprise

**Why We Can't Fix It (Yet):**

Slint's `Property<T>` system doesn't support `ModelRc<T>` as a bindable property type.

Attempting to change:
```rust
pub current_selected_ids: Property<SharedString>
```
to:
```rust
pub current_selected_ids: Property<ModelRc<i32>>
```

Fails with:
```
error[E0277]: the trait bound `ModelRc<i32>: From<Value>` is not satisfied
```

The SlintElement derive macro requires all property types to implement `From<Value>` and
`Into<Value>` for the property binding system. ModelRc doesn't implement these traits.

**Workaround in Library:**

We provide callback-based selection checking:
```slint
// Library (node_editor_lib.slint)
pure callback is-selected(node-id: int) -> bool;
pure callback is-link-selected(link-id: int) -> bool;
```

Applications implement these in Rust:
```rust
window.on_is_selected(move |node_id| {
    window.get_current_selected_ids()
        .split(',')
        .filter_map(|s| s.trim().parse::<i32>().ok())
        .any(|id| id == node_id)
});
```

**Future Solution:**

This requires changes to Slint's core framework:

1. **Option A**: Implement `From<Value>` / `Into<Value>` for `ModelRc<T>`
   - Might require serialization support in Value enum
   - Could have performance implications

2. **Option B**: Create a special property type for models
   - `ModelProperty<T>` that bypasses the Value conversion
   - Would need compiler support

3. **Option C**: Use a different mechanism for reactive model access
   - Perhaps a `model<T>` property modifier that generates accessors
   - Example: `out model<[int]> selected-ids` → generates getter methods

**Impact:**

This limitation affects any Slint application that needs to expose collections
reactively. Workarounds include:
- Using strings (current approach)
- Using callbacks to access data
- Managing collection state entirely in application layer

---

## Other Architecture Decisions

### Why Three Layers?

1. **Background** - Grid and link rendering (native, for performance)
2. **Children** - Node components (Slint, for flexibility)
3. **Overlay** - Input handling (native, for consistent behavior)

This separation allows:
- High-performance grid/link rendering
- Flexible node customization
- Consistent input behavior across applications

### Why Batch Reporting?

Pin and node positions are reported in batches (semicolon-separated strings) rather
than individual callbacks to minimize:
- Number of Slint ↔ Rust crossings
- Property update notifications
- Recomputation overhead

Format: `"id,x,y;id,x,y;..."`

This is 10-50x faster than individual callbacks in large graphs (100+ nodes).
