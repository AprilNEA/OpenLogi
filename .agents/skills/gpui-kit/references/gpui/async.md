<!-- OpenLogi modification: normalized trailing whitespace and final newlines. -->

# Async & Background Tasks

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Core Patterns](#core-patterns) · [Common Pitfalls](#common-pitfalls)

## Overview

GPUI provides integrated async runtime for foreground UI updates and background computation.

**Key Concepts:**

- **Foreground tasks**: UI thread, can update entities (`cx.spawn`)
- **Background tasks**: Worker threads, CPU-intensive work (`cx.background_spawn`)
- All entity updates happen on foreground thread

## Quick Start

### Foreground Tasks (UI Updates)

When spawned from `Context<Self>`, the closure receives `(WeakEntity<Self>, &mut AsyncApp)`:

```rust
impl MyComponent {
    fn fetch_data(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // Runs on UI thread, can await and update entities
            let data = fetch_from_api().await;

            this.update(cx, |state, cx| {
                state.data = Some(data);
                cx.notify();
            }).ok();
        }).detach();
    }
}
```

When spawned from `&mut App` (not inside an entity), the closure receives only `(cx: &mut AsyncApp)`:

```rust
cx.spawn(async move |cx: &mut AsyncApp| {
    // No entity reference
}).detach();
```

### Spawn with Window Context (spawn_in)

Use `spawn_in` when the task also needs window access (`update_in`):

```rust
impl MyComponent {
    fn animate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            // cx here is AsyncWindowContext
            this.update_in(cx, |state, window, cx| {
                // Can access window here
                state.frame += 1;
                cx.notify();
            }).ok();
        }).detach();
    }
}
```

### Background Tasks (Heavy Work)

```rust
impl MyComponent {
    fn process_file(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();

        cx.background_spawn(async move {
            // Runs on background thread, CPU-intensive
            let result = heavy_computation().await;
            result
        })
        .then(cx.spawn(move |result, cx| {
            // Back to foreground to update UI
            entity.update(cx, |state, cx| {
                state.result = result;
                cx.notify();
            }).ok();
        }))
        .detach();
    }
}
```

### Task Management

```rust
struct MyView {
    _task: Task<()>,  // Prefix with _ if stored but not accessed
}

impl MyView {
    fn new(cx: &mut Context<Self>) -> Self {
        let _task = cx.spawn(async move |this, cx: &mut AsyncApp| {
            // Task automatically cancelled when dropped
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                this.update(cx, |state, cx| {
                    state.tick();
                    cx.notify();
                }).ok();
            }
        });

        Self { _task }
    }
}
```

## Core Patterns

### 1. Async Data Fetching (from Context<Self>)

```rust
cx.spawn(async move |this, cx: &mut AsyncApp| {
    let data = fetch_data().await?;
    this.update(cx, |state, cx| {
        state.data = Some(data);
        cx.notify();
    })?;
    Ok::<_, anyhow::Error>(())
}).detach();
```

### 2. Background Computation + UI Update

```rust
cx.background_spawn(async move {
    heavy_work()
})
.then(cx.spawn(move |this, cx: &mut AsyncApp| {
    this.update(cx, |state, cx| {
        state.result = result;
        cx.notify();
    }).ok();
}))
.detach();
```

### 3. Periodic Tasks

```rust
cx.spawn(async move |this, cx: &mut AsyncApp| {
    loop {
        cx.background_executor().timer(Duration::from_secs(5)).await;

        this.update(cx, |state, cx| {
            state.tick();
            cx.notify();
        }).ok();
    }
}).detach();
```

### 4. Task Cancellation

Tasks are automatically cancelled when dropped. Store in struct to keep alive.

## Common Pitfalls

### ❌ Don't: Use `defer_in` and then update the same entity through its handle

`cx.defer_in(window, callback)` schedules `callback` to run **on the current entity** — GPUI re-acquires that entity's lock to execute it. Calling `entity.update(cx, …)` on the *same* entity from within the deferred callback re-enters the lock and panics:

```
cannot update … while it is already being updated
```

```rust
// ❌ Panic: list entity is locked for the defer_in; calling list.update re-enters
fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
    cx.defer_in(window, |list_state, window, cx| {
        parent.update(cx, |this, cx| {
            this.inner_list.update(cx, |_, _| {}); // PANIC if inner_list == the deferred entity
        });
    });
}
```

```rust
// ✅ Correct: use the direct &mut reference — no lock needed
fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
    cx.defer_in(window, |list_state, window, cx| {
        // Access list data directly through the &mut reference
        list_state.delegate_mut().some_method();

        // Update a *different* entity — fine, different lock
        parent.update(cx, |this, cx| { /* … */ });

        // Sync list state directly after parent update — no lock needed
        list_state.delegate_mut().update_snapshot(new_val);
    });
}
```

The rule: inside a `defer_in` callback, **never call `entity.update(cx, …)` or `entity.read(cx)` on the entity the `defer_in` was scheduled on**. Use the `&mut Entity` direct reference the callback provides instead.

### ❌ Don't: Update entities from background tasks

```rust
// ❌ Wrong: Can't update entities from background thread
cx.background_spawn(async move {
    entity.update(cx, |state, cx| { // Compile error!
        state.data = data;
    });
});
```

### ✅ Do: Use foreground task or chain

```rust
// ✅ Correct: Chain with foreground task
cx.background_spawn(async move { data })
    .then(cx.spawn(move |data, cx| {
        entity.update(cx, |state, cx| {
            state.data = data;
            cx.notify();
        }).ok();
    }))
    .detach();
```
