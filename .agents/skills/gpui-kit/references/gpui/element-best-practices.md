# Element Best Practices

**Contents:** [State Management](#state-management) · [Performance Considerations](#performance-considerations) · [Interaction Handling](#interaction-handling) · [Layout Strategies](#layout-strategies) · [Error Handling](#error-handling) · [Testing Element Implementations](#testing-element-implementations) · [Common Pitfalls](#common-pitfalls) · [Performance Checklist](#performance-checklist)

## State Management

### Using Associated Types Effectively

**Good:** Use associated types to pass meaningful data between phases

```rust
// Good: Structured state with type safety
type RequestLayoutState = (StyledText, Vec<ChildLayout>);
type PrepaintState = (Hitbox, Vec<ChildBounds>);
```

**Bad:** Using empty state when you need data

```rust
// Bad: No state when you need to pass data
type RequestLayoutState = ();
type PrepaintState = ();
// Now you can't pass layout info to paint phase!
```

### Managing Complex State

For elements with complex state, create dedicated structs:

```rust
// Good: Dedicated struct for complex state
pub struct TextElementState {
    pub styled_text: StyledText,
    pub text_layout: TextLayout,
    pub child_states: Vec<ChildState>,
}

type RequestLayoutState = TextElementState;
```

**Benefits:**
- Clear documentation of state structure
- Easy to extend
- Type-safe access

### State Lifecycle

**Golden Rule:** State flows in one direction through the phases

```
request_layout → RequestLayoutState →
prepaint → PrepaintState →
paint
```

**Don't:**
- Store state in the element struct that should be in associated types
- Try to mutate element state in paint phase (use `cx.notify()` to schedule re-render)
- Pass mutable references across phase boundaries

## Performance Considerations

### Minimize Allocations in Paint Phase

**Critical:** Paint phase is called every frame during animations. Minimize allocations.

**Good:** Pre-allocate in `request_layout` or `prepaint`

```rust
impl Element for MyElement {
    fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
        -> (LayoutId, Vec<StyledText>)
    {
        // Allocate once during layout
        let styled_texts = self.children
            .iter()
            .map(|child| StyledText::new(child.text.clone()))
            .collect();

        (layout_id, styled_texts)
    }

    fn paint(&mut self, .., styled_texts: &mut Vec<StyledText>, ..) {
        // Just use pre-allocated styled_texts
        for text in styled_texts {
            text.paint(..);
        }
    }
}
```

**Bad:** Allocate in `paint` phase

```rust
fn paint(&mut self, ..) {
    // Bad: Allocation in paint phase!
    let styled_texts: Vec<_> = self.children
        .iter()
        .map(|child| StyledText::new(child.text.clone()))
        .collect();
}
```

### Cache Expensive Computations

Use memoization for expensive operations:

```rust
pub struct CachedElement {
    // Cache key
    last_text: Option<SharedString>,
    last_width: Option<Pixels>,

    // Cached result
    cached_layout: Option<TextLayout>,
}

impl Element for CachedElement {
    fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
        -> (LayoutId, TextLayout)
    {
        let current_width = window.bounds().width();

        // Check if cache is valid
        if self.last_text.as_ref() != Some(&self.text)
            || self.last_width != Some(current_width)
            || self.cached_layout.is_none()
        {
            // Recompute expensive layout
            self.cached_layout = Some(self.compute_text_layout(current_width));
            self.last_text = Some(self.text.clone());
            self.last_width = Some(current_width);
        }

        // Use cached layout
        let layout = self.cached_layout.as_ref().unwrap();
        (layout_id, layout.clone())
    }
}
```

### Lazy Child Rendering

Only render visible children in scrollable containers:

```rust
fn paint(&mut self, .., bounds: Bounds<Pixels>, paint_state: &mut Self::PrepaintState, ..) {
    for (i, child) in self.children.iter_mut().enumerate() {
        let child_bounds = paint_state.child_bounds[i];

        // Only paint visible children
        if self.is_visible(&child_bounds, &bounds) {
            child.paint(..);
        }
    }
}

fn is_visible(&self, child_bounds: &Bounds<Pixels>, container_bounds: &Bounds<Pixels>) -> bool {
    child_bounds.bottom() >= container_bounds.top() &&
    child_bounds.top() <= container_bounds.bottom()
}
```

## Interaction Handling

### Proper Event Bubbling

Always check phase and bounds before handling events:

```rust
fn paint(&mut self, .., window: &mut Window, cx: &mut App) {
    window.on_mouse_event({
        let hitbox = self.hitbox.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            // Check phase first
            if !phase.bubble() {
                return;
            }

            // Check if event is within bounds
            if !hitbox.is_hovered(window) {
                return;
            }

            // Handle event
            self.handle_click(event);

            // Stop propagation if handled
            cx.stop_propagation();
        }
    });
}
```

**Don't forget:**
- Check `phase.bubble()` or `phase.capture()` as appropriate
- Check hitbox hover state or bounds
- Call `cx.stop_propagation()` if you handle the event

### Hitbox Management

Create hitboxes in `prepaint` phase, not `paint`:

**Good:**

```rust
fn prepaint(&mut self, .., bounds: Bounds<Pixels>, window: &mut Window, ..) -> Hitbox {
    // Create hitbox in prepaint
    window.insert_hitbox(bounds, HitboxBehavior::Normal)
}

fn paint(&mut self, .., hitbox: &mut Hitbox, window: &mut Window, ..) {
    // Use hitbox in paint
    window.set_cursor_style(CursorStyle::PointingHand, hitbox);
}
```

**Hitbox Behaviors:**

```rust
// Normal: Blocks events from passing through
HitboxBehavior::Normal

// Transparent: Allows events to pass through to elements below
HitboxBehavior::Transparent
```

### Cursor Style Guidelines

Set appropriate cursor styles for interactivity cues:

```rust
// Text selection
window.set_cursor_style(CursorStyle::IBeam, &hitbox);

// Clickable elements (desktop convention: use default, not pointing hand)
window.set_cursor_style(CursorStyle::Arrow, &hitbox);

// Links (web convention: use pointing hand)
window.set_cursor_style(CursorStyle::PointingHand, &hitbox);

// Resizable edges
window.set_cursor_style(CursorStyle::ResizeLeftRight, &hitbox);
```

**Desktop vs Web Convention:**
- Desktop apps: Use `Arrow` for buttons
- Web apps: Use `PointingHand` for links only

## Layout Strategies

### Fixed Size Elements

For elements with known, unchanging size:

```rust
fn request_layout(&mut self, .., window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
    let layout_id = window.request_layout(
        Style {
            size: size(px(200.), px(100.)),
            ..default()
        },
        vec![], // No children
        cx
    );
    (layout_id, ())
}
```

### Content-Based Sizing

For elements sized by their content:

```rust
fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
    -> (LayoutId, Size<Pixels>)
{
    // Measure content
    let text_bounds = self.measure_text(window);
    let padding = px(16.);

    let layout_id = window.request_layout(
        Style {
            size: size(
                text_bounds.width() + padding * 2.,
                text_bounds.height() + padding * 2.,
            ),
            ..default()
        },
        vec![],
        cx
    );

    (layout_id, text_bounds)
}
```

### Flexible Layouts

For elements that adapt to available space:

```rust
fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
    -> (LayoutId, Vec<LayoutId>)
{
    let mut child_layout_ids = Vec::new();

    for child in &mut self.children {
        let (layout_id, _) = child.request_layout(window, cx);
        child_layout_ids.push(layout_id);
    }

    let layout_id = window.request_layout(
        Style {
            flex_direction: FlexDirection::Row,
            gap: px(8.),
            size: Size {
                width: relative(1.0),  // Fill parent width
                height: auto(),        // Auto height
            },
            ..default()
        },
        child_layout_ids.clone(),
        cx
    );

    (layout_id, child_layout_ids)
}
```

## Error Handling

### Graceful Degradation

Handle errors gracefully, don't panic:

```rust
fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
    -> (LayoutId, Option<TextLayout>)
{
    // Try to create styled text
    match StyledText::new(self.text.clone()).request_layout(None, None, window, cx) {
        Ok((layout_id, text_layout)) => {
            (layout_id, Some(text_layout))
        }
        Err(e) => {
            // Log error
            eprintln!("Failed to layout text: {}", e);

            // Fallback to simple text
            let fallback_text = StyledText::new("(Error loading text)".into());
            let (layout_id, _) = fallback_text.request_layout(None, None, window, cx);
            (layout_id, None)
        }
    }
}
```

### Defensive Bounds Checking

Always validate bounds and indices:

```rust
fn paint_selection(&self, selection: &Selection, text_layout: &TextLayout, ..) {
    // Validate selection bounds
    let start = selection.start.min(self.text.len());
    let end = selection.end.min(self.text.len());

    if start >= end {
        return; // Invalid selection
    }

    let rects = text_layout.rects_for_range(start..end);
    // Paint selection...
}
```

## Testing Element Implementations

### Layout Tests

Test that layout calculations are correct:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    #[gpui_kit::test]
    fn test_element_layout(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut window = cx.open_window(Default::default(), |_, _| ()).unwrap();

            window.update(cx, |window, cx| {
                let mut element = MyElement::new();
                let (layout_id, layout_state) = element.request_layout(
                    None,
                    None,
                    window,
                    cx
                );

                // Assert layout properties
                let bounds = window.layout_bounds(layout_id);
                assert_eq!(bounds.size.width, px(200.));
                assert_eq!(bounds.size.height, px(100.));
            });
        });
    }
}
```

### Interaction Tests

Test that interactions work correctly:

```rust
#[gpui_kit::test]
fn test_element_click(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let mut window = cx.open_window(Default::default(), |_, cx| {
            cx.new(|_| MyElement::new())
        }).unwrap();

        window.update(cx, |window, cx| {
            let view = window.root_view().unwrap();

            // Simulate click
            let position = point(px(10.), px(10.));
            window.dispatch_event(MouseDownEvent {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            });

            // Assert element responded
            view.read(cx).assert_clicked();
        });
    });
}
```

## Common Pitfalls

### ❌ Storing Layout State in Element Struct

**Bad:**

```rust
pub struct MyElement {
    id: ElementId,
    // Bad: This should be in RequestLayoutState
    cached_layout: Option<TextLayout>,
}
```

**Good:**

```rust
pub struct MyElement {
    id: ElementId,
    text: SharedString,
}

