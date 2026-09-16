# Context Management

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Common Operations](#common-operations) · [Context Hierarchy](#context-hierarchy) · [cx.listener](#cxlistener--binding-callbacks-to-self) · [subscribe_in](#subscribe_in--subscribe-with-window-access) · [observe_window_activation](#observe_window_activation) · [observe_global](#observe_global) · [defer / defer_in](#defer-and-defer_in) · [Naming Convention](#context-naming-convention)

## Overview

GPUI uses different context types for different scenarios:

**Context Types:**
- **`App`**: Global app state, entity creation
- **`Window`**: Window-specific operations, painting, layout
- **`Context<T>`**: Entity-specific context for component `T`
- **`AsyncApp`**: Async context for foreground tasks
- **`AsyncWindowContext`**: Async context with window access

## Quick Start

### Context<T> - Component Context

```rust
impl MyComponent {
    fn update_state(&mut self, cx: &mut Context<Self>) {
        self.value = 42;
        cx.notify(); // Trigger re-render

        // Spawn async task
        cx.spawn(async move |cx| {
            // Async work
        }).detach();

        // Get current entity
        let entity = cx.entity();
    }
}
```

### App - Global Context

```rust
fn main() {
    let app = Application::new();
    app.run(|cx: &mut App| {
        // Create entities
        let entity = cx.new(|cx| MyState::default());

        // Open windows
        cx.open_window(WindowOptions::default(), |window, cx| {
            cx.new(|cx| Root::new(view, window, cx))
        });
    });
}
```

### Window - Window Context

```rust
impl Render for MyView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Window operations
        let is_focused = window.is_window_focused();
        let bounds = window.bounds();

        div().child("Content")
    }
}
```

### AsyncApp - Async Context

```rust
cx.spawn(async move |cx: &mut AsyncApp| {
    let data = fetch_data().await;

    entity.update(cx, |state, inner_cx| {
        state.data = data;
        inner_cx.notify();
    }).ok();
}).detach();
```

## Common Operations

### Entity Operations

```rust
// Create entity
let entity = cx.new(|cx| MyState::default());

// Update entity
entity.update(cx, |state, cx| {
    state.value = 42;
    cx.notify();
});

// Read entity
let value = entity.read(cx).value;
```

### Notifications and Events

```rust
// Trigger re-render
cx.notify();

// Emit event
cx.emit(MyEvent::Updated);

// Observe entity
cx.observe(&entity, |this, observed, cx| {
    // React to changes
}).detach();

// Subscribe to events
cx.subscribe(&entity, |this, source, event, cx| {
    // Handle event
}).detach();
```

### Window Operations

```rust
// Window state
let focused = window.is_window_focused();
let bounds = window.bounds();
let scale = window.scale_factor();

// Close window
window.remove_window();
```

### Async Operations

```rust
// Spawn foreground task
cx.spawn(async move |cx| {
    // Async work with entity access
}).detach();

// Spawn background task
cx.background_spawn(async move {
    // Heavy computation
}).detach();
```

## Context Hierarchy

```
App (Global)
  └─ Window (Per-window)
       └─ Context<T> (Per-component)
            └─ AsyncApp (In async tasks)
                 └─ AsyncWindowContext (Async + Window)
```

## cx.listener — Binding Callbacks to Self

`cx.listener` creates a callback that borrows `&mut self` (the current entity). Use it for `on_click`, `on_action`, and other element event handlers:

```rust
impl Render for MyView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .on_action(cx.listener(Self::on_save))
            .child(
                Button::new("btn")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.count += 1;
                        cx.notify();
                    }))
            )
    }
}

impl MyView {
    fn on_save(&mut self, _: &Save, _window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
    }
}
```

`cx.listener(Self::method)` is equivalent to creating a closure that calls `self.method(...)`.

## subscribe_in — Subscribe with Window Access

Use `subscribe_in` (instead of `subscribe`) when the callback needs `&mut Window`:

```rust
let _subscription = cx.subscribe_in(&input, window, |this, state, event, window, cx| {
    match event {
        InputEvent::Change => {
            let val = state.read(cx).value();
            this.on_input_change(val, window, cx);
        }
        _ => {}
    }
});
// Store _subscription in struct to keep it alive
```

`subscribe` vs `subscribe_in`:
- `subscribe(&entity, |this, source, event, cx|)` — no window access
- `subscribe_in(&entity, window, |this, source, event, window, cx|)` — has window access

## observe_window_activation

React when the window gains or loses focus:

```rust
let _sub = cx.observe_window_activation(window, |this, window, cx| {
    if window.is_window_active() {
        this.resume(cx);
    } else {
        this.pause(cx);
    }
});
```

## observe_global

React when a global value changes:

```rust
cx.observe_global::<Theme>(|cx| {
    // Theme changed — react
    cx.notify();
});
```

## defer and defer_in

Schedule work after the current update completes:

```rust
// defer: runs after current App update, no window access
cx.defer(|cx| {
    // Runs after current entity update is done
});

// defer_in: runs after update, with window access
cx.defer_in(window, |this, window, cx| {
    // Can access window here
    // CAUTION: never call entity.update(cx) on *this same entity* inside defer_in
    // — it re-enters the lock and panics. Use the &mut self reference directly.
    this.some_method(window, cx);
});
```

## Context Naming Convention

Always name contexts `cx` regardless of type:

```rust
fn new(window: &mut Window, cx: &mut App) {}             // cx = App
impl Render for View {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) {}  // cx = Context<Self>
}
cx.spawn(async move |this, cx: &mut AsyncApp| {})         // cx = AsyncApp
```
