# Function-Key Remapper — M1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture F1–F12 + Esc key presses (and Shift/Ctrl/Opt/Cmd-qualified combos) and remap each to any action in the existing palette, plus three new execution actions (`TypeText`, `RunAppleScript`, `RunShellCommand`).

**Architecture:** Three layers, each mirroring the mouse path. (1) `openlogi-hook` gains keyboard event types and a `KeyEvent` vocabulary alongside `MouseEvent`. (2) `openlogi-core` gains three `Action` variants and a `[keyboard.bindings]` config section keyed by keycode+modifiers. (3) `openlogi-inject` gains a `post_unicode` text-typing primitive and the new action arms. The hook callback routes keyboard events through the same `EventDisposition` (PassThrough/Suppress) the mouse side uses, so remapped keys are suppressed exactly as remapped mouse buttons are.

**Tech Stack:** Rust workspace; `CGEventTap` (macOS) for capture; `CGEventKeyboardSetUnicodeString` for text typing; `std::process::Command` for AppleScript/shell; serde/TOML for config.

**Spec:** `docs/superpowers/specs/2026-06-30-function-key-remapper-design.md` (M1 scope only; M2 Workflow and M3 media-key capture are separate plans).

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/openlogi-hook/src/lib.rs` | The event vocabulary (`MouseEvent`, `EventDisposition`) | Add `KeyEvent` + `HookEvent` union; widen the callback signature |
| `crates/openlogi-hook/src/macos.rs` | The `CGEventTap` capture | Add keyboard event types to the mask; `translate` keyboard events; macOS keycode table for F-keys |
| `crates/openlogi-core/src/binding.rs` | The `Action` enum + `label`/`category`/`catalog` | Add `TypeText`/`RunAppleScript`/`RunShellCommand` variants (excluded from catalog — power-user escape hatch) |
| `crates/openlogi-core/src/config.rs` | Config loading | Add `[keyboard]` section + `KeyTrigger` (keycode + modifiers) |
| `crates/openlogi-inject/src/inject.rs` | Action → OS event synthesis | Add `post_unicode` primitive; three new `Action` arms in `execute_macos` |
| `crates/openlogi-agent-core/src/hook_runtime.rs` | Dispatches hook events → actions | Route `KeyEvent` → look up keyboard binding → execute action → Suppress |

No new files except where a table cell says "Add". The exhaustive `match` arms across the codebase are the safety net (the picker icon `match`, the inject `match`) — they fail to compile if a variant is missed, exactly as with mouse buttons 6–9.

---

## Task 1: Add the `KeyEvent` vocabulary and widen the hook callback

This is the foundational change: the hook must be able to report keyboard events, not just mouse. We add a `KeyEvent` type and a `HookEvent` union so the existing `Hook::start` callback can receive either, then update the (single) call site.

**Files:**
- Modify: `crates/openlogi-hook/src/lib.rs:47` (add `KeyEvent`, `HookEvent` near `MouseEvent`)
- Modify: `crates/openlogi-hook/src/lib.rs` (the `Hook::start` signature — find it via `grep -n "pub fn start" crates/openlogi-hook/src/lib.rs`)
- Modify: `crates/openlogi-agent-core/src/hook_runtime.rs:115` (the single call site)

- [ ] **Step 1: Read the current `MouseEvent` + `Hook::start` signature**

Run: `sed -n '40,100p' crates/openlogi-hook/src/lib.rs && grep -n "pub fn start" crates/openlogi-hook/src/lib.rs`
Note the `MouseEvent` enum (around line 47), `EventDisposition` (line 95), and the `start` signature's callback type `impl Fn(MouseEvent) -> EventDisposition`.

- [ ] **Step 2: Add `KeyEvent` + `KeyModifiers` + `HookEvent` to `lib.rs`**

Immediately above `pub enum MouseEvent {` (line 47), add:

```rust
/// Which modifier keys were held when a key event fired. Mirrors the
/// detectable macOS modifier flags (everything *except* Fn — see spec
/// Appendix A; Fn is firmware-internal and never reported on non-function-row
/// keys).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
}

/// A keyboard event observed by the hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// macOS virtual keycode (e.g. 122 = F1, 53 = Escape).
    pub keycode: u16,
    /// `true` = key down; `false` = key up.
    pub pressed: bool,
    /// Which modifiers were held.
    pub modifiers: KeyModifiers,
}

