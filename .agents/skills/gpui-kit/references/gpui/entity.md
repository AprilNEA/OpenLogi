<!-- OpenLogi modification: normalized trailing whitespace and final newlines. -->

# Entity State Management

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Core Principles](#core-principles) · [Common Use Cases](#common-use-cases) · [Extended References](#reference-documentation)

## Overview

An `Entity<T>` is a handle to state of type `T`, providing safe access and updates.

**Key Methods:**
- `entity.read(cx)` → `&T` - Read-only access
- `entity.read_with(cx, |state, cx| ...)` → `R` - Read with closure
- `entity.update(cx, |state, cx| ...)` → `R` - Mutable update
- `entity.downgrade()` → `WeakEntity<T>` - Create weak reference
- `entity.entity_id()` → `EntityId` - Unique identifier

**Entity Types:**
- **`Entity<T>`**: Strong reference (increases ref count)
- **`WeakEntity<T>`**: Weak reference (doesn't prevent cleanup, returns `Result`)

## Quick Start

### Creating and Using Entities

```rust
// Create entity
let counter = cx.new(|cx| Counter { count: 0 });

// Read state
let count = counter.read(cx).count;

// Update state
counter.update(cx, |state, cx| {
    state.count += 1;
    cx.notify(); // Trigger re-render
});

// Weak reference (for closures/callbacks)
let weak = counter.downgrade();
let _ = weak.update(cx, |state, cx| {
    state.count += 1;
    cx.notify();
});
```

### In Components

```rust
struct MyComponent {
    shared_state: Entity<SharedData>,
}

impl MyComponent {
    fn new(cx: &mut App) -> Entity<Self> {
        let shared = cx.new(|_| SharedData::default());

        cx.new(|cx| Self {
            shared_state: shared,
        })
    }

    fn update_shared(&mut self, cx: &mut Context<Self>) {
        self.shared_state.update(cx, |state, cx| {
            state.value = 42;
            cx.notify();
        });
    }
}
```

### Async Operations

When calling `cx.spawn` from `Context<Self>`, the closure receives `(WeakEntity<Self>, &mut AsyncApp)`:

```rust
impl MyComponent {
    fn fetch_data(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let data = fetch_from_api().await;

            // Update entity safely via the weak reference
            let _ = this.update(cx, |state, cx| {
                state.data = Some(data);
                cx.notify();
            });
        }).detach();
    }
}
```

## Core Principles

### Always Use Weak References in Closures

```rust
// ✅ Good: Weak reference prevents retain cycles
let weak = cx.entity().downgrade();
callback(move || {
    let _ = weak.update(cx, |state, cx| cx.notify());
});

// ❌ Bad: Strong reference may cause memory leak
let strong = cx.entity();
callback(move || {
    strong.update(cx, |state, cx| cx.notify());
});
```

### Use Inner Context

```rust
// ✅ Good: Use inner cx from closure
entity.update(cx, |state, inner_cx| {
    inner_cx.notify(); // Correct
});

// ❌ Bad: Use outer cx (multiple borrow error)
entity.update(cx, |state, inner_cx| {
    cx.notify(); // Wrong!
});
```

### Avoid Nested Entity Updates

Nested `entity.update(cx, …)` calls are dangerous. The default posture is: **do not nest them**. The sub-cases below clarify when a panic is guaranteed vs. merely possible.

**Same entity → always panics.**
GPUI locks an entity for the entire duration of its update or render pass. Re-entering that same lock panics immediately:

```
cannot update … while it is already being updated
```

```rust
// ❌ Panic: updating entity_a from inside entity_a's own update
entity_a.update(cx, |state, cx| {
    entity_a.update(cx, |_, _| {}); // PANIC — same lock
});
```

**Different entity → generally safe, but indirect cycles still panic.**
Each entity has its own lock, so updating `entity_b` from within `entity_a`'s update normally succeeds. However, if `entity_b`'s callback reaches back into `entity_a` — directly or through a chain — GPUI will attempt to re-acquire `entity_a`'s lock and panic.

```rust
// ✅ Usually fine: different entities, no cycle
entity_a.update(cx, |_, cx| {
    entity_b.update(cx, |_, _| {}); // OK — different lock
});

// ❌ Panic: indirect cycle back to entity_a
entity_a.update(cx, |_, cx| {
    entity_b.update(cx, |_, cx| {
        entity_a.update(cx, |_, _| {}); // PANIC — entity_a is still locked
    });
});
```

When in doubt, flatten the call sequence rather than nesting: finish the outer update, then update the second entity from outside.

**`defer_in` does not bypass the lock.** `cx.defer_in(window, callback)` schedules `callback` to run on the current entity — meaning GPUI re-acquires the entity's lock to execute it. The re-entrancy rules apply equally inside the deferred callback:

```rust
// ❌ Panic: defer_in re-locks entity_a; calling entity_a.update inside re-enters
impl SomeDelegate for MyAdapter {
    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        cx.defer_in(window, |list_state, window, cx| {
            // list_state is locked for this callback!
            parent.update(cx, |this, cx| {
                this.list.update(cx, |_, _| {}); // PANIC — list is already locked above
            });
        });
    }
}

// ✅ Fix: use the direct &mut reference the callback provides
impl SomeDelegate for MyAdapter {
    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        cx.defer_in(window, |list_state, window, cx| {
            // Access list data directly — no entity lock needed
            list_state.delegate_mut().some_hook();

            // Update the *parent* entity — different lock, safe
            parent.update(cx, |this, cx| { /* … */ });

            // Sync list state directly after parent update
            list_state.delegate_mut().update_snapshot(new_val);
        });
    }
}
```

**Snapshot pattern for render callbacks.** `render_item` (and any other rendering hook) runs inside the entity's render pass. It must never call `entity.read(cx)` or `entity.update(cx, …)` on any external entity. Instead, keep a plain `snapshot` field updated eagerly from *outside* render after every mutation:

```rust
// ❌ Panic in render_item — ListState is already locked
fn render_item(&mut self, ix: IndexPath, window: &mut Window, cx: &mut Context<ListState<Self>>) -> … {
    let checked = parent_entity.read(cx).selection.contains(&ix); // PANIC
}

// ✅ Read from a plain snapshot field — no entity access
fn render_item(&mut self, ix: IndexPath, window: &mut Window, cx: &mut Context<ListState<Self>>) -> … {
    let checked = self.selection_snapshot.iter().any(|(sel_ix, _)| sel_ix == &ix);
}
```

## Common Use Cases

1. **Component State**: Internal state that needs reactivity
2. **Shared State**: State shared between multiple components
3. **Parent-Child**: Coordinating between related components (use weak refs)
4. **Async State**: Managing state that changes from async operations
5. **Observations**: Reacting to changes in other entities

## Reference Documentation

- **API**: See [entity-api.md](entity-api.md) — entity types, methods, lifecycle, error handling
- **Patterns**: See [entity-patterns.md](entity-patterns.md) — model-view, cross-entity communication, observer
- **Best Practices**: See [entity-best-practices.md](entity-best-practices.md) — pitfalls, memory, performance, async
- **Advanced**: See [entity-advanced.md](entity-advanced.md) — collections, registry, debounce, state machines
