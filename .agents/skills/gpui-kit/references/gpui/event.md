<!-- OpenLogi modification: normalized trailing whitespace and final newlines. -->

# Events & Subscriptions

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Common Patterns](#common-patterns) · [subscribe_in](#subscribe_in--subscription-with-window-access) · [observe_window_activation](#observe_window_activation) · [observe_global](#observe_global) · [Subscription Lifetime](#subscription-lifetime) · [Best Practices](#best-practices)

## Overview

GPUI provides event system for component coordination:

**Event Mechanisms:**
- **Custom Events**: Define and emit type-safe events
- **Observations**: React to entity state changes
- **Subscriptions**: Listen to events from other entities
- **Global Events**: App-wide event handling

## Quick Start

### Define and Emit Events

```rust
#[derive(Clone)]
enum MyEvent {
    DataUpdated(String),
    ActionTriggered,
}

impl MyComponent {
    fn update_data(&mut self, data: String, cx: &mut Context<Self>) {
        self.data = data.clone();

        // Emit event
        cx.emit(MyEvent::DataUpdated(data));
        cx.notify();
    }
}
```

### Subscribe to Events

```rust
impl Listener {
    fn new(source: Entity<MyComponent>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            // Subscribe to events
            cx.subscribe(&source, |this, emitter, event: &MyEvent, cx| {
                match event {
                    MyEvent::DataUpdated(data) => {
                        this.handle_update(data.clone(), cx);
                    }
                    MyEvent::ActionTriggered => {
                        this.handle_action(cx);
                    }
                }
            }).detach();

            Self { source }
        })
    }
}
```

### Observe Entity Changes

```rust
impl Observer {
    fn new(target: Entity<Target>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            // Observe entity for any changes
            cx.observe(&target, |this, observed, cx| {
                // Called when observed.update() calls cx.notify()
                println!("Target changed");
                cx.notify();
            }).detach();

            Self { target }
        })
    }
}
```

## Common Patterns

### 1. Parent-Child Communication

```rust
// Parent emits events
impl Parent {
    fn notify_children(&mut self, cx: &mut Context<Self>) {
        cx.emit(ParentEvent::Updated);
        cx.notify();
    }
}

// Children subscribe
impl Child {
    fn new(parent: Entity<Parent>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            cx.subscribe(&parent, |this, parent, event, cx| {
                this.handle_parent_event(event, cx);
            }).detach();

            Self { parent }
        })
    }
}
```

### 2. Global Event Broadcasting

```rust
struct EventBus {
    listeners: Vec<WeakEntity<dyn Listener>>,
}

impl EventBus {
    fn broadcast(&mut self, event: GlobalEvent, cx: &mut Context<Self>) {
        self.listeners.retain(|weak| {
            weak.update(cx, |listener, cx| {
                listener.on_event(&event, cx);
            }).is_ok()
        });
    }
}
```

### 3. Observer Pattern

```rust
cx.observe(&entity, |this, observed, cx| {
    // React to any state change
    let state = observed.read(cx);
    this.sync_with_state(state, cx);
}).detach();
```

## subscribe_in — Subscription with Window Access

Use when the subscription callback needs `&mut Window`:

```rust
// Store subscriptions to keep them alive
struct MyComponent {
    _subscriptions: Vec<Subscription>,
}

impl MyComponent {
    fn new(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _subscriptions = vec![
            cx.subscribe_in(input, window, |this, state, event, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => this.on_submit(window, cx),
                    InputEvent::Change => {
                        let val = state.read(cx).value();
                        this.on_change(val, cx);
                    }
                    _ => {}
                }
            }),
        ];
        Self { _subscriptions }
    }
}
```

`subscribe` vs `subscribe_in`:
- `cx.subscribe(&entity, |this, source, event, cx|)` — no window
- `cx.subscribe_in(&entity, window, |this, source, event, window, cx|)` — window access

## observe_window_activation

```rust
let _sub = cx.observe_window_activation(window, |this, window, cx| {
    if window.is_window_active() {
        this.start_polling(cx);
    } else {
        this.stop_polling(cx);
    }
});
```

## observe_global

```rust
cx.observe_global::<Theme>(|cx| {
    cx.notify(); // Re-render when theme changes
});
```

## Subscription Lifetime

Subscriptions are cancelled when dropped. Two ways to keep alive:

```rust
// 1. .detach() — lives until entity is dropped
cx.subscribe(&entity, |this, _, event, cx| {
    // ...
}).detach();

// 2. Store in struct — cancelled when struct drops
struct MyView {
    _subscriptions: Vec<Subscription>,
}
// _subscriptions.push(cx.subscribe(...));
```

Use `.detach()` for permanent subscriptions; store in struct for subscriptions that should stop when the component unmounts.

## Best Practices

### ✅ Detach Subscriptions

```rust
// ✅ Detach to keep alive
cx.subscribe(&entity, |this, source, event, cx| {
    // Handle event
}).detach();
```

### ✅ Clean Event Types

```rust
#[derive(Clone)]
enum AppEvent {
    DataChanged { id: usize, value: String },
    ActionPerformed(ActionType),
    Error(String),
}
```

### ❌ Avoid Event Loops

```rust
// ❌ Don't create mutual subscriptions
entity1.subscribe(entity2) → emits event
entity2.subscribe(entity1) → emits event → infinite loop!
```

## Reference Documentation