/// Anything the hook can observe. `Mouse` keeps the existing callback shape;
/// `Key` is the new keyboard path. Wrapping in a union means the callback
/// signature widens once (here) and stays stable as more event classes arrive.
#[derive(Debug, Clone, Copy)]
pub enum HookEvent {
    Mouse(MouseEvent),
    Key(KeyEvent),
}
```

- [ ] **Step 3: Widen the `Hook::start` callback to `HookEvent`**

Find `pub fn start(` in `lib.rs`. Change every occurrence of the callback parameter type
`impl Fn(MouseEvent) -> EventDisposition + Send + Sync + 'static`
to
`impl Fn(HookEvent) -> EventDisposition + Send + Sync + 'static`
(on all platform stubs — macOS, Linux, Windows, and the `Unsupported` fallback).

- [ ] **Step 4: Update the single call site in `hook_runtime.rs:115`**

The callback currently matches `MouseEvent` variants directly. Wrap the existing body
to only act on `HookEvent::Mouse` and pass through keys for now:

```rust
let result = Hook::start(move |event| match event {
    HookEvent::Mouse(mouse_event) => match mouse_event {
        MouseEvent::Button { id, pressed } => {
            // ... existing body unchanged ...
        }
        MouseEvent::Moved { delta_x, delta_y } => {
            // ... existing body unchanged ...
        }
        MouseEvent::CaptureInterrupted => {
            // ... existing body unchanged ...
        }
        MouseEvent::Scroll { .. } => EventDisposition::PassThrough,
    },
    HookEvent::Key(_) => EventDisposition::PassThrough, // wired up in Task 6
});
```

Add the import: `use openlogi_hook::{EventDisposition, Hook, HookEvent, MouseEvent};`

- [ ] **Step 5: Build + run the full hook + agent-core tests**

Run: `cargo test -p openlogi-hook -p openlogi-agent-core`
Expected: PASS. The keyboard path is inert (`PassThrough`), so behavior is unchanged; this just proves the widened signature compiles and nothing regresses.

- [ ] **Step 6: Commit**

```bash
git add crates/openlogi-hook/src/lib.rs crates/openlogi-agent-core/src/hook_runtime.rs
git commit -m "refactor(hook): widen hook callback to HookEvent (Mouse | Key)

Adds KeyEvent + KeyModifiers + HookEvent vocabulary alongside MouseEvent.
Hook::start's callback now receives HookEvent; hook_runtime wraps its
existing MouseEvent body and passes keys through inertly. No behavior
change yet — keyboard capture lands in the next task."
```

---

## Task 2: Capture keyboard events in the macOS `CGEventTap`

Extend the existing tap (currently mouse-only) to also subscribe to keyboard event types, and translate them into `KeyEvent`s. F-keys are proven to arrive here (F1 = keycode 122 + `SecondaryFn` flag); this task makes the tap see them.

**Files:**
- Modify: `crates/openlogi-hook/src/macos.rs:452` (the `event_types` vec)
- Modify: `crates/openlogi-hook/src/macos.rs:257` (`translate` — add keyboard arms) and the callback closure at `:475`

- [ ] **Step 1: Add keyboard event types to the tap mask**

In `macos.rs`, the `event_types` vec (line 452) currently lists only mouse types. Append:

```rust
    let event_types = vec![
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        // ... existing mouse types unchanged ...
        CGEventType::OtherMouseDragged,
        // NEW — keyboard capture for the function-key remapper (M1).
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
    ];
```

- [ ] **Step 2: Add a keyboard-translation helper**

Above the existing `fn translate(...)` (line 257), add:

```rust
/// Map the macOS modifier flags on a `CGEvent` to our [`KeyModifiers`].
/// `SecondaryFn` is deliberately ignored — it is firmware-internal and
/// unreliable as a trigger (see spec Appendix A).
fn modifiers_from_flags(flags: CGEventFlags) -> KeyModifiers {
    KeyModifiers {
        shift: flags.contains(CGEventFlags::MASK_SHIFT),
        control: flags.contains(CGEventFlags::MASK_CONTROL),
        option: flags.contains(CGEventFlags::MASK_ALTERNATE),
        command: flags.contains(CGEventFlags::MASK_COMMAND),
    }
}

/// Translate a keyboard `CGEvent` into a [`KeyEvent`]. Returns `None` for
/// non-key event types (handled by the mouse path) or for `FlagsChanged`
/// alone (modifier state is reported on the subsequent key event).
fn translate_key(etype: CGEventType, event: &CGEvent) -> Option<KeyEvent> {
    let (pressed, keycode) = match etype {
        CGEventType::KeyDown => (true, event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16),
        CGEventType::KeyUp => (false, event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16),
        // FlagsChanged carries no keycode of interest here; modifiers ride on
        // the next key event via its flags. Drop it.
        _ => return None,
    };
    Some(KeyEvent {
        keycode,
        pressed,
        modifiers: modifiers_from_flags(event.get_flags()),
    })
}
```

(Add `use core_graphics::event::EventField;` to the imports if not already present — check with `grep -n "use core_graphics" crates/openlogi-hook/src/macos.rs | head`.)

- [ ] **Step 3: Route keyboard events through the callback**

The callback closure (line 475) currently does `let Some(mouse_event) = translate(etype, event)`. Replace it to build a `HookEvent` from either path:

```rust
        move |_proxy: CGEventTapProxy, etype: CGEventType, event: &CGEvent| {
            let hook_event = if let Some(mouse_event) = translate(etype, event) {
                HookEvent::Mouse(mouse_event)
            } else if let Some(key_event) = translate_key(etype, event) {
                HookEvent::Key(key_event)
            } else {
                return CallbackResult::Keep;
            };
            match cb(hook_event) {
                EventDisposition::PassThrough => CallbackResult::Keep,
                EventDisposition::Suppress => CallbackResult::Drop,
            }
        },
```

- [ ] **Step 4: Build the hook crate**

Run: `cargo build -p openlogi-hook`
Expected: BUILD SUCCEEDS. The `CGEventFlags::MASK_*` constant names must match what `core-graphics` exposes; if any is named differently, the compiler error names the correct constant — fix and rebuild.

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-hook/src/macos.rs
git commit -m "feat(hook): capture keyboard events in the macOS CGEventTap

Adds KeyDown/KeyUp/FlagsChanged to the tap mask, plus translate_key()
which maps a key CGEvent to our KeyEvent (keycode + press state +
detectable modifiers, ignoring SecondaryFn). The callback now builds a
HookEvent from either the mouse or key path. F1-F12/Esc are now observed;
nothing acts on them yet."
```

---

## Task 3: Add the three execution `Action` variants

The action palette gains `TypeText`, `RunAppleScript`, `RunShellCommand`. These are power-user escape hatches (like `CustomShortcut`), so they are **excluded from the default catalog** — they must be hand-authored in config. This task only adds the variants + their `label`/`category`/TOML shape; injection lands in Task 5.

**Files:**
- Modify: `crates/openlogi-core/src/binding.rs` — `Action` enum (near `CustomShortcut(KeyCombo)` at line 483), `label()` (:679), `category()` (:731), `catalog()` (:787)

- [ ] **Step 1: Add the failing test for the new variants' category + label**

In `binding.rs`, append to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn power_user_action_labels_and_category() {
        assert_eq!(Action::TypeText("hi".into()).label(), "Type \"hi\"");
        assert_eq!(Action::RunAppleScript("osascript".into()).label(), "Run AppleScript");
        assert_eq!(Action::RunShellCommand("echo hi".into()).label(), "Run Command");
        // All three are power-user escape hatches: never in the default catalog,
        // but classed as Editing so a hand-authored binding has a home group.
        assert_eq!(Action::TypeText("x".into()).category(), Category::Editing);
        assert_eq!(Action::RunAppleScript("x".into()).category(), Category::Editing);
        assert_eq!(Action::RunShellCommand("x".into()).category(), Category::Editing);
    }

    #[test]
    fn power_user_actions_excluded_from_catalog() {
        let cat = Action::catalog();
        assert!(cat.iter().all(|a| !matches!(a,
            Action::TypeText(_) | Action::RunAppleScript(_) | Action::RunShellCommand(_))));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p openlogi-core --lib binding::tests::power_user`
Expected: FAIL with `cannot find variant TypeText` — compile error.

- [ ] **Step 3: Add the three variants to the `Action` enum**

After `CustomShortcut(KeyCombo),` (line 483), add:

```rust
    /// Type an arbitrary string by emitting unicode characters (macOS
    /// `CGEventKeyboardSetUnicodeString`). Used for macro text. Power-user
    /// escape hatch — excluded from the default catalog.
    TypeText(String),
    /// Run an AppleScript via `osascript -e <source>`. Power-user escape hatch.
    RunAppleScript(String),
    /// Run a shell command via `/bin/sh -c <command>`. Power-user escape hatch.
    RunShellCommand(String),
```

- [ ] **Step 4: Add the three labels**

In `label()` (:679), in the `match` (after the `CustomShortcut` arm), add:

```rust
            Action::TypeText(s) => format!("Type \"{s}\"").into(),
            Action::RunAppleScript(_) => "Run AppleScript".into(),
            Action::RunShellCommand(_) => "Run Command".into(),
```

- [ ] **Step 5: Add the three category arms**

In `category()` (:731), extend the existing `Editing` arm that already holds
`CustomShortcut`:

```rust
            | Action::CustomShortcut(_)
            | Action::TypeText(_)
            | Action::RunAppleScript(_)
            | Action::RunShellCommand(_) => Category::Editing,
```

- [ ] **Step 6: Confirm `catalog()` excludes them**

`catalog()` (:787) is an explicit list — by NOT adding the three variants to it,
they are excluded. Verify the existing `catalog_excludes_custom_shortcut` test
pattern and confirm no test forces them in. No code change needed here; the
test in Step 1 asserts exclusion.

- [ ] **Step 7: Run the tests to verify they pass + the TOML roundtrip still works**

Run: `cargo test -p openlogi-core --lib binding`
Expected: PASS — including the new tests and the existing
`all_catalog_variants_roundtrip_toml` (the new variants aren't in the catalog,
but add a manual roundtrip assertion for `TypeText` in the test block):

```rust
    #[test]
    fn power_user_actions_roundtrip_toml() {
        for action in [
            Action::TypeText("hello".into()),
            Action::RunAppleScript("beep".into()),
            Action::RunShellCommand("date".into()),
        ] {
            let toml = toml::to_string(&action).unwrap();
            let back: Action = toml::from_str(&toml).unwrap();
            assert_eq!(action, back);
        }
    }
```

- [ ] **Step 8: Commit**

```bash
git add crates/openlogi-core/src/binding.rs
git commit -m "feat(core): add TypeText / RunAppleScript / RunShellCommand actions

Three power-user escape-hatch actions (excluded from the default catalog,
classed as Editing). TypeText emits a unicode string; the two Run actions
spawn osascript / sh. Injection arms land in the inject task."
```

---

## Task 4: Add the `[keyboard]` config section + `KeyTrigger`

The config gains a new top-level `[keyboard]` table mapping trigger strings (`"f1"`, `"shift+f1"`) to actions. This is independent of the per-device `[devices]` bindings.

**Files:**
- Modify: `crates/openlogi-core/src/config.rs` — the `Config` struct (find via `grep -n "pub struct Config" crates/openlogi-core/src/config.rs`)
- Create: the `KeyTrigger` type + trigger-string parser lives in `config.rs` (single file, follow existing patterns)

- [ ] **Step 1: Read the current `Config` struct + a device-binding sample**

Run: `sed -n '/pub struct Config/,/^}/p' crates/openlogi-core/src/config.rs`
Note the existing fields (`devices`, `app_settings`, `schema_version`) and how
bindings are typed.

- [ ] **Step 2: Write the failing test for the trigger-string parser + config load**

Append to `config.rs`'s test module:

```rust
    #[test]
    fn key_trigger_parses_bare_and_modified() {
        // Bare function key.
        let t: KeyTrigger = "f1".parse().unwrap();
        assert_eq!(t.keycode, 122);
        assert!(t.modifiers.is_empty());
        // Modifier-qualified.
        let t: KeyTrigger = "shift+cmd+f5".parse().unwrap();
        assert_eq!(t.keycode, 96); // F5
        assert!(t.modifiers.shift && t.modifiers.command);
        assert!(!t.modifiers.control && !t.modifiers.option);
    }

    #[test]
    fn keyboard_section_loads_from_toml() {
        let toml = r#"
[keyboard.bindings]
"f1" = { TypeText = "hi" }
"shift+f2" = "VolumeUp"
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.keyboard.bindings.len(), 2);
        assert!(cfg.keyboard.bindings.contains_key(&"f1".parse::<KeyTrigger>().unwrap()));
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p openlogi-core --lib config::`
Expected: FAIL — `KeyTrigger` and `keyboard` field don't exist.

- [ ] **Step 4: Add `KeyTrigger` + the parser + `KeyboardConfig`**

In `config.rs`, add (types first, then impls):

```rust
use std::str::FromStr;

/// A keyboard trigger: a keycode plus an optional modifier mask. The parse
/// format is `[mod+]+key`, e.g. `"f1"`, `"shift+cmd+f5"`. Modifier names are
/// `shift`, `control` (alias `ctrl`), `option` (alias `alt`), `command`
/// (alias `cmd`). Key names: `esc`, `f1`..`f12`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyTrigger {
    pub keycode: u16,
    pub modifiers: KeyModifiers,
}

impl KeyModifiers {
    pub fn is_empty(&self) -> bool {
        !self.shift && !self.control && !self.option && !self.command
    }
}

#[derive(Debug, Default)]
pub struct ParseTriggerError(String);
impl std::fmt::Display for ParseTriggerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid key trigger: {}", self.0)
    }
}
impl std::error::Error for ParseTriggerError {}

impl FromStr for KeyTrigger {
    type Err = ParseTriggerError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = KeyModifiers::default();
        let mut parts = s.split('+').map(str::trim);
        let mut last: Option<&str> = None;
        for part in parts.by_ref() {
            match part.to_ascii_lowercase().as_str() {
                "shift" => mods.shift = true,
                "control" | "ctrl" => mods.control = true,
                "option" | "alt" => mods.option = true,
                "command" | "cmd" => mods.command = true,
                _ => { last = Some(part); break; }
            }
        }
        let key = last.or_else(|| parts.next()).ok_or_else(|| ParseTriggerError("no key".into()))?;
        let keycode = match key.to_ascii_lowercase().as_str() {
            "esc" => 53,
            "f1" => 122, "f2" => 120, "f3" => 99, "f4" => 118,
            "f5" => 96,  "f6" => 97,  "f7" => 98,  "f8" => 100,
            "f9" => 101, "f10" => 109, "f11" => 103, "f12" => 111,
            other => return Err(ParseTriggerError(format!("unknown key '{other}'"))),
        };
        Ok(KeyTrigger { keycode, modifiers: mods })
    }
}

/// The top-level `[keyboard]` table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyboardConfig {
    /// Maps a trigger string (parsed into [`KeyTrigger`]) to its action.
    /// Keyed by a `KeyTrigger`-rendered string for stable TOML.
    #[serde(default)]
    pub bindings: std::collections::HashMap<KeyTrigger, openlogi_core::binding::Action>,
}
```

(`KeyModifiers` lives in `openlogi-hook`; re-export or duplicate the four bools
in `openlogi-core` to avoid a core→hook dependency. Prefer duplicating — core
must stay leaf-level. Use a `core::KeyModifiers` with the same shape and
convert at the boundary in Task 6.)

Add the field to `Config`:

```rust
    #[serde(default)]
    pub keyboard: KeyboardConfig,
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p openlogi-core --lib config::`
Expected: PASS — parser + TOML load both green.

- [ ] **Step 6: Commit**

```bash
git add crates/openlogi-core/src/config.rs
git commit -m "feat(core): add [keyboard] config section + KeyTrigger parser

KeyTrigger parses '[mod+]+key' strings (f1, shift+cmd+f5, esc) into a
keycode + modifier mask, using the macOS F-key virtual keycodes. The
[keyboard.bindings] table maps triggers to Actions, independent of the
per-device bindings."
```

---

## Task 5: Add the `post_unicode` primitive + inject the three new actions

The execution layer. `post_unicode` types a string via `CGEventKeyboardSetUnicodeString`; the three new `Action` arms call it (for `TypeText`) or spawn a process (for the two Run actions).

**Files:**
- Modify: `crates/openlogi-inject/src/inject.rs:516` (add `post_unicode` next to `post_key`) and the `execute_macos` match arms

- [ ] **Step 1: Add the `post_unicode` primitive to the macOS mod**

In the `mod macos {` block (after `post_media_key`, line 541), add:

```rust
    /// Type an arbitrary unicode string by emitting a single key event per
    /// character whose payload is set via `CGEventKeyboardSetUnicodeString`.
    /// This sidesteps the keyboard layout entirely — characters are injected
    /// as unicode, so "bite me" types verbatim regardless of layout.
    pub(super) fn post_unicode(text: &str) {
        for ch in text.chars() {
            let mut buf = [0u16; 2];
            let s: Cow<str> = Cow::Owned(ch.to_string());
            let _ = s; // unused; the unicode string is set on the event below.
            let event = CGEvent::new(None);
            event.set_flags(CGEventFlags::empty());
            // CGEventKeyboardSetUnicodeString: max 20 UTF-16 units per call;
            // one char at a time is simplest and always in-bounds.
            let units: Vec<u16> = ch.encode_utf16(&mut buf).to_vec();
            unsafe {
                core_foundation::string::CFString::from(&*units.iter()
                    .filter_map(|&u| char::from_u32(u as u32))
                    .collect::<String>());
            }
            // Use the core-graphics binding's keyboard-set-unicode path:
            event.set_string_from_utf16(&units);
            event.post(CGEventTapLocation::HID);
        }
    }
```

NOTE: the exact `core-graphics` API for setting a unicode string on a `CGEvent`
varies by crate version — `set_string_from_utf16` is the typical name. If the
compiler rejects it, run `grep -rn "KeyboardSetUnicodeString\|set_string\|unicode" ~/.cargo/registry/src/*/core-graphics-*/src/event.rs` to find the exact method
name in the pinned version, and use that. The contract is: one `CGEvent` per
character, unicode payload set, posted to HID.

- [ ] **Step 2: Add the three `execute_macos` arms**

In `execute_macos` (find the `match action {` and the `CustomShortcut` arm near line 142), add:

```rust
        Action::TypeText(text) => macos::post_unicode(text),
        Action::RunAppleScript(src) => {
            // Fire-and-forget; the agent must not block the event tap thread.
            let src = src.clone();
            std::thread::spawn(move || {
                let _ = std::process::Command::new("osascript")
                    .args(["-e", &src])
                    .output();
            });
        }
        Action::RunShellCommand(cmd) => {
            let cmd = cmd.clone();
            std::thread::spawn(move || {
                let _ = std::process::Command::new("/bin/sh")
                    .args(["-c", &cmd])
                    .output();
            });
        }
```

(The Run actions spawn off the tap thread because the tap callback must not
block — posting a key while the tap is waiting on a child process wedges input.
Same discipline the existing mouse actions follow.)

- [ ] **Step 3: Build the inject crate**

Run: `cargo build -p openlogi-inject`
Expected: BUILD SUCCEEDS once the `post_unicode` API name matches the pinned
core-graphics version (resolve per the NOTE in Step 1 if needed).

- [ ] **Step 4: Commit**

```bash
git add crates/openlogi-inject/src/inject.rs
git commit -m "feat(inject): post_unicode primitive + TypeText/Run* execution

post_unicode types a string one char at a time via
CGEventKeyboardSetUnicodeString (layout-independent). TypeText uses it;
RunAppleScript spawns osascript, RunShellCommand spawns /bin/sh, both
off the tap thread so a slow script can't wedge input."
```

---

## Task 6: Wire keyboard events → bindings → actions in `hook_runtime`

The integration task. A `KeyEvent` arrives; look it up in the `[keyboard.bindings]` table (by keycode + modifiers); if matched, execute the action and `Suppress` the original key; else `PassThrough`.

**Files:**
- Modify: `crates/openlogi-agent-core/src/hook_runtime.rs` (the `HookEvent::Key(_)` arm from Task 1, Step 4)

- [ ] **Step 1: Read how mouse bindings are looked up + executed**

Run: `grep -n "bindings\|MouseEvent::Button\|inject\|execute" crates/openlogi-agent-core/src/hook_runtime.rs | head -20`
Note how `MouseEvent::Button { id, pressed }` finds its action and calls into
`openlogi-inject`. Mirror that for keys.

- [ ] **Step 2: Replace the inert `HookEvent::Key(_)` arm with real lookup**

The binding state needs access to the loaded `Config`'s `keyboard.bindings`.
Capture an `Arc<HashMap<KeyTrigger, Action>>` into the hook closure (same way
the mouse bindings are captured — find the existing `Arc` capture pattern in
`hook_runtime.rs` and mirror it). Then:

```rust
    HookEvent::Key(KeyEvent { keycode, pressed: true, modifiers }) => {
        // Only act on key-down (avoid double-fire on key-up).
        let trigger = KeyTrigger { keycode, modifiers: convert_modifiers(modifiers) };
        match keyboard_bindings.get(&trigger) {
            Some(action) => {
                execute_action(action);  // reuse the existing mouse-action executor
                EventDisposition::Suppress   // eat the original key
            }
            None => EventDisposition::PassThrough,
        }
    }
    HookEvent::Key(_) => EventDisposition::PassThrough, // key-up, ignore
```

`convert_modifiers` maps `hook::KeyModifiers` → `config::KeyModifiers` (the
duplicate-type boundary noted in Task 4, Step 4). Add it as a small `fn` in
`hook_runtime.rs`.

- [ ] **Step 3: Build + run agent-core tests**

Run: `cargo test -p openlogi-agent-core`
Expected: PASS. No new test here — the integration is exercised manually in
Task 7 (the unit-testable seams are the parser and the action arms, both
already covered).

- [ ] **Step 4: Commit**

```bash
git add crates/openlogi-agent-core/src/hook_runtime.rs
git commit -m "feat(agent): dispatch keyboard events to [keyboard] bindings

A key-down whose keycode+modifiers match a [keyboard.bindings] entry
executes its action and suppresses the original key; unmatched keys pass
through. Reuses the existing action executor; key-up is ignored."
```

---

## Task 7: Manual end-to-end verification on hardware

M1 is complete at this point. This task verifies it on real hardware — the
critical check, per the spec's "test incrementally on hardware" note.

- [ ] **Step 1: Build the dev agent**

Run: `cargo build -p openlogi-agent`

- [ ] **Step 2: Add a test binding to config**

Append to `~/.config/openlogi/config.toml`:

```toml
[keyboard.bindings]
"f1" = { TypeText = "hello from F1" }
```

- [ ] **Step 3: Stop the installed agent and run the dev agent foreground**

```sh
launchctl bootout gui/$(id -u)/org.openlogi.agent
# also quit the GUI so it doesn't respawn the agent
osascript -e 'tell application "OpenLogi" to quit'
sleep 2
OPENLOGI_LOG=debug target/debug/openlogi-agent
```

- [ ] **Step 4: Press F1 in a text field**

Expected: the text "hello from F1" is typed. The original F1 is suppressed (no
brightness/media action fires).

- [ ] **Step 5: Verify the failure modes don't wedge input**

- Press an **unbound** key (e.g. `a`) — it must type normally (PassThrough works).
- Hold the agent running for 30s of mixed typing — input must not freeze. If it
  does, the tap is wedging; revisit Task 2 (the documented HID-tap failure mode).

- [ ] **Step 6: Restore the installed agent**

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/org.openlogi.agent.plist
```

- [ ] **Step 7: Commit any fixups + tag the milestone**

If Steps 4–5 surfaced anything, fix and commit. Then:

```bash
git commit --allow-empty -m "chore: M1 complete — F-key capture + action palette verified on hardware"
```

---

## Self-Review Notes

**Spec coverage (M1 scope):** Execution actions (TypeText/RunAppleScript/RunShellCommand) → Task 3 + 5. `[keyboard.bindings]` config + KeyTrigger → Task 4. F-key capture → Task 2. Modifier-qualified combos → Task 4 (parser) + Task 6 (dispatch). Press-to-bind is **deferred to a later M1.x** — it's a UI/UX flow, not a correctness gap, and adding it here would balloon the plan; flagged honestly rather than hidden. Fixed F-key list → covered by the parser's `f1..f12`/`esc` table (Task 4). Suppress-on-remap → Task 6 + verified in Task 7 Step 5. Media-key reassignment (existing `post_media_key`) is already wired and reachable as a binding target (Task 4's `Action` is the full enum).

**Placeholder scan:** Task 5 Step 1 has an explicit NOTE about resolving the exact `core-graphics` unicode API name against the pinned version — this is a *known unknown with a resolution path*, not a placeholder; the grep command finds the real method name. No "TBD"/"implement later"/"add error handling" anywhere.

**Type consistency:** `KeyEvent` (hook) ↔ `KeyModifiers` (hook) ↔ `KeyTrigger` (config) ↔ `KeyModifiers` (config, duplicate) — the `convert_modifiers` boundary fn in Task 6 bridges the intentional duplicate (core stays leaf-level, no core→hook dep). `Action::TypeText(String)` etc. consistent across Tasks 3/5/6.

**Out of scope (separate plans):** M2 `Workflow` sequencer, M3 media-key capture, press-to-bind UI, per-app keyboard profiles, Windows/Linux capture.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-30-function-key-remapper-m1.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, review between tasks, fast iteration. Best for a multi-task plan touching the input hook (where each task changes observable behavior).

**2. Inline Execution** — execute tasks in this session with checkpoints for review.

Which approach?
