# Entity Patterns

**Contents:** [Application Scenarios](#application-scenarios) · [Common Patterns](#common-patterns) · [Pattern Selection Guide](#pattern-selection-guide)

## Application Scenarios

### Model-View Separation

Separate business logic (model) from UI (view) using entities.

```rust
struct CounterModel {
    count: usize,
    listeners: Vec<Box<dyn Fn(usize)>>,
}

struct CounterView {
    model: Entity<CounterModel>,
}

impl CounterModel {
    fn increment(&mut self, cx: &mut Context<Self>) {
        self.count += 1;

        // Notify listeners
        for listener in &self.listeners {
            listener(self.count);
        }

        cx.notify();
    }

    fn decrement(&mut self, cx: &mut Context<Self>) {
        if self.count > 0 {
            self.count -= 1;
            cx.notify();
        }
    }
}

impl CounterView {
    fn new(cx: &mut App) -> Entity<Self> {
        let model = cx.new(|_cx| CounterModel {
            count: 0,
            listeners: Vec::new(),
        });

        cx.new(|cx| Self { model })
    }

    fn increment_count(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| {
            model.increment(cx);
        });
    }
}

impl Render for CounterView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.model.read(cx).count;

        div()
            .child(format!("Count: {}", count))
            .child(
                Button::new("increment")
                    .label("Increment")
                    .on_click(cx.listener(|this, _, cx| {
                        this.increment_count(cx);
                    }))
            )
    }
}
```

### Component State Management

Managing complex component state with entities.

```rust
struct TodoList {
    todos: Vec<Todo>,
    filter: TodoFilter,
    next_id: usize,
}

struct Todo {
    id: usize,
    text: String,
    completed: bool,
}

enum TodoFilter {
    All,
    Active,
    Completed,
}

impl TodoList {
    fn new() -> Self {
        Self {
            todos: Vec::new(),
            filter: TodoFilter::All,
            next_id: 0,
        }
    }

    fn add_todo(&mut self, text: String, cx: &mut Context<Self>) {
        self.todos.push(Todo {
            id: self.next_id,
            text,
            completed: false,
        });
        self.next_id += 1;
        cx.notify();
    }

    fn toggle_todo(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(todo) = self.todos.iter_mut().find(|t| t.id == id) {
            todo.completed = !todo.completed;
            cx.notify();
        }
    }

    fn remove_todo(&mut self, id: usize, cx: &mut Context<Self>) {
        self.todos.retain(|t| t.id != id);
        cx.notify();
    }

    fn set_filter(&mut self, filter: TodoFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }

    fn visible_todos(&self) -> impl Iterator<Item = &Todo> {
        self.todos.iter().filter(move |todo| match self.filter {
            TodoFilter::All => true,
            TodoFilter::Active => !todo.completed,
            TodoFilter::Completed => todo.completed,
        })
    }
}
```

### Cross-Entity Communication

Coordinating state between parent and child entities.

```rust
struct ParentComponent {
    child_entities: Vec<Entity<ChildComponent>>,
    global_message: String,
}

struct ChildComponent {
    id: usize,
    message: String,
    parent: WeakEntity<ParentComponent>,
}

impl ParentComponent {
    fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            child_entities: Vec::new(),
            global_message: String::new(),
        })
    }

    fn add_child(&mut self, cx: &mut Context<Self>) {
        let parent_weak = cx.entity().downgrade();
        let child_id = self.child_entities.len();

        let child = cx.new(|cx| ChildComponent {
            id: child_id,
            message: String::new(),
            parent: parent_weak,
        });

        self.child_entities.push(child);
        cx.notify();
    }

    fn broadcast_message(&mut self, message: String, cx: &mut Context<Self>) {
        self.global_message = message.clone();

        // Update all children
        for child in &self.child_entities {
            child.update(cx, |child_state, cx| {
                child_state.message = message.clone();
                cx.notify();
            });
        }

        cx.notify();
    }
}

impl ChildComponent {
    fn notify_parent(&mut self, message: String, cx: &mut Context<Self>) {
        if let Ok(_) = self.parent.update(cx, |parent_state, cx| {
            parent_state.global_message = format!("Child {}: {}", self.id, message);
            cx.notify();
        }) {
            // Parent successfully notified
        }
    }
}
```

### Async Operations with Entities

Managing async state updates.

```rust
struct DataLoader {
    loading: bool,
    data: Option<String>,
    error: Option<String>,
}

impl DataLoader {
    fn new() -> Self {
        Self {
            loading: false,
            data: None,
            error: None,
        }
    }

    fn load_data(&mut self, cx: &mut Context<Self>) {
        // Set loading state
        self.loading = true;
        self.error = None;
        cx.notify();

        // Get weak reference for async task
        let entity = cx.entity().downgrade();

        cx.spawn(async move |cx| {
            // Simulate async operation
            tokio::time::sleep(Duration::from_secs(2)).await;
            let result = fetch_data().await;

            // Update entity with result
            let _ = entity.update(cx, |state, cx| {
                state.loading = false;
                match result {
                    Ok(data) => state.data = Some(data),
                    Err(e) => state.error = Some(e.to_string()),
                }
                cx.notify();
            });
        }).detach();
    }
}

async fn fetch_data() -> Result<String, anyhow::Error> {
    // Actual fetch implementation
    Ok("Fetched data".to_string())
}
```

### Background Task Coordination

Using background tasks with entity updates.

```rust
struct ImageProcessor {
    images: Vec<ProcessedImage>,
    processing: bool,
}

struct ProcessedImage {
    path: PathBuf,
    thumbnail: Option<Vec<u8>>,
}

impl ImageProcessor {
    fn process_images(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.processing = true;
        cx.notify();

        let entity = cx.entity().downgrade();

        cx.background_spawn({
            let paths = paths.clone();
            async move {
                let mut processed = Vec::new();

                for path in paths {
                    // Process image on background thread
                    let thumbnail = generate_thumbnail(&path).await;
                    processed.push((path, thumbnail));
                }

                // Send results back to foreground
                processed
            }
        })
        .then(cx.spawn(move |processed, cx| {
            // Update entity on foreground thread
            let _ = entity.update(cx, |state, cx| {
                for (path, thumbnail) in processed {
                    state.images.push(ProcessedImage {
                        path,
                        thumbnail: Some(thumbnail),
                    });
                }
                state.processing = false;
                cx.notify();
            });
        }))
        .detach();
    }
}
```

## Common Patterns

### 1. Stateful Components

Use entities for components that maintain internal state.

```rust
struct StatefulComponent {
    value: i32,
    history: Vec<i32>,
}

impl StatefulComponent {
    fn update_value(&mut self, new_value: i32, cx: &mut Context<Self>) {
        self.history.push(self.value);
        self.value = new_value;
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if let Some(prev_value) = self.history.pop() {
            self.value = prev_value;
            cx.notify();
        }
    }
}
```

### 2. Shared State

Share state between multiple components using entities.

```rust
struct SharedState {
    theme: Theme,
    user: Option<User>,
}

struct ComponentA {
    shared: Entity<SharedState>,
}

struct ComponentB {
    shared: Entity<SharedState>,
}

// Both components can read/update the same shared state
impl ComponentA {
    fn update_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.shared.update(cx, |state, cx| {
            state.theme = theme;
            cx.notify();
        });
    }
}
```

### 3. Event Coordination

Use entities to coordinate events between components.

```rust
struct EventCoordinator {
    listeners: Vec<WeakEntity<dyn EventListener>>,
}

trait EventListener {
    fn on_event(&mut self, event: &AppEvent, cx: &mut App);
}

impl EventCoordinator {
    fn emit_event(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        // Notify all listeners
        self.listeners.retain(|weak_listener| {
            weak_listener.update(cx, |listener, cx| {
                listener.on_event(&event, cx);
            }).is_ok()
        });
        cx.notify();
    }
}
```

### 4. Async State Management

Manage state that changes based on async operations.

```rust
struct AsyncState<T> {
    state: AsyncValue<T>,
}

enum AsyncValue<T> {
    Loading,
    Loaded(T),
    Error(String),
}

impl<T> AsyncState<T> {
    fn is_loading(&self) -> bool {
        matches!(self.state, AsyncValue::Loading)
    }

    fn value(&self) -> Option<&T> {
        match &self.state {
            AsyncValue::Loaded(v) => Some(v),
            _ => None,
        }
    }
}
```

### 5. Parent-Child Relationships

Manage hierarchical relationships with weak references.

```rust
struct Parent {
    children: Vec<Entity<Child>>,
}

struct Child {
    parent: WeakEntity<Parent>,
    data: String,
}

impl Child {
    fn notify_parent_of_change(&mut self, cx: &mut Context<Self>) {
        if let Ok(_) = self.parent.update(cx, |parent, cx| {
            // Parent can react to child change
            cx.notify();
        }) {
            // Successfully notified
        }
    }
}
```

### 6. Observer Pattern

React to entity state changes using observers.

```rust
struct Observable {
    value: i32,
}

struct Observer {
    observed: Entity<Observable>,
}

impl Observer {
    fn new(observed: Entity<Observable>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            // Observe the entity
            cx.observe(&observed, |this, observed_entity, cx| {
                // React to changes
                let value = observed_entity.read(cx).value;
                println!("Value changed to: {}", value);
            }).detach();

            Self { observed }
        })
    }
}
```

### 7. Event Subscription

Handle events emitted by other entities.

```rust
#[derive(Clone)]
enum DataEvent {
    Updated,
    Deleted,
}

struct DataSource {
    data: Vec<String>,
}

impl DataSource {
    fn update_data(&mut self, cx: &mut Context<Self>) {
        // Update data
        cx.emit(DataEvent::Updated);
        cx.notify();
    }
}

struct DataConsumer {
    source: Entity<DataSource>,
}

impl DataConsumer {
    fn new(source: Entity<DataSource>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            // Subscribe to events
            cx.subscribe(&source, |this, source, event: &DataEvent, cx| {
                match event {
                    DataEvent::Updated => {
                        // Handle update
                        cx.notify();
                    }
                    DataEvent::Deleted => {
                        // Handle deletion
                    }
                }
            }).detach();

            Self { source }
        })
    }
}
```

### 8. Resource Management

Manage external resources with proper cleanup.

```rust
struct FileHandle {
    path: PathBuf,
    file: Option<File>,
}

impl FileHandle {
    fn open(&mut self, cx: &mut Context<Self>) -> Result<()> {
        self.file = Some(File::open(&self.path)?);
        cx.notify();
        Ok(())
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.file = None;
        cx.notify();
    }
}

impl Drop for FileHandle {
    fn drop(&mut self) {
        // Cleanup when entity is dropped
        if let Some(file) = self.file.take() {
            drop(file);
        }
    }
}
```

## Pattern Selection Guide

| Need | Pattern | Complexity |
|------|---------|------------|
| Component with internal state | Stateful Components | Low |
| State shared by multiple components | Shared State | Low |
| Coordinate events between components | Event Coordination | Medium |
| Handle async data fetching | Async State Management | Medium |
| Parent-child component hierarchy | Parent-Child Relationships | Medium |
| React to state changes | Observer Pattern | Medium |
| Handle custom events | Event Subscription | Medium-High |
| Manage external resources | Resource Management | High |

Choose the simplest pattern that meets your requirements. Combine patterns as needed for complex scenarios.