type RequestLayoutState = TextLayout; // Good: State in associated type
```

### ❌ Mutating Element in Paint Phase

**Bad:**

```rust
fn paint(&mut self, ..) {
    self.counter += 1; // Bad: Mutating element in paint
}
```

**Good:**

```rust
fn paint(&mut self, .., window: &mut Window, cx: &mut App) {
    window.on_mouse_event(move |event, phase, window, cx| {
        if phase.bubble() {
            self.counter += 1;
            cx.notify(); // Schedule re-render
        }
    });
}
```

### ❌ Creating Hitboxes in Paint Phase

**Bad:**

```rust
fn paint(&mut self, .., bounds: Bounds<Pixels>, window: &mut Window, ..) {
    // Bad: Creating hitbox in paint
    let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
}
```

**Good:**

```rust
fn prepaint(&mut self, .., bounds: Bounds<Pixels>, window: &mut Window, ..) -> Hitbox {
    // Good: Creating hitbox in prepaint
    window.insert_hitbox(bounds, HitboxBehavior::Normal)
}
```

### ❌ Ignoring Event Phase

**Bad:**

```rust
window.on_mouse_event(move |event, phase, window, cx| {
    // Bad: Not checking phase
    self.handle_click(event);
});
```

**Good:**

```rust
window.on_mouse_event(move |event, phase, window, cx| {
    // Good: Checking phase
    if !phase.bubble() {
        return;
    }
    self.handle_click(event);
});
```

## Performance Checklist

Before shipping an element implementation, verify:

- [ ] No allocations in `paint` phase (except event handlers)
- [ ] Expensive computations are cached/memoized
- [ ] Only visible children are rendered in scrollable containers
- [ ] Hitboxes created in `prepaint`, not `paint`
- [ ] Event handlers check phase and bounds
- [ ] Layout state is passed through associated types, not stored in element
- [ ] Element implements proper error handling with fallbacks
- [ ] Tests cover layout calculations and interactions
