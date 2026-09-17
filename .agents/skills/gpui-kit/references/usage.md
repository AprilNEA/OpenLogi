# gpui-kit Usage Guide

**Contents:** [Setup](#setup) · [Component Types](#component-types) · [Common Components](#common-components) (Button, Input, Select, Checkbox, Icon, Dialog, Notification, Tabs, Tooltip, Form, List) · [Theming](#theming) · [Layout Helpers](#layout-helpers) · [Overlay Layers](#overlay-layers-dialogs-sheets-notifications) · [Shared Traits](#shared-traits)

## Setup

Use the [tested application recipe](recipes.md) for complete examples and their verification command. Store subscription handles on the owning view; binding one to a constructor-local variable alone does not keep it alive after construction.

### 1. Cargo.toml

```toml
[dependencies]
gpui-kit = "0.6" # re-exports GPUI, platform, base, component and the default icons; Shell is a separate host dependency
```

### 2. Initialization

```rust
fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx); // MUST be first

            cx.spawn(async move |cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    let view = cx.new(|_| MyApp);
                    cx.new(|cx| Root::new(view, window, cx)) // Root wraps first view
                }).expect("Failed to open window");
            }).detach();
        });
}
```

**`Root` is required** as the first-level child of every window — it enables dialogs, sheets, and notifications.

---

## Component Types

### Stateless (most components)

Used directly in `render`, no stored state:

```rust
use gpui_kit::component::button::Button;

impl Render for MyView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Button::new("btn").primary().label("Submit")
            .on_click(|_, _, _| println!("clicked"))
    }
}
```

### Stateful (Input, Select, Combobox, etc.)

Require an `Entity<State>` stored in your view:

```rust
use gpui_kit::component::input::{Input, InputState};

struct MyView {
    name: Entity<InputState>,
}

impl MyView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Your name")),
        }
    }
}

impl Render for MyView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Input::new(&self.name)
    }
}
```

---

## Common Components

### Button

```rust
use gpui_kit::component::button::{Button, ButtonGroup};

// Variants
Button::new("btn").label("Default")
Button::new("btn").primary().label("Primary")
Button::new("btn").danger().label("Delete")
Button::new("btn").warning().label("Warning")
Button::new("btn").success().label("Success")
Button::new("btn").ghost().label("Ghost")
Button::new("btn").link().label("Link")

// States
Button::new("btn").label("Text").disabled(true)
Button::new("btn").label("Text").loading(true)
Button::new("btn").label("Text").selected(true)

// With icon
Button::new("btn").icon(IconName::Plus).label("Add")

// Sizes
Button::new("btn").xsmall().label("XS")
Button::new("btn").small().label("S")
Button::new("btn").large().label("L")

// Group
ButtonGroup::new("group")
    .child(Button::new("a").label("A"))
    .child(Button::new("b").label("B"))
    .on_click(|indices, _, _| { /* selected indices */ })
```

### Input

```rust
use gpui_kit::component::input::{Input, InputState};

// State setup (in new/init)
let input = cx.new(|cx| InputState::new(window, cx)
    .placeholder("Enter text...")
    .default_value("Hello")
);

// Render
Input::new(&input)
Input::new(&input).cleanable(true)           // clear button
Input::new(&input).disabled(true)
Input::new(&input).prefix(Icon::new(IconName::Search).small())
Input::new(&input).suffix(Button::new("b").ghost().icon(IconName::X).xsmall())
Input::new(&input).content_type(InputContentType::Password)
Input::new(&input).mask_toggle()             // password reveal toggle
Input::new(&input).appearance(false)         // remove default border/bg

// Reading value
let value = input.read(cx).value();

// Keep this subscription in the owning view (for example, `_subscriptions`).
self._subscriptions.push(cx.subscribe_in(&input, window, |view, state, event, window, cx| {
    match event {
        InputEvent::Change => { let v = state.read(cx).value(); }
        InputEvent::PressEnter { .. } => { /* submit */ }
        InputEvent::Focus | InputEvent::Blur => {}
    }
}));
```

### Select

```rust
use gpui_kit::component::select::{Select, SelectState};

// Simple string list
let state = cx.new(|cx| {
    SelectState::new(vec!["Apple", "Orange", "Banana"], Some(IndexPath::default()), window, cx)
});

// Render
Select::new(&state)
Select::new(&state).placeholder("Pick one")

// Reading selection
let selected = state.read(cx).selected_item();
```

### Checkbox / Switch / Radio

```rust
use gpui_kit::component::{checkbox::Checkbox, switch::Switch};

// Stateless (controlled)
Checkbox::new("cb").checked(self.checked)
    .on_change(cx.listener(|this, checked, _, cx| {
        this.checked = *checked;
        cx.notify();
    }))

Switch::new("sw").checked(self.enabled)
    .on_change(cx.listener(|this, checked, _, cx| {
        this.enabled = *checked;
        cx.notify();
    }))
```

### Icon

```rust
use gpui_kit::component::{Icon, IconName};

Icon::new(IconName::Check)
Icon::new(IconName::Search).small()
Icon::new(IconName::Plus).large().text_color(cx.theme().primary)
```

### Dialog

```rust
use gpui_kit::component::dialog::{Dialog, DialogAction, DialogClose, DialogFooter};

// Open from window context. `footer` takes an element, not a closure.
// DialogClose dismisses the dialog, so no manual close call is needed.
window.open_dialog(cx, |dialog, _, _| {
    dialog
        .title("Export Report")
        .child("Choose a destination for the exported file.")
        .footer(
            DialogFooter::new()
                .gap_2()
                .child(DialogClose::new().child(
                    Button::new("cancel").label("Cancel").outline(),
                ))
                .child(DialogAction::new().child(
                    Button::new("export").label("Export").primary(),
                )),
        )
});
```

### AlertDialog

Use `AlertDialog` — not `Dialog` — to confirm a consequential action. It is not
overlay-closable and has no close button, so the choice must be made. Name the
object in the title and the result on the confirming button; see the Design
Guides for the copy rules.

```rust
use gpui_kit::component::{button::ButtonVariant, dialog::DialogButtonProps};

window.open_alert_dialog(cx, |alert, _, _| {
    alert
        .title("Remove “Roadmap”?")
        .description("Files on disk aren’t deleted.")
        .button_props(
            DialogButtonProps::default()
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(|_, _, _| true),
        )
});
```

### Notification

```rust
// Simple string message
window.push_notification("Saved successfully!", cx);

// With type variant
window.push_notification(
    Notification::new("Upload complete").info().message("File uploaded"),
    cx,
);
```

### Tabs

```rust
use gpui_kit::component::tab::{Tab, TabBar};

TabBar::new("tabs")
    .child(Tab::new("tab1").child("Overview"))
    .child(Tab::new("tab2").child("Settings"))
    .child(Tab::new("tab3").child("Logs"))
```

### Tooltip

```rust
// On any element with .id(), add .tooltip():
div()
    .id("my-btn")
    .tooltip(|window, cx| Tooltip::new("Delete item").build(window, cx))
    .child("Delete")

// Or on a Button directly:
Button::new("btn").icon(IconName::Trash).tooltip("Delete")
```

### Form

```rust
use gpui_kit::component::form::{v_form, h_form, field};

// Vertical form
v_form()
    .child(field().label("Name").child(Input::new(&self.name)))
    .child(field().label("Email").child(Input::new(&self.email)))
    .footer(Button::new("submit").primary().label("Submit"))

// Horizontal label alignment
h_form()
    .child(field().label("Username").child(Input::new(&self.username)))
```

### List (searchable, virtualized)

```rust
use gpui_kit::component::list::{List, ListState, ListDelegate, ListItem, ListEvent};

// Implement ListDelegate for your data type, then:
let list_state = cx.new(|cx| ListState::new(MyDelegate::new(), window, cx));

// Render
List::new(&list_state)
// Keep this subscription in the owning view, alongside list_state.
self._subscriptions.push(cx.subscribe(&list_state, |this, _, event, cx| {
    if let ListEvent::Select(index_path) = event {
        // handle selection
    }
}));
```

---

## Theming

```rust
use gpui_kit::component::ActiveTheme as _;

// Access colors
cx.theme().primary
cx.theme().background
cx.theme().foreground
cx.theme().border
cx.theme().surface
cx.theme().muted
cx.theme().destructive

// Use in styles
div()
    .bg(cx.theme().surface)
    .text_color(cx.theme().foreground)
    .border_color(cx.theme().border)
```

### Switch Theme

```rust
use gpui_kit::component::Theme;

// Toggle light/dark
cx.update_global::<Theme, _>(|theme, cx| {
    theme.toggle_mode(cx);
});

// Load a named theme
Theme::global_mut(cx).apply_config(&theme_config);
```

---

## Layout Helpers

`gpui_kit::component` extends GPUI with convenient layout methods:

```rust
h_flex()    // div().flex().flex_row().items_center()
v_flex()    // div().flex().flex_col()

// Common patterns
h_flex().gap_2().items_center()
    .child(Icon::new(IconName::User))
    .child(label("Username"))

v_flex().gap_4().p_4()
    .child(Input::new(&self.name))
    .child(Input::new(&self.email))
    .child(Button::new("submit").primary().label("Submit"))
```

---

## Overlay Layers (Dialogs, Sheets, Notifications)

To render overlays, add these to your first-level view's render:

```rust
impl Render for MyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(self.main_content(window, cx))
            .children(Root::render_dialog_layer(cx))
            .children(Root::render_sheet_layer(cx))
            .children(Root::render_notification_layer(cx))
    }
}
```

---

## Shared Traits

Builders return the component so methods can be chained. Constructors follow component families: controls take a stable ID, retained controls take state, and compound parts may take no arguments. See [component conventions](conventions.md).

- `Sizable`: `.xsmall()` / `.small()` / `.medium()` (default) / `.large()`
- `Disableable`: `.disabled(bool)`
- `Selectable`: `.selected(bool)`
- `Styled`: any GPUI style methods (`.w()`, `.bg()`, `.p_2()`, etc.)

For any component not covered here, fetch its doc from:
`https://gpui-kit.com/component/{name}.md`
