# Entity API Reference

**Contents:** [Entity Types](#entity-types) · [Entity Creation](#entity-creation) · [Entity Operations](#entity-operations) · [Context Methods](#context-methods-for-entities) · [Async Operations](#async-operations) · [Entity Lifecycle](#entity-lifecycle) · [EntityId](#entityid) · [Error Handling](#error-handling) · [Type Conversions](#type-conversions)

## Entity Types

### Entity<T>

A strong reference to state of type `T`.

**Methods:**
- `entity_id()` → `EntityId` - Returns unique identifier
- `downgrade()` → `WeakEntity<T>` - Creates weak reference
- `read(cx)` → `&T` - Immutable access to state
- `read_with(cx, |state, cx| ...)` → `R` - Read with closure, returns closure result
- `update(cx, |state, cx| ...)` → `R` - Mutable update with `Context<T>`, returns closure result
- `update_in(cx, |state, window, cx| ...)` → `R` - Update with `Window` access (requires `AsyncWindowContext` or `VisualTestContext`)

**Important Notes:**
- Trying to update an entity while it's already being updated will panic
- Within closures, use the inner `cx` provided to avoid multiple borrow issues
- With async contexts, return values are wrapped in `anyhow::Result`

### WeakEntity<T>

A weak reference to state of type `T`.

**Methods:**
- `upgrade()` → `Option<Entity<T>>` - Convert to strong reference if still alive
- `read_with(cx, |state, cx| ...)` → `Result<R>` - Read if entity exists
- `update(cx, |state, cx| ...)` → `Result<R>` - Update if entity exists
- `update_in(cx, |state, window, cx| ...)` → `Result<R>` - Update with window if entity exists

**Use Cases:**
- Avoid circular dependencies between entities
- Store references in closures/callbacks without preventing cleanup
- Optional relationships between components

**Important:** All operations return `Result` since the entity may no longer exist.

### AnyEntity

Dynamically-typed entity handle for storing entities of different types.

### AnyWeakEntity

Dynamically-typed weak entity handle.

## Entity Creation

### cx.new()

Create new entity with initial state.

```rust
let entity = cx.new(|cx| MyState {
    count: 0,
    name: "Default".to_string(),
});
```

**Parameters:**
- `cx: &mut App` or other context type
- Closure receiving `&mut Context<T>` returning initial state `T`

**Returns:** `Entity<T>`

## Entity Operations

### Reading State

#### read()

Direct read-only access to state.

```rust
let count = my_entity.read(cx).count;
```

**Use when:** Simple field access, no context operations needed.

#### read_with()

Read with context access in closure.

```rust
let count = my_entity.read_with(cx, |state, cx| {
    // Can access both state and context
    state.count
});

// Return multiple values
let (count, theme) = my_entity.read_with(cx, |state, cx| {
    (state.count, cx.theme().clone())
});
```

**Use when:** Need context operations, multiple return values, complex logic.

### Updating State

#### update()

Mutable update with `Context<T>`.

```rust
my_entity.update(cx, |state, cx| {
    state.count += 1;
    cx.notify(); // Trigger re-render
});
```

**Available Operations:**
- `cx.notify()` - Trigger re-render
- `cx.entity()` - Get current entity
- `cx.emit(event)` - Emit event
- `cx.spawn(task)` - Spawn async task
- Other `Context<T>` methods

#### update_in()

Update with both `Window` and `Context<T>` access.

```rust
my_entity.update_in(cx, |state, window, cx| {
    state.focused = window.is_window_focused();
    cx.notify();
});
```

**Requires:** `AsyncWindowContext` or `VisualTestContext`

**Use when:** Need window-specific operations like focus state, window bounds, etc.

## Context Methods for Entities

### cx.entity()

Get current entity being updated.

```rust
impl MyComponent {
    fn some_method(&mut self, cx: &mut Context<Self>) {
        let current_entity = cx.entity();  // Entity<MyComponent>
        let weak = current_entity.downgrade();
    }
}
```

### cx.observe()

Observe entity for changes.

```rust
cx.observe(&entity, |this, observed_entity, cx| {
    // Called when observed_entity.update() calls cx.notify()
    println!("Entity changed");
}).detach();
```

**Returns:** `Subscription` - Call `.detach()` to make permanent

### cx.subscribe()

Subscribe to events from entity.

```rust
cx.subscribe(&entity, |this, emitter, event: &SomeEvent, cx| {
    // Called when emitter emits SomeEvent
    match event {
        SomeEvent::DataChanged => {
            cx.notify();
        }
    }
}).detach();
```

**Returns:** `Subscription` - Call `.detach()` to make permanent

### cx.observe_new_entities()

Register callback for new entities of a type.

```rust
cx.observe_new_entities::<MyState>(|entity, cx| {
    println!("New entity created: {:?}", entity.entity_id());
}).detach();
```

## Async Operations

### cx.spawn()

Spawn foreground task (UI thread).

```rust
cx.spawn(async move |this, cx| {
    // `this`: WeakEntity<T>
    // `cx`: &mut AsyncApp

    let result = some_async_work().await;

    // Update entity safely
    let _ = this.update(cx, |state, cx| {
        state.data = result;
        cx.notify();
    });
}).detach();
```

**Note:** Always use weak entity reference in spawned tasks to prevent retain cycles.

### cx.background_spawn()

Spawn background task (background thread).

```rust
cx.background_spawn(async move {
    // Long-running computation
    let result = heavy_computation().await;
    // Cannot directly update entities here
    // Use channels or spawn foreground task to update
}).detach();
```

## Entity Lifecycle

### Creation

Entities are created via `cx.new()` and immediately registered in the app.

### Reference Counting

- `Entity<T>` is a strong reference (increases reference count)
- `WeakEntity<T>` is a weak reference (does not increase reference count)
- Cloning `Entity<T>` increases reference count

### Disposal

Entities are automatically disposed when all strong references are dropped.

```rust
{
    let entity = cx.new(|cx| MyState::default());
    // entity exists
} // entity dropped here if no other strong references exist
```

**Memory Leak Prevention:**
- Use `WeakEntity` in closures/callbacks
- Use `WeakEntity` for parent-child relationships
- Avoid circular strong references

## EntityId

Every entity has a unique identifier.

```rust
let id: EntityId = entity.entity_id();

// EntityIds can be compared
if entity1.entity_id() == entity2.entity_id() {
    // Same entity
}
```

**Use Cases:**
- Debugging and logging
- Entity comparison without borrowing
- Hash maps keyed by entity

## Error Handling

### WeakEntity Operations

All `WeakEntity` operations return `Result`:

```rust
let weak = entity.downgrade();

// Handle potential failure
match weak.read_with(cx, |state, cx| state.count) {
    Ok(count) => println!("Count: {}", count),
    Err(e) => eprintln!("Entity no longer exists: {}", e),
}

// Or use Result combinators
let _ = weak.update(cx, |state, cx| {
    state.count += 1;
    cx.notify();
}).ok(); // Ignore errors
```

### Update Panics

Nested updates on the same entity will panic:

```rust
// ❌ Will panic
entity.update(cx, |state1, cx| {
    entity.update(cx, |state2, cx| {
        // Panic: entity already borrowed
    });
});
```

**Solution:** Perform updates sequentially or use different entities.

## Type Conversions

### Entity → WeakEntity

```rust
let entity: Entity<T> = cx.new(|cx| T::default());
let weak: WeakEntity<T> = entity.downgrade();
```

### WeakEntity → Entity

```rust
let weak: WeakEntity<T> = entity.downgrade();
let strong: Option<Entity<T>> = weak.upgrade();
```

### AnyEntity

```rust
let any: AnyEntity = entity.into();
let typed: Option<Entity<T>> = any.downcast::<T>();
```

## Best Practice Guidelines

### Always Use Inner cx

```rust
// ✅ Good: Use inner cx
entity.update(cx, |state, inner_cx| {
    inner_cx.notify(); // Use inner_cx, not outer cx
});

// ❌ Bad: Use outer cx
entity.update(cx, |state, inner_cx| {
    cx.notify(); // Wrong! Multiple borrow error
});
```

### Weak References in Closures

```rust
// ✅ Good: Weak reference
let weak = cx.entity().downgrade();
callback(move || {
    let _ = weak.update(cx, |state, cx| {
        cx.notify();
    });
});

// ❌ Bad: Strong reference (retain cycle)
let strong = cx.entity();
callback(move || {
    strong.update(cx, |state, cx| {
        // May never be dropped
        cx.notify();
    });
});
```

### Sequential Updates

```rust
// ✅ Good: Sequential updates
entity1.update(cx, |state, cx| { /* ... */ });
entity2.update(cx, |state, cx| { /* ... */ });

// ❌ Bad: Nested updates
entity1.update(cx, |_, cx| {
    entity2.update(cx, |_, cx| {
        // May panic if entities are related
    });
});
```
