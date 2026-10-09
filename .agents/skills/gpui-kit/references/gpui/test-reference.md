<!-- OpenLogi modification: normalized trailing whitespace and final newlines. -->

# Test Reference

**Contents:** [Testing Patterns](#testing-patterns) · [Testing Crash-Free State Management (Re-entrancy)](#testing-crash-free-state-management-re-entrancy) · [Property Testing](#property-testing) · [Distributed Systems Testing](#distributed-systems-testing) · [Mocking and Isolation](#mocking-and-isolation)

## Testing Patterns

### Basic Entity Testing

Test entity creation, updates, and reads:

```rust
#[gpui_kit::test]
fn test_counter_entity(cx: &mut TestAppContext) {
    let counter = cx.new(|cx| Counter::new(cx));

    // Test initial state
    let initial_count = counter.read_with(cx, |counter, _| counter.count);
    assert_eq!(initial_count, 0);

    // Test updates
    counter.update(cx, |counter, cx| {
        counter.count = 42;
        cx.notify();
    });

    let updated_count = counter.read_with(cx, |counter, _| counter.count);
    assert_eq!(updated_count, 42);
}
```

### Event Testing

Test event emission and handling:

```rust
#[derive(Clone)]
struct ValueChanged {
    new_value: i32,
}

impl EventEmitter<ValueChanged> for MyComponent {}

#[gpui_kit::test]
fn test_event_emission(cx: &mut TestAppContext) {
    let component = cx.new(|cx| {
        let mut comp = MyComponent::default();

        // Subscribe to self
        cx.subscribe_self(|this, event: &ValueChanged, cx| {
            this.received_value = event.new_value;
            cx.notify();
        });

        comp
    });

    // Emit event
    component.update(cx, |_, cx| {
        cx.emit(ValueChanged { new_value: 123 });
    });

    // Verify event was handled
    let received = component.read_with(cx, |comp, _| comp.received_value);
    assert_eq!(received, 123);
}
```

### Action Testing

Test action dispatching and handling:

```rust
actions!(my_app, [Increment, Decrement]);

#[gpui_kit::test]
fn test_action_dispatch(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| MyComponent::new(cx))
        }).unwrap()
    });

    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let counter = window.root(&mut cx).unwrap();

    // Dispatch action via focus handle
    let focus_handle = counter.read_with(&cx, |counter, _| counter.focus_handle.clone());
    cx.update(|window, cx| {
        focus_handle.dispatch_action(&Increment, window, cx);
    });

    let count = counter.read_with(&cx, |counter, _| counter.count);
    assert_eq!(count, 1);
}
```

### Async Testing

Test async operations and background tasks:

```rust
impl MyComponent {
    fn load_data(&self, cx: &mut Context<Self>) -> Task<i32> {
        cx.spawn(async move |this, cx| {
            // Simulate async work
            this.update(cx, |comp, _| comp.loading = true).await;
            // Return result
            42
        })
    }

    fn background_update(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // Background work
            this.update(cx, |comp, _| {
                comp.value += 10;
            }).await;
        }).detach();
    }
}

#[gpui_kit::test]
async fn test_async_operations(cx: &mut TestAppContext) {
    let component = cx.new(|cx| MyComponent::new(cx));

    // Test awaited task
    let result = component.update(cx, |comp, cx| comp.load_data(cx)).await;
    assert_eq!(result, 42);

    // Test detached task
    component.update(cx, |comp, cx| comp.background_update(cx));

    // Detached tasks don't run until you yield
    let value_before = component.read_with(cx, |comp, _| comp.value);
    assert_eq!(value_before, 0);

    // Run pending tasks
    cx.run_until_parked();

    let value_after = component.read_with(cx, |comp, _| comp.value);
    assert_eq!(value_after, 10);
}
```

### Timer Testing

Test timer-based operations:

```rust
impl MyComponent {
    fn delayed_action(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;

            this.update(cx, |comp, cx| {
                comp.action_performed = true;
                cx.notify();
            }).await;
        }).detach();
    }
}

#[gpui_kit::test]
async fn test_timers(cx: &mut TestAppContext) {
    let component = cx.new(|cx| MyComponent::new(cx));

    component.update(cx, |comp, cx| comp.delayed_action(cx));

    // Action shouldn't have completed yet
    let performed = component.read_with(cx, |comp, _| comp.action_performed);
    assert!(!performed);

    // Run until parked (timers complete)
    cx.run_until_parked();

    let performed = component.read_with(cx, |comp, _| comp.action_performed);
    assert!(performed);
}
```

### External I/O Testing

For tests involving external systems, use `allow_parking()`:

```rust
#[gpui_kit::test]
async fn test_external_io(cx: &mut TestAppContext) {
    // Allow parking for external I/O
    cx.executor().allow_parking();

    // Simulate external operation
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        tx.send(42).ok();
    });

    let result = rx.await.unwrap();
    assert_eq!(result, 42);
}
```

## Testing Crash-Free State Management (Re-entrancy)

The most dangerous class of GPUI bugs is **entity re-entrancy**: code that tries to update or read an entity while it is already locked in a render/update pass. These bugs are invisible at compile time — they only panic at runtime, typically when the user clicks something in a list or dropdown.

**Key properties of re-entrancy panics:**
- Triggered by user interaction (click, key press), not during static rendering.
- `#[should_panic]` only confirms the bug exists — tests must pass *without* panicking.
- `cx.run_until_parked()` is required after the triggering action to let deferred callbacks (`defer_in`) execute.

### Pattern: Drive `confirm` / `cancel` through the real delegate

```rust
#[gpui_kit::test]
fn test_confirm_does_not_panic(cx: &mut TestAppContext) {
    // Build the component state the same way the real app does.
    let state = cx.new(|cx| {
        SelectState::new(MyDelegate::default(), None, &mut cx.window_handle(), cx)
    });

    // Simulate the user clicking the first item — this exercises on_confirm
    // and its deferred callback, which is where re-entrancy bugs hide.
    state.update(cx, |this, cx| {
        this.state.list.update(cx, |list, cx| {
            list.delegate_mut().set_selected_index(Some(IndexPath::new(0)), window, cx);
            list.delegate_mut().confirm(false, window, cx);
        });
    });

    // Let defer_in callbacks execute — required to trigger the crash.
    cx.run_until_parked();

    // If we reach here without panicking, the re-entrancy is fixed.
    let selected = state.read_with(cx, |s, _| s.selected_value().cloned());
    assert!(selected.is_some());
}
```

### Pattern: Verify `on_will_change` and `on_confirm` hooks are called correctly

Use a recording delegate to assert that hooks fire with the right arguments and in the right order:

```rust
#[derive(Default)]
struct RecordingDelegate {
    items: Vec<MyItem>,
    will_change_calls: Vec<Vec<IndexPath>>,
    confirm_calls: Vec<Vec<IndexPath>>,
}

impl SearchableListDelegate for RecordingDelegate {
    // … required impls …

    fn on_will_change(
        &mut self,
        change: &mut SearchableListChange<Self>,
        _current: &[(IndexPath, Self::Item)],
    ) {
        self.will_change_calls.push(
            change.select_queue.iter().map(|(ix, _)| *ix).collect()
        );
    }

    fn on_confirm(&mut self, final_selection: &[(IndexPath, Self::Item)]) {
        self.confirm_calls.push(
            final_selection.iter().map(|(ix, _)| *ix).collect()
        );
    }
}

#[gpui_kit::test]
fn test_hooks_fire_in_correct_order(cx: &mut TestAppContext) {
    let state = cx.new(|cx| SelectState::new(RecordingDelegate::with_items(3), None, window, cx));

    // Simulate confirm on item 0
    state.update(cx, |this, cx| {
        // … trigger confirm …
    });
    cx.run_until_parked();

    state.read_with(cx, |s, cx| {
        let delegate = s.state.list.read(cx).delegate().delegate;
        assert_eq!(delegate.will_change_calls.len(), 1);
        assert_eq!(delegate.confirm_calls.len(), 1);
        assert_eq!(delegate.confirm_calls[0], vec![IndexPath::new(0)]);
    });
}
```

### Pattern: Rapid multiple confirms (snapshot consistency)

```rust
#[gpui_kit::test]
fn test_rapid_confirms_keep_consistent_snapshot(cx: &mut TestAppContext) {
    let state = cx.new(|cx| SelectState::new(MyDelegate::with_items(5), None, window, cx));

    for i in 0..5 {
        state.update(cx, |this, cx| {
            // trigger confirm on item i
        });
        cx.run_until_parked();

        state.read_with(cx, |s, cx| {
            let snapshot = s.state.list.read(cx).delegate().selection_snapshot.clone();
            let selection = s.state.selection.clone();
            assert_eq!(snapshot, selection, "snapshot out of sync after confirm {i}");
        });
    }
}
```

### Checklist: What to test for each component that uses `defer_in`

- [ ] `confirm` path: no panic, correct final selection, snapshot matches selection
- [ ] `cancel` path: no panic, selection unchanged, popover closed
- [ ] `on_will_change` veto: selection not changed, `on_confirm` not called
- [ ] `on_will_change` mutating the change: final selection reflects delegate's modification
- [ ] Rapid consecutive confirms: each one leaves snapshot consistent with selection
- [ ] `render_item` never panics even when called immediately after a mutation

## Property Testing

Use random data to test edge cases:

```rust
#[gpui_kit::test(iterations = 10)]
fn test_counter_random_operations(cx: &mut TestAppContext, mut rng: StdRng) {
    let counter = cx.new(|cx| Counter::new(cx));

    let mut expected = 0i32;
    for _ in 0..100 {
        let delta = rng.random_range(-10..=10);
        expected += delta;

        counter.update(cx, |counter, cx| {
            counter.count += delta;
            cx.notify();
        });
    }

    let actual = counter.read_with(cx, |counter, _| counter.count);
    assert_eq!(actual, expected);
}
```

## Distributed Systems Testing

Test multiple app contexts communicating:

```rust
#[derive(Clone)]
struct NetworkMessage {
    from: String,
    to: String,
    data: i32,
}

#[gpui_kit::test]
fn test_distributed_apps(cx_a: &mut TestAppContext, cx_b: &mut TestAppContext) {
    // Create components in different app contexts
    let comp_a = cx_a.new(|_| MyComponent::new("A".to_string()));
    let comp_b = cx_b.new(|_| MyComponent::new("B".to_string()));

    // Simulate message passing
    comp_a.update(cx_a, |comp, cx| {
        comp.send_message("B", 42, cx);
    });

    // Run async operations
    cx_a.run_until_parked();

    // Verify message received in other context
    comp_b.update(cx_b, |comp, _| {
        comp.receive_messages();
    });

    let messages = comp_b.read_with(cx_b, |comp, _| comp.messages.clone());
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].data, 42);
}
```

### Interleaving Testing

Test concurrent operations with random execution order:

```rust
#[gpui_kit::test(iterations = 10)]
fn test_concurrent_operations(
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    mut rng: StdRng,
) {
    let comp_a = cx_a.new(|_| MyComponent::new());
    let comp_b = cx_b.new(|_| MyComponent::new());

    // Perform random operations across contexts
    for i in 0..20 {
        if rng.random_bool(0.5) {
            comp_a.update(cx_a, |comp, cx| {
                comp.perform_operation(i, cx);
            });
        } else {
            comp_b.update(cx_b, |comp, cx| {
                comp.perform_operation(i, cx);
            });
        }
    }

    // Run all pending operations
    cx_a.run_until_parked();

    // Verify final state
    let state_a = comp_a.read_with(cx_a, |comp, _| comp.state);
    let state_b = comp_b.read_with(cx_b, |comp, _| comp.state);

    // Assert invariants hold despite execution order
    assert!(state_a.is_consistent());
    assert!(state_b.is_consistent());
}
```

## Mocking and Isolation

### Network Mocking

Create mock networks for testing distributed features:

```rust
struct MockNetwork {
    messages: Arc<Mutex<Vec<NetworkMessage>>>,
}

impl MockNetwork {
    fn new() -> Self {
        Self {
            messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn send(&self, message: NetworkMessage) {
        self.messages.lock().unwrap().push(message);
    }

    fn receive_all(&self) -> Vec<NetworkMessage> {
        self.messages.lock().unwrap().drain(..).collect()
    }
}

#[gpui_kit::test]
fn test_networked_components(cx: &mut TestAppContext) {
    let network = Arc::new(MockNetwork::new());

    let sender = cx.new(|_| MessageSender::new(network.clone()));
    let receiver = cx.new(|_| MessageReceiver::new(network));

    // Send message
    sender.update(cx, |sender, _| {
        sender.send("Hello");
    });

    // Receive message
    receiver.update(cx, |receiver, _| {
        receiver.receive_all();
    });

    let received = receiver.read_with(cx, |receiver, _| receiver.messages.clone());
    assert_eq!(received, vec!["Hello"]);
}
```
