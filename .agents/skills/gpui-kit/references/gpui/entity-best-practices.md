# Entity Best Practices

**Contents:** [Avoiding Common Pitfalls](#avoiding-common-pitfalls) · [Performance Optimization](#performance-optimization) · [Entity Lifecycle Management](#entity-lifecycle-management) · [Entity Observation Best Practices](#entity-observation-best-practices) · [Async Best Practices](#async-best-practices) · [Testing Best Practices](#testing-best-practices) · [Performance Checklist](#performance-checklist)

## Avoiding Common Pitfalls

### Avoid Re-entrant Entity Access (Same Entity)

**Problem:** GPUI locks an entity for the entire duration of its render or update pass. Any attempt to `read` or `update` that *same* entity while the lock is held panics:

```
cannot update … while it is already being updated
```

```rust
// ❌ Panic: entity_a updated from within entity_a's own update
entity_a.update(cx, |_, cx| {
    entity_a.update(cx, |_, _| {}); // PANIC
});

// ✅ Fine: updating a *different* entity from within an update
entity_a.update(cx, |_, cx| {
    entity_b.update(cx, |_, _| {}); // OK — different lock
});
```

**Note:** Updating two *different* entities in a nested fashion is safe. The restriction is strictly about re-entering the *same* entity's lock.

### `defer_in` Re-locks the Entity — Same Rules Apply

`cx.defer_in(window, callback)` schedules `callback` to run *on the entity the context refers to*. GPUI re-acquires that entity's lock to execute the deferred callback, so the re-entrancy rules apply equally inside it:

```rust
// ❌ Panic: defer_in runs with ListState locked; calling list.update re-enters
fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
    cx.defer_in(window, |list_state, window, cx| {
        // list_state IS locked for this entire callback!
        parent.update(cx, |this, cx| {
            this.list.update(cx, |_, _| {}); // PANIC — re-enters ListState lock
        });
    });
}
```

**Fix:** Use the direct `&mut` reference the callback provides instead of going through the entity handle:

```rust
// ✅ Correct: direct mutable access — no lock needed
fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
    cx.defer_in(window, |list_state, window, cx| {
        // Step 1: call hooks directly through list_state (no entity lock)
        list_state.delegate_mut().on_will_change(&mut op, &snapshot);

        // Step 2: update the parent entity — different lock, safe
        let new_sel = parent.update(cx, |this, cx| {
            this.state.apply_change(op);
            cx.notify();
            this.state.selection.clone() // return data needed for step 3
        });

        // Step 3: sync list state directly — no entity lock
        if let Ok(sel) = new_sel {
            list_state.delegate_mut().update_snapshot(sel.clone());
            list_state.delegate_mut().on_confirm(&sel);
        }
    });
}
```

### Render Callbacks Must Not Access External Entities

`render_item` and any other rendering hook runs inside the entity's render pass. Calling `entity.read(cx)` or `entity.update(cx, …)` on an external entity (which may itself be in a render/update pass) panics with the same re-entrancy error.

**Fix:** Keep a plain `snapshot` field updated eagerly from outside render after every mutation:

```rust
// ❌ Panic — called during ListState render; external entity access re-enters
fn render_item(&mut self, ix: IndexPath, …) -> … {
    let checked = parent.read(cx).selection.contains(&ix); // PANIC
}

// ✅ Read from snapshot field — no entity access at all
fn render_item(&mut self, ix: IndexPath, …) -> … {
    let checked = self.selection_snapshot.iter().any(|(sel_ix, _)| sel_ix == &ix);
}
// After every mutation from outside render:
list.update(cx, |l, _| l.delegate_mut().update_snapshot(new_snapshot));
```

### Use Weak References in Closures

**Problem:** Strong references in closures can create retain cycles and memory leaks.

```rust
// ❌ Bad: Strong reference creates retain cycle
impl MyComponent {
    fn setup_callback(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity(); // Strong reference

        some_callback(move || {
            entity.update(cx, |state, cx| {
                // This closure holds a strong reference
                // If the closure itself is retained by the entity, memory leak!
                cx.notify();
            });
        });
    }
}
```

**Solution:** Use weak references in closures.

```rust
// ✅ Good: Weak reference prevents retain cycle
impl MyComponent {
    fn setup_callback(&mut self, cx: &mut Context<Self>) {
        let weak_entity = cx.entity().downgrade(); // Weak reference

        some_callback(move || {
            // Safe: weak reference doesn't prevent cleanup
            let _ = weak_entity.update(cx, |state, cx| {
                cx.notify();
            });
        });
    }
}
```

### Use Inner Context in Closures

**Problem:** Using outer context causes multiple borrow errors.

```rust
// ❌ Bad: Using outer cx causes borrow issues
entity.update(cx, |state, inner_cx| {
    cx.notify(); // Wrong! Using outer cx
    cx.spawn(...); // Multiple borrow error
});
```

**Solution:** Always use the inner context provided to the closure.

```rust
// ✅ Good: Use inner cx
entity.update(cx, |state, inner_cx| {
    inner_cx.notify(); // Correct
    inner_cx.spawn(...); // Works fine
});
```

### Entity as Props - Use Weak References

**Problem:** Strong entity references in props can create ownership issues.

```rust
// ❌ Questionable: Strong reference in child
struct ChildComponent {
    parent: Entity<ParentComponent>, // Strong reference
}
```

**Better:** Use weak references for parent relationships.

```rust
// ✅ Good: Weak reference prevents issues
struct ChildComponent {
    parent: WeakEntity<ParentComponent>, // Weak reference
}

impl ChildComponent {
    fn notify_parent(&mut self, cx: &mut Context<Self>) {
        // Check if parent still exists
        if let Ok(_) = self.parent.update(cx, |parent_state, cx| {
            // Update parent
            cx.notify();
        }) {
            // Parent successfully updated
        }
    }
}
```

## Performance Optimization

### Minimize cx.notify() Calls

Each `cx.notify()` triggers a re-render. Batch updates when possible.

```rust
// ❌ Bad: Multiple notifications
impl MyComponent {
    fn update_multiple_fields(&mut self, cx: &mut Context<Self>) {
        self.field1 = new_value1;
        cx.notify(); // Unnecessary intermediate notification

        self.field2 = new_value2;
        cx.notify(); // Unnecessary intermediate notification

        self.field3 = new_value3;
        cx.notify();
    }
}
```

```rust
// ✅ Good: Single notification after all updates
impl MyComponent {
    fn update_multiple_fields(&mut self, cx: &mut Context<Self>) {
        self.field1 = new_value1;
        self.field2 = new_value2;
        self.field3 = new_value3;
        cx.notify(); // Single notification
    }
}
```

### Conditional Updates

Only notify when state actually changes.

```rust
impl MyComponent {
    fn set_value(&mut self, new_value: i32, cx: &mut Context<Self>) {
        if self.value != new_value {
            self.value = new_value;
            cx.notify(); // Only notify if changed
        }
    }
}
```

### Use read_with for Complex Operations

Prefer `read_with` over separate `read` calls.

```rust
// ❌ Less efficient: Multiple borrows
let state_ref = entity.read(cx);
let value1 = state_ref.field1;
let value2 = state_ref.field2;
// state_ref borrowed for entire scope

// ✅ More efficient: Single borrow with closure
let (value1, value2) = entity.read_with(cx, |state, cx| {
    (state.field1, state.field2)
});
```

### Avoid Excessive Entity Creation

Creating entities has overhead. Reuse when appropriate.

```rust
// ❌ Bad: Creating entity per item in render
impl Render for MyList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().children(
            self.items.iter().map(|item| {
                // Don't create entities in render!
                let entity = cx.new(|_| item.clone());
                ItemView { entity }
            })
        )
    }
}
```

```rust
// ✅ Good: Create entities once, reuse
struct MyList {
    item_entities: Vec<Entity<Item>>,
}

impl MyList {
    fn add_item(&mut self, item: Item, cx: &mut Context<Self>) {
        let entity = cx.new(|_| item);
        self.item_entities.push(entity);
        cx.notify();
    }
}
```

## Entity Lifecycle Management

### Clean Up Weak References

Periodically clean up invalid weak references from collections.

```rust
struct Container {
    weak_children: Vec<WeakEntity<Child>>,
}

impl Container {
    fn cleanup_invalid_children(&mut self, cx: &mut Context<Self>) {
        // Remove weak references that are no longer valid
        let before_count = self.weak_children.len();
        self.weak_children.retain(|weak| weak.upgrade().is_some());
        let after_count = self.weak_children.len();

        if before_count != after_count {
            cx.notify(); // Notify if list changed
        }
    }
}
```

### Entity Cloning and Sharing

Understand that cloning `Entity<T>` increases reference count.

```rust
// Each clone increases the reference count
let entity1: Entity<MyState> = cx.new(|_| MyState::default());
let entity2 = entity1.clone(); // Reference count: 2
let entity3 = entity1.clone(); // Reference count: 3

// Entity is dropped only when all references are dropped
drop(entity1); // Reference count: 2
drop(entity2); // Reference count: 1
drop(entity3); // Reference count: 0, entity is deallocated
```

### Proper Resource Cleanup

Implement cleanup in `Drop` or explicit cleanup methods.

```rust
struct ManagedResource {
    handle: Option<FileHandle>,
}

impl ManagedResource {
    fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.handle.take() {
            // Explicit cleanup
            handle.close();
            cx.notify();
        }
    }
}

impl Drop for ManagedResource {
    fn drop(&mut self) {
        // Automatic cleanup when entity is dropped
        if let Some(handle) = self.handle.take() {
            handle.close();
        }
    }
}
```

## Entity Observation Best Practices

### Detach Subscriptions Appropriately

Call `.detach()` on subscriptions you want to keep alive.

```rust
impl MyComponent {
    fn new(other_entity: Entity<OtherComponent>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            // Observer will live as long as both entities exist
            cx.observe(&other_entity, |this, observed, cx| {
                // Handle changes
                cx.notify();
            }).detach(); // Important: detach to make permanent

            Self { /* fields */ }
        })
    }
}
```

### Avoid Observation Cycles

Don't create mutual observation between entities.

```rust
// ❌ Bad: Mutual observation can cause infinite loops
entity1.update(cx, |_, cx| {
    cx.observe(&entity2, |_, _, cx| {
        cx.notify(); // May trigger entity2's observer
    }).detach();
});

entity2.update(cx, |_, cx| {
    cx.observe(&entity1, |_, _, cx| {
        cx.notify(); // May trigger entity1's observer → infinite loop
    }).detach();
});
```

## Async Best Practices

### Always Use Weak References in Async Tasks

```rust
// ✅ Good: Weak reference in spawned task
impl MyComponent {
    fn fetch_data(&mut self, cx: &mut Context<Self>) {
        let weak_entity = cx.entity().downgrade();

        cx.spawn(async move |cx| {
            let data = fetch_from_api().await;

            // Entity may have been dropped during fetch
            let _ = weak_entity.update(cx, |state, cx| {
                state.data = Some(data);
                cx.notify();
            });
        }).detach();
    }
}
```

### Handle Async Errors Gracefully

```rust
impl MyComponent {
    fn fetch_data(&mut self, cx: &mut Context<Self>) {
        let weak_entity = cx.entity().downgrade();

        cx.spawn(async move |cx| {
            match fetch_from_api().await {
                Ok(data) => {
                    let _ = weak_entity.update(cx, |state, cx| {
                        state.data = Some(data);
                        state.error = None;
                        cx.notify();
                    });
                }
                Err(e) => {
                    let _ = weak_entity.update(cx, |state, cx| {
                        state.error = Some(e.to_string());
                        cx.notify();
                    });
                }
            }
        }).detach();
    }
}
```

### Cancellation Patterns

Implement cancellation for long-running tasks.

```rust
struct DataFetcher {
    current_task: Option<Task<()>>,
    data: Option<String>,
}

impl DataFetcher {
    fn fetch_data(&mut self, url: String, cx: &mut Context<Self>) {
        // Cancel previous task
        self.current_task = None; // Dropping task cancels it

        let weak_entity = cx.entity().downgrade();

        let task = cx.spawn(async move |cx| {
            let data = fetch_from_url(&url).await?;

            let _ = weak_entity.update(cx, |state, cx| {
                state.data = Some(data);
                cx.notify();
            });

            Ok::<(), anyhow::Error>(())
        });

        self.current_task = Some(task);
    }
}
```

## Testing Best Practices

### Use TestAppContext for Entity Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    #[gpui_kit::test]
    fn test_entity_update(cx: &mut TestAppContext) {
        let entity = cx.new(|_| MyState { count: 0 });

        entity.update(cx, |state, cx| {
            state.count += 1;
            assert_eq!(state.count, 1);
        });

        let count = entity.read(cx).count;
        assert_eq!(count, 1);
    }
}
```

### Test Entity Observation

```rust
#[gpui_kit::test]
fn test_entity_observation(cx: &mut TestAppContext) {
    let observed = cx.new(|_| MyState { value: 0 });
    let observer = cx.new(|cx| Observer::new(observed.clone(), cx));

    // Update observed entity
    observed.update(cx, |state, cx| {
        state.value = 42;
        cx.notify();
    });

    // Verify observer was notified
    observer.read(cx).assert_observed();
}
```

## Performance Checklist

Before shipping entity-based code, verify:

- [ ] No strong references in closures/callbacks (use `WeakEntity`)
- [ ] No nested entity updates (use sequential updates)
- [ ] Using inner `cx` in update closures
- [ ] Batching updates before calling `cx.notify()`
- [ ] Cleaning up invalid weak references periodically
- [ ] Using `read_with` for complex read operations
- [ ] Properly detaching subscriptions and observers
- [ ] Using weak references in async tasks
- [ ] No observation cycles between entities
- [ ] Proper error handling in async operations
- [ ] Resource cleanup in `Drop` or explicit methods
- [ ] Tests cover entity lifecycle and interactions
