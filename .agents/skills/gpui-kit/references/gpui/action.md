<!-- OpenLogi modification: normalized trailing whitespace and final newlines. -->

# Actions & Keybindings

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Key Formats](#key-formats) · [Action Naming](#action-naming) · [Context-Aware Bindings](#context-aware-bindings) · [Best Practices](#best-practices)

## Overview

Actions provide declarative keyboard-driven UI interactions in GPUI.

**Key Concepts:**
- Define actions with `actions!` macro or `#[derive(Action)]`
- Bind keys with `cx.bind_keys()`
- Handle with `.on_action()` on elements
- Context-aware via `key_context()`

## Quick Start

### Simple Actions

```rust
use gpui_kit::actions;

actions!(editor, [MoveUp, MoveDown, Save, Quit]);

const CONTEXT: &str = "Editor";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", MoveUp, Some(CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(CONTEXT)),
        KeyBinding::new("cmd-s", Save, Some(CONTEXT)),
        KeyBinding::new("cmd-q", Quit, Some(CONTEXT)),
    ]);
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::save))
    }
}

impl Editor {
    fn move_up(&mut self, _: &MoveUp, cx: &mut Context<Self>) {
        // Handle move up
        cx.notify();
    }

    fn move_down(&mut self, _: &MoveDown, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn save(&mut self, _: &Save, cx: &mut Context<Self>) {
        // Save logic
        cx.notify();
    }
}
```

### Actions with Parameters

```rust
#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = editor)]
pub struct InsertText {
    pub text: String,
}

#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = editor, no_json)]
pub struct Digit(pub u8);

cx.bind_keys([
    KeyBinding::new("0", Digit(0), Some(CONTEXT)),
    KeyBinding::new("1", Digit(1), Some(CONTEXT)),
    // ...
]);

impl Editor {
    fn on_digit(&mut self, action: &Digit, cx: &mut Context<Self>) {
        self.insert_digit(action.0, cx);
    }
}
```

## Key Formats

```rust
// Modifiers
"cmd-s"         // Command (macOS) / Ctrl (Windows/Linux)
"ctrl-c"        // Control
"alt-f"         // Alt
"shift-tab"     // Shift
"cmd-ctrl-f"    // Multiple modifiers

// Keys
"a-z", "0-9"    // Letters and numbers
"f1-f12"        // Function keys
"up", "down", "left", "right"
"enter", "escape", "space", "tab"
"backspace", "delete"
"-", "=", "[", "]", etc.  // Special characters
```

## Action Naming

Prefer verb-noun pattern:

```rust
actions!([
    OpenFile,      // ✅ Good
    CloseWindow,   // ✅ Good
    ToggleSidebar, // ✅ Good
    Save,          // ✅ Good (common exception)
]);
```

## Context-Aware Bindings

```rust
const EDITOR_CONTEXT: &str = "Editor";
const MODAL_CONTEXT: &str = "Modal";

// Same key, different contexts
cx.bind_keys([
    KeyBinding::new("escape", CloseModal, Some(MODAL_CONTEXT)),
    KeyBinding::new("escape", ClearSelection, Some(EDITOR_CONTEXT)),
]);

// Set context on element
div()
    .key_context(EDITOR_CONTEXT)
    .child(editor_content)
```

## Best Practices

### ✅ Use Contexts

```rust
// ✅ Good: Context-aware
div()
    .key_context("MyComponent")
    .on_action(cx.listener(Self::handle))
```

### ✅ Name Actions Clearly

```rust
// ✅ Good: Clear intent
actions!([
    SaveDocument,
    CloseTab,
    TogglePreview,
]);
```

### ✅ Handle with Listeners

```rust
// ✅ Good: Proper handler naming
impl MyComponent {
    fn on_action_save(&mut self, _: &Save, cx: &mut Context<Self>) {
        // Handle save
        cx.notify();
    }
}

div().on_action(cx.listener(Self::on_action_save))
```
