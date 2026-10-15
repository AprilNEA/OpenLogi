# Common Element Patterns

**Contents:** [Text Rendering Elements](#text-rendering-elements) · [Container Elements](#container-elements) · [Interactive Elements](#interactive-elements) · [Composite Elements](#composite-elements) · [Scrollable Elements](#scrollable-elements) · [Pattern Selection Guide](#pattern-selection-guide)

## Text Rendering Elements

Elements that display and manipulate text content.

### Pattern Characteristics

- Use `StyledText` for text layout and rendering
- Handle text selection in `paint` phase with hitbox interaction
- Create hitboxes for text interaction in `prepaint`
- Support text highlighting and custom styling via runs

### Implementation Template

```rust
pub struct TextElement {
    id: ElementId,
    text: SharedString,
    style: TextStyle,
}

impl Element for TextElement {
    type RequestLayoutState = StyledText;
    type PrepaintState = Hitbox;

    fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
        -> (LayoutId, StyledText)
    {
        let styled_text = StyledText::new(self.text.clone())
            .with_style(self.style);
        let (layout_id, _) = styled_text.request_layout(None, None, window, cx);
        (layout_id, styled_text)
    }

    fn prepaint(&mut self, .., bounds: Bounds<Pixels>, styled_text: &mut StyledText,
                window: &mut Window, cx: &mut App) -> Hitbox
    {
        styled_text.prepaint(None, None, bounds, &mut (), window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(&mut self, .., bounds: Bounds<Pixels>, styled_text: &mut StyledText,
             hitbox: &mut Hitbox, window: &mut Window, cx: &mut App)
    {
        styled_text.paint(None, None, bounds, &mut (), &mut (), window, cx);
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
    }
}
```

### Use Cases

- Code editors with syntax highlighting
- Rich text displays
- Labels with custom formatting
- Selectable text areas

## Container Elements

Elements that manage and layout child elements.

### Pattern Characteristics

- Manage child element layouts and positions
- Handle scrolling and clipping when needed
- Implement flex/grid-like layouts
- Coordinate child interactions and event delegation

### Implementation Template

```rust
pub struct ContainerElement {
    id: ElementId,
    children: Vec<AnyElement>,
    direction: FlexDirection,
    gap: Pixels,
}

impl Element for ContainerElement {
    type RequestLayoutState = Vec<LayoutId>;
    type PrepaintState = Vec<Bounds<Pixels>>;

    fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
        -> (LayoutId, Vec<LayoutId>)
    {
        let child_layout_ids: Vec<_> = self.children
            .iter_mut()
            .map(|child| child.request_layout(window, cx).0)
            .collect();

        let layout_id = window.request_layout(
            Style {
                flex_direction: self.direction,
                gap: self.gap,
                ..default()
            },
            child_layout_ids.clone(),
            cx
        );

        (layout_id, child_layout_ids)
    }

    fn prepaint(&mut self, .., bounds: Bounds<Pixels>, layout_ids: &mut Vec<LayoutId>,
                window: &mut Window, cx: &mut App) -> Vec<Bounds<Pixels>>
    {
        let mut child_bounds = Vec::new();

        for (child, layout_id) in self.children.iter_mut().zip(layout_ids.iter()) {
            let child_bound = window.layout_bounds(*layout_id);
            child.prepaint(child_bound, window, cx);
            child_bounds.push(child_bound);
        }

        child_bounds
    }

    fn paint(&mut self, .., child_bounds: &mut Vec<Bounds<Pixels>>,
             window: &mut Window, cx: &mut App)
    {
        for (child, bounds) in self.children.iter_mut().zip(child_bounds.iter()) {
            child.paint(*bounds, window, cx);
        }
    }
}
```

### Use Cases

- Panels and split views
- List containers
- Grid layouts
- Tab containers

## Interactive Elements

Elements that respond to user input (mouse, keyboard, touch).

### Pattern Characteristics

- Create appropriate hitboxes for interaction areas
- Handle mouse/keyboard/touch events properly
- Manage focus and cursor styles
- Support hover, active, and disabled states

### Implementation Template

```rust
pub struct InteractiveElement {
    id: ElementId,
    content: AnyElement,
    on_click: Option<Box<dyn Fn(&MouseUpEvent, &mut Window, &mut App)>>,
    hover_style: Option<Style>,
}

impl Element for InteractiveElement {
    type RequestLayoutState = LayoutId;
    type PrepaintState = (Hitbox, bool); // hitbox and is_hovered

    fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
        -> (LayoutId, LayoutId)
    {
        let (content_layout, _) = self.content.request_layout(window, cx);
        (content_layout, content_layout)
    }

    fn prepaint(&mut self, .., bounds: Bounds<Pixels>, content_layout: &mut LayoutId,
                window: &mut Window, cx: &mut App) -> (Hitbox, bool)
    {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let is_hovered = hitbox.is_hovered(window);

        self.content.prepaint(bounds, window, cx);

        (hitbox, is_hovered)
    }

    fn paint(&mut self, .., bounds: Bounds<Pixels>, content_layout: &mut LayoutId,
             prepaint: &mut (Hitbox, bool), window: &mut Window, cx: &mut App)
    {
        let (hitbox, is_hovered) = prepaint;

        // Paint hover background if hovered
        if *is_hovered {
            if let Some(hover_style) = &self.hover_style {
                window.paint_quad(paint_quad(
                    bounds,
                    Anchor::all(px(4.)),
                    hover_style.background_color.unwrap_or(cx.theme().hover),
                ));
            }
        }

        // Paint content
        self.content.paint(bounds, window, cx);

        // Handle click
        if let Some(on_click) = self.on_click.as_ref() {
            window.on_mouse_event({
                let on_click = on_click.clone();
                let hitbox = hitbox.clone();
                move |event: &MouseUpEvent, phase, window, cx| {
                    if hitbox.is_hovered(window) && phase.bubble() {
                        on_click(event, window, cx);
                        cx.stop_propagation();
                    }
                }
            });
        }

        // Set cursor style
        window.set_cursor_style(CursorStyle::PointingHand, hitbox);
    }
}
```

### Use Cases

- Buttons
- Links
- Clickable cards
- Drag handles
- Menu items

## Composite Elements

Elements that combine multiple child elements with complex coordination.

### Pattern Characteristics

- Combine multiple child elements with different types
- Manage complex state across children
- Coordinate animations and transitions
- Handle focus delegation between children

### Implementation Template

```rust
pub struct CompositeElement {
    id: ElementId,
    header: AnyElement,
    content: AnyElement,
    footer: Option<AnyElement>,
}

struct CompositeLayoutState {
    header_layout: LayoutId,
    content_layout: LayoutId,
    footer_layout: Option<LayoutId>,
}

struct CompositePaintState {
    header_bounds: Bounds<Pixels>,
    content_bounds: Bounds<Pixels>,
    footer_bounds: Option<Bounds<Pixels>>,
}

impl Element for CompositeElement {
    type RequestLayoutState = CompositeLayoutState;
    type PrepaintState = CompositePaintState;

    fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
        -> (LayoutId, CompositeLayoutState)
    {
        let (header_layout, _) = self.header.request_layout(window, cx);
        let (content_layout, _) = self.content.request_layout(window, cx);
        let footer_layout = self.footer.as_mut()
            .map(|f| f.request_layout(window, cx).0);

        let mut children = vec![header_layout, content_layout];
        if let Some(footer) = footer_layout {
            children.push(footer);
        }

        let layout_id = window.request_layout(
            Style {
                flex_direction: FlexDirection::Column,
                size: Size {
                    width: relative(1.0),
                    height: auto(),
                },
                ..default()
            },
            children,
            cx
        );

        (layout_id, CompositeLayoutState {
            header_layout,
            content_layout,
            footer_layout,
        })
    }

    fn prepaint(&mut self, .., bounds: Bounds<Pixels>, layout: &mut CompositeLayoutState,
                window: &mut Window, cx: &mut App) -> CompositePaintState
    {
        let header_bounds = window.layout_bounds(layout.header_layout);
        let content_bounds = window.layout_bounds(layout.content_layout);
        let footer_bounds = layout.footer_layout
            .map(|id| window.layout_bounds(id));

        self.header.prepaint(header_bounds, window, cx);
        self.content.prepaint(content_bounds, window, cx);
        if let (Some(footer), Some(bounds)) = (&mut self.footer, footer_bounds) {
            footer.prepaint(bounds, window, cx);
        }

        CompositePaintState {
            header_bounds,
            content_bounds,
            footer_bounds,
        }
    }

    fn paint(&mut self, .., paint_state: &mut CompositePaintState,
             window: &mut Window, cx: &mut App)
    {
        self.header.paint(paint_state.header_bounds, window, cx);
        self.content.paint(paint_state.content_bounds, window, cx);
        if let (Some(footer), Some(bounds)) = (&mut self.footer, paint_state.footer_bounds) {
            footer.paint(bounds, window, cx);
        }
    }
}
```

### Use Cases

- Dialog boxes (header + content + footer)
- Cards with multiple sections
- Form layouts
- Panels with toolbars

## Scrollable Elements

Elements with scrollable content areas.

### Pattern Characteristics

- Manage scroll state (offset, velocity)
- Handle scroll events (wheel, drag, touch)
- Paint scrollbars (track and thumb)
- Clip content to visible area

### Implementation Template

```rust
pub struct ScrollableElement {
    id: ElementId,
    content: AnyElement,
    scroll_offset: Point<Pixels>,
    content_size: Size<Pixels>,
}

struct ScrollPaintState {
    hitbox: Hitbox,
    visible_bounds: Bounds<Pixels>,
}

impl Element for ScrollableElement {
    type RequestLayoutState = (LayoutId, Size<Pixels>);
    type PrepaintState = ScrollPaintState;

    fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
        -> (LayoutId, (LayoutId, Size<Pixels>))
    {
        let (content_layout, _) = self.content.request_layout(window, cx);
        let content_size = window.layout_bounds(content_layout).size;

        let layout_id = window.request_layout(
            Style {
                size: Size {
                    width: relative(1.0),
                    height: px(400.), // Fixed viewport height
                },
                overflow: Overflow::Hidden,
                ..default()
            },
            vec![content_layout],
            cx
        );

        (layout_id, (content_layout, content_size))
    }

    fn prepaint(&mut self, .., bounds: Bounds<Pixels>, layout: &mut (LayoutId, Size<Pixels>),
                window: &mut Window, cx: &mut App) -> ScrollPaintState
    {
        let (content_layout, content_size) = layout;

        // Calculate content bounds with scroll offset
        let content_bounds = Bounds::new(
            point(bounds.left(), bounds.top() - self.scroll_offset.y),
            *content_size
        );

        self.content.prepaint(content_bounds, window, cx);

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        ScrollPaintState {
            hitbox,
            visible_bounds: bounds,
        }
    }

    fn paint(&mut self, .., layout: &mut (LayoutId, Size<Pixels>),
             paint_state: &mut ScrollPaintState, window: &mut Window, cx: &mut App)
    {
        let (_, content_size) = layout;

        // Paint content
        self.content.paint(paint_state.visible_bounds, window, cx);

        // Paint scrollbar
        self.paint_scrollbar(paint_state.visible_bounds, *content_size, window, cx);

        // Handle scroll events
        window.on_mouse_event({
            let hitbox = paint_state.hitbox.clone();
            let content_height = content_size.height;
            let visible_height = paint_state.visible_bounds.size.height;

            move |event: &ScrollWheelEvent, phase, window, cx| {
                if hitbox.is_hovered(window) && phase.bubble() {
                    // Update scroll offset
                    self.scroll_offset.y -= event.delta.y;

                    // Clamp to valid range
                    let max_scroll = (content_height - visible_height).max(px(0.));
                    self.scroll_offset.y = self.scroll_offset.y
                        .max(px(0.))
                        .min(max_scroll);

                    cx.notify();
                    cx.stop_propagation();
                }
            }
        });
    }
}

impl ScrollableElement {
    fn paint_scrollbar(
        &self,
        bounds: Bounds<Pixels>,
        content_size: Size<Pixels>,
        window: &mut Window,
        cx: &mut App
    ) {
        let visible_height = bounds.size.height;
        let content_height = content_size.height;

        if content_height <= visible_height {
            return; // No scrollbar needed
        }

        let scrollbar_width = px(8.);

        // Calculate thumb position and size
        let scroll_ratio = self.scroll_offset.y / (content_height - visible_height);
        let thumb_height = (visible_height / content_height) * visible_height;
        let thumb_y = scroll_ratio * (visible_height - thumb_height);

        // Paint track
        window.paint_quad(paint_quad(
            Bounds::new(
                point(bounds.right() - scrollbar_width, bounds.top()),
                size(scrollbar_width, visible_height)
            ),
            Anchor::default(),
            cx.theme().scrollbar_track,
        ));

        // Paint thumb
        window.paint_quad(paint_quad(
            Bounds::new(
                point(bounds.right() - scrollbar_width, bounds.top() + thumb_y),
                size(scrollbar_width, thumb_height)
            ),
            Anchor::all(px(4.)),
            cx.theme().scrollbar_thumb,
        ));
    }
}
```

### Use Cases

- Scrollable lists
- Code editors with large files
- Long-form text content
- Image galleries

## Pattern Selection Guide

| Need | Pattern | Complexity |
|------|---------|------------|
| Display styled text | Text Rendering | Low |
| Layout multiple children | Container | Low-Medium |
| Handle clicks/hovers | Interactive | Medium |
| Complex multi-part UI | Composite | Medium-High |
| Large content with scrolling | Scrollable | High |

Choose the simplest pattern that meets your requirements, then extend as needed.
