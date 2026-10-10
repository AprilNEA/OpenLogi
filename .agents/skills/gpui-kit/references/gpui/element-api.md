# Element API Reference

**Contents:** [Element Trait Structure](#element-trait-structure) · [Associated Types](#associated-types) · [Methods](#methods) · [IntoElement Integration](#intoelement-integration) · [Layout System Integration](#layout-system-integration) · [Hitbox System](#hitbox-system) · [Event Handling](#event-handling) · [Cursor Styles](#cursor-styles)

## Element Trait Structure

The `Element` trait requires implementing three associated types and five methods:

```rust
pub trait Element: 'static + IntoElement {
    type RequestLayoutState: 'static;
    type PrepaintState: 'static;

    fn id(&self) -> Option<ElementId>;
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>>;
    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState);
    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState;
    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    );
}
```

## Associated Types

### RequestLayoutState

Data passed from `request_layout` to `prepaint` and `paint` phases.

**Usage:**
- Store layout calculations (styled text, child layout IDs)
- Cache expensive computations
- Pass child state between phases

**Examples:**
```rust
// Simple: no state needed
type RequestLayoutState = ();

// Single value
type RequestLayoutState = StyledText;

// Multiple values
type RequestLayoutState = (StyledText, Vec<ChildLayout>);

// Complex struct
pub struct MyLayoutState {
    pub styled_text: StyledText,
    pub child_layouts: Vec<(LayoutId, ChildState)>,
    pub computed_bounds: Bounds<Pixels>,
}
type RequestLayoutState = MyLayoutState;
```

### PrepaintState

Data passed from `prepaint` to `paint` phase.

**Usage:**
- Store hitboxes for interaction
- Cache visual bounds
- Store prepaint results

**Examples:**
```rust
// Simple: just a hitbox
type PrepaintState = Hitbox;

// Optional hitbox
type PrepaintState = Option<Hitbox>;

// Multiple values
type PrepaintState = (Hitbox, Vec<Bounds<Pixels>>);

// Complex struct
pub struct MyPaintState {
    pub hitbox: Hitbox,
    pub child_bounds: Vec<Bounds<Pixels>>,
    pub visible_range: Range<usize>,
}
type PrepaintState = MyPaintState;
```

## Methods

### id()

Returns optional unique identifier for debugging and inspection.

```rust
fn id(&self) -> Option<ElementId> {
    Some(self.id.clone())
}

// Or if no ID needed
fn id(&self) -> Option<ElementId> {
    None
}
```

### source_location()

Returns source location for debugging. Usually returns `None` unless debugging is needed.

```rust
fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
    None
}
```

### request_layout()

Calculates sizes and positions for the element tree.

**Parameters:**
- `global_id`: Global element identifier (optional)
- `inspector_id`: Inspector element identifier (optional)
- `window`: Mutable window reference
- `cx`: Mutable app context

**Returns:**
- `(LayoutId, Self::RequestLayoutState)`: Layout ID and state for next phases

**Responsibilities:**
1. Calculate child layouts by calling `child.request_layout()`
2. Create own layout using `window.request_layout()`
3. Return layout ID and state to pass to next phases

**Example:**
```rust
fn request_layout(
    &mut self,
    global_id: Option<&GlobalElementId>,
    inspector_id: Option<&InspectorElementId>,
    window: &mut Window,
    cx: &mut App,
) -> (LayoutId, Self::RequestLayoutState) {
    // 1. Calculate child layouts
    let child_layout_id = self.child.request_layout(
        global_id,
        inspector_id,
        window,
        cx
    ).0;

    // 2. Create own layout
    let layout_id = window.request_layout(
        Style {
            size: size(px(200.), px(100.)),
            ..default()
        },
        vec![child_layout_id],
        cx
    );

    // 3. Return layout ID and state
    (layout_id, MyLayoutState { child_layout_id })
}
```

### prepaint()

Prepares for painting by creating hitboxes and computing final bounds.

**Parameters:**
- `global_id`: Global element identifier (optional)
- `inspector_id`: Inspector element identifier (optional)
- `bounds`: Final bounds calculated by layout engine
- `request_layout`: Mutable reference to layout state
- `window`: Mutable window reference
- `cx`: Mutable app context

**Returns:**
- `Self::PrepaintState`: State for paint phase

**Responsibilities:**
1. Compute final child bounds based on layout bounds
2. Call `child.prepaint()` for all children
3. Create hitboxes using `window.insert_hitbox()`
4. Return state for paint phase

**Example:**
```rust
fn prepaint(
    &mut self,
    global_id: Option<&GlobalElementId>,
    inspector_id: Option<&InspectorElementId>,
    bounds: Bounds<Pixels>,
    request_layout: &mut Self::RequestLayoutState,
    window: &mut Window,
    cx: &mut App,
) -> Self::PrepaintState {
    // 1. Compute child bounds
    let child_bounds = bounds; // or calculated subset

    // 2. Prepaint children
    self.child.prepaint(
        global_id,
        inspector_id,
        child_bounds,
        &mut request_layout.child_state,
        window,
        cx
    );

    // 3. Create hitboxes
    let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

    // 4. Return paint state
    MyPaintState { hitbox }
}
```

### paint()

Renders the element and handles interactions.

**Parameters:**
- `global_id`: Global element identifier (optional)
- `inspector_id`: Inspector element identifier (optional)
- `bounds`: Final bounds for rendering
- `request_layout`: Mutable reference to layout state
- `prepaint`: Mutable reference to prepaint state
- `window`: Mutable window reference
- `cx`: Mutable app context

**Responsibilities:**
1. Paint children first (bottom to top)
2. Paint own content (backgrounds, borders, etc.)
3. Set up interactions (mouse events, cursor styles)

**Example:**
```rust
fn paint(
    &mut self,
    global_id: Option<&GlobalElementId>,
    inspector_id: Option<&InspectorElementId>,
    bounds: Bounds<Pixels>,
    request_layout: &mut Self::RequestLayoutState,
    prepaint: &mut Self::PrepaintState,
    window: &mut Window,
    cx: &mut App,
) {
    // 1. Paint children first
    self.child.paint(
        global_id,
        inspector_id,
        child_bounds,
        &mut request_layout.child_state,
        &mut prepaint.child_paint_state,
        window,
        cx
    );

    // 2. Paint own content
    window.paint_quad(paint_quad(
        bounds,
        Anchor::all(px(4.)),
        cx.theme().background,
    ));

    // 3. Set up interactions
    window.on_mouse_event({
        let hitbox = prepaint.hitbox.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if hitbox.is_hovered(window) && phase.bubble() {
                // Handle click
                cx.stop_propagation();
            }
        }
    });

    window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
}
```

## IntoElement Integration

Elements must also implement `IntoElement` to be used as children:

```rust
impl IntoElement for MyElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
```

This allows your custom element to be used directly in the element tree:

```rust
div()
    .child(MyElement::new()) // Works because of IntoElement
```

## Common Parameters

### Global and Inspector IDs

Both are optional identifiers used for debugging and inspection:
- `global_id`: Unique identifier across entire app
- `inspector_id`: Identifier for dev tools/inspector

Usually passed through to children without modification.

### Window and Context

- `window: &mut Window`: Window-specific operations (painting, hitboxes, events)
- `cx: &mut App`: App-wide operations (spawning tasks, accessing globals)

## Layout System Integration

### window.request_layout()

Creates a layout node with specified style and children:

```rust
let layout_id = window.request_layout(
    Style {
        size: size(px(200.), px(100.)),
        flex: Flex::Column,
        gap: px(8.),
        ..default()
    },
    vec![child1_layout_id, child2_layout_id],
    cx
);
```

### Bounds<Pixels>

Represents rectangular region:

```rust
pub struct Bounds<T> {
    pub origin: Point<T>,
    pub size: Size<T>,
}

// Create bounds
let bounds = Bounds::new(
    point(px(10.), px(20.)),
    size(px(100.), px(50.))
);

// Access properties
bounds.left()    // origin.x
bounds.top()     // origin.y
bounds.right()   // origin.x + size.width
bounds.bottom()  // origin.y + size.height
bounds.center()  // center point
```

## Hitbox System

### Creating Hitboxes

```rust
// Normal hitbox (blocks events)
let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

// Transparent hitbox (passes events through)
let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Transparent);
```

### Using Hitboxes

```rust
// Check if hovered
if hitbox.is_hovered(window) {
    // ...
}

// Set cursor style
window.set_cursor_style(CursorStyle::PointingHand, &hitbox);

// Use in event handlers
window.on_mouse_event(move |event, phase, window, cx| {
    if hitbox.is_hovered(window) && phase.bubble() {
        // Handle event
    }
});
```

## Event Handling

### Mouse Events

```rust
// Mouse down
window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
    if phase.bubble() && bounds.contains(&event.position) {
        // Handle mouse down
        cx.stop_propagation(); // Prevent bubbling
    }
});

// Mouse up
window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
    // Handle mouse up
});

// Mouse move
window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
    // Handle mouse move
});

// Scroll
window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
    // Handle scroll
});
```

### Event Phase

Events go through two phases:
- **Capture**: Top-down (parent → child)
- **Bubble**: Bottom-up (child → parent)

```rust
move |event, phase, window, cx| {
    if phase.capture() {
        // Handle in capture phase
    } else if phase.bubble() {
        // Handle in bubble phase
    }

    cx.stop_propagation(); // Stop event from continuing
}
```

## Cursor Styles

Available cursor styles:

```rust
CursorStyle::Arrow
CursorStyle::IBeam           // Text selection
CursorStyle::PointingHand    // Clickable
CursorStyle::ResizeLeft
CursorStyle::ResizeRight
CursorStyle::ResizeUp
CursorStyle::ResizeDown
CursorStyle::ResizeLeftRight
CursorStyle::ResizeUpDown
CursorStyle::Crosshair
CursorStyle::OperationNotAllowed
```

Usage:

```rust
window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
```
