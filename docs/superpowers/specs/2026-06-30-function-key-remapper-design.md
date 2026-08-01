# Design: Function-Key Remapper

**Status:** Draft (pending user review)
**Date:** 2026-06-30
**Scope:** Turn every capturable function-row key (and, in a later milestone, the
system media keys) into a fully programmable trigger that can reassign media
keys, type macro strings, run AppleScript, run shell commands, or execute a
timed multi-step workflow.

## Motivation

OpenLogi today remaps **mouse** buttons only. Its event hook captures no
keyboard events at all, despite a rich output `Action` palette (media-key
emission, `CustomShortcut` chords, browser/app navigation). Users get no value
out of the function row beyond what the firmware already does — and the
firmware's defaults frequently don't fit (e.g. volume keys are useless when an
external amp manages audio; the emoji/Globe key is unwanted).

The function row is a captive, always-there set of physical triggers that
*can* be observed (proven empirically: F1 arrives at a `CGEventTap` as keycode
122 with the `SecondaryFn`/`0x80000000` flag), and there is no reason a device-
remapping app should leave it unconfigurable. This design makes every
capturable key a fully programmable one.

## Goals / non-goals

**Goals**
- Remap F1–F12 + Esc (the literal function-row keys) to arbitrary actions.
- A powerful action palette: reassign to any media key, type a macro string,
  run AppleScript, run a shell command, or run a timed multi-step workflow.
- Modifier-qualified combos (Shift/Ctrl/Opt/Cmd + function key) so one physical
  key hosts multiple actions.
- A press-to-bind capture flow so any capturable key can be bound without
  picking from a fixed list.

**Non-goals (for this design)**
- **Capturing the `Fn` modifier itself** as a trigger. Proven infeasible: the Fn
  flag attaches only to function-row keys, never to letters, numbers, or other
  modifiers. `Fn+Q` is byte-identical to plain `Q` at the event tap. Fn is
  firmware-internal unless the key has a dual function-row meaning. See
  Appendix A.
- **Windows / Linux** capture in the initial milestones. macOS first; the
  execution actions cross-platform where they already are; capture ported later.
- **Per-application profiles** for keyboard bindings in M1 (the mouse side has
  these; the keyboard side inherits them in a later milestone once the base
  works).

## Background: what exists today

- **Hook is mouse-only** (`crates/openlogi-hook/src/macos.rs::translate`):
  handles `LeftMouseDown`/`RightMouseDown`/scroll/move only. Zero keyboard
  events. This is the central new ground.
- **Rich action palette** (`crates/openlogi-core/src/binding.rs::Action`):
  `VolumeUp/Down`, `MuteVolume`, `PlayPause`, `NextTrack`, `PrevTrack`,
  `BrightnessUp/Down`, `BrowserBack/Forward`, `MissionControl`, `LaunchpadShow`,
  `Paste/Copy/Cut/Undo/Redo`, `CustomShortcut(KeyCombo)`, `SetDpiPreset`, and
  (via the mouse-buttons-6-9 PR) `MouseButton6..9`.
- **Media-key emission exists** (`macos::post_media_key(NX_KEYTYPE_*)`) — so
  "reassign to a media key" is already an execution primitive, not new work.
- **Key-chord emission exists** (`CustomShortcut` → `macos::post_key` +
  modifiers) — so emitting key sequences is partially there, but there is **no
  text-typing / unicode-string primitive** (`CGEventKeyboardSetUnicodeString`).
- **Config is TOML**, keyed per-device, with a frozen variant-name contract and
  `schema_version` for migrations.

## Architecture

The feature splits into two halves with very different risk profiles.

### Half 1 — Execution (what a key does): all buildable

The action palette gains three new `Action` variants, all reusing the existing
enum → picker → injection pipeline (the same one extended for mouse buttons
6–9). No new mechanism, only new variants + two new emission primitives.

| Action variant | Mechanism | New work |
|---|---|---|
| `RunAppleScript(String)` | spawn `osascript -e "<src>"` | new variant, trivial |
| `RunShellCommand(String)` | spawn shell, capture nothing | new variant, trivial |
| `TypeText(String)` | new `macos::post_unicode(&str)` via `CGEventKeyboardSetUnicodeString` | new variant **+ new emitter** |
| `Workflow(Vec<WorkflowStep>)` | a sequencer that runs steps with `Delay` timing | new variant **+ new sequencer subsystem** |
| (media reassignment) | existing `post_media_key` | **already exists** |

A `WorkflowStep` is a small enum:

```rust
enum WorkflowStep {
    TypeText(String),
    PressKey(KeyCode),                 // reuse the key-emitter from CustomShortcut
    Delay(Duration),
    RunAppleScript(String),
    RunShellCommand(String),
}
```

The sequencer runs steps in order, awaiting `Delay`s. This is the native,
no-code version of the "type 'bite me', wait 5s, Enter, wait 5s, type more,
Escape" example. Power users can equivalently express the same thing in a
single `RunAppleScript` or `RunShellCommand`.

### Half 2 — Capture (which key triggers it): split by risk

Extending the mouse-only hook to also subscribe to keyboard `CGEvent` types is
the central new capture work. It splits by key class:

| Key class | Capture mechanism | Risk |
|---|---|---|
| **F1–F12, Esc** (function mode) | Extend the existing `CGEventTap` mask to include `keyDown`/`keyUp`/`flagsChanged`; new `KeyEvent` vocabulary analogous to `MouseEvent` | **Low** — same tap, new event types. F1 proven empirically (keycode 122 + `0x80000000`). |
| **Media keys** (volume / brightness / emoji / play / etc.) | New `NX_SYSDEFINED` system-event tap (`CGSSetSystemDefinedMediaTap`) — a **separate event stream** OpenLogi has none of today | **High / unproven** — gated milestone; see M3. |

This split is why the milestones order F-key capture before media-key capture:
F-key capture is an extension of the proven existing tap; media-key capture is a
new subsystem whose feasibility must be empirically confirmed before design
commits to it.

## Trigger specification

Three complementary ways to specify a trigger, all producing the same
`KeyTrigger`:

```rust
/// A keyboard trigger: a keycode plus an optional modifier mask.
/// Stored under `[keyboard.bindings]` keyed by a stable string.
struct KeyTrigger {
    keycode: u16,          // macOS kVK_* code (e.g. 122 = F1)
    modifiers: Modifiers,  // Shift/Control/Option/Command mask; empty for bare
}
```

1. **Fixed F-key list** — the picker offers F1–F12 + Esc (the keys proven
   capturable). Matches the mouse-button picker UX.
2. **Modifier-qualified combos** — Shift/Ctrl/Opt/Cmd + F-key, so one physical
   key hosts several actions. These modifiers ARE detectable (unlike Fn).
3. **Press-to-bind capture** — a "press a key to bind" flow: OpenLogi records
   the next `keyDown`'s keycode (+modifiers) and binds it. Generalizes beyond
   the fixed list to any capturable key.

## Config schema (additive)

A new top-level `[keyboard]` section, keyed by a stable trigger string. New
`Action` variants are tagged unions (serde external tagging), consistent with
`CustomShortcut(KeyCombo)`:

```toml
# Existing device bindings unchanged:
[devices."<addr>".bindings]
GestureButton = "MissionControl"

# NEW — keyboard bindings, independent of device:
[keyboard.bindings]
"f1"            = { TypeText = "bite me" }
"shift+f1"      = { RunAppleScript = "tell application \"Terminal\" to activate" }
"cmd+f1"        = { RunShellCommand = "open -a 'Safari' https://example.com" }
"f2"            = "VolumeUp"                  # reassign a function key to a media key
"f3"            = { Workflow = [
    { TypeText = "bite me" },
    { Delay = "5s" },
    { PressKey = "Return" },
    { Delay = "5s" },
    { TypeText = "bite me bad" },
    { PressKey = "Return" },
    { PressKey = "Escape" },
]}
```

**Stability contract:** existing variant names are frozen; these are purely
additive. A `[keyboard]` section is new, but unknown top-level sections are
ignored by older loaders, so no `schema_version` bump is *required* for
back-compat. Bump it anyway (cheap, conventional) so the GUI can show a clean
"what changed" diff and refuse to silently drop bindings a newer build wrote.

## Milestones

**M1 — F-key capture + powerful action palette (shippable, low-risk)**
- Extend `openlogi-hook` to capture keyboard `CGEvent`s; new `KeyEvent` vocab.
- New `Action` variants: `RunAppleScript`, `RunShellCommand`, `TypeText`
  (+ new `macos::post_unicode` emitter).
- `[keyboard.bindings]` config + loader; fixed F-key picker UI.
- Modifier-qualified combos (Shift/Ctrl/Opt/Cmd + F-key).
- Deliverable: any F1–F12/Esc (and combo) runs AppleScript / shell / types a
  string / fires a media key.

**M2 — Native Workflow sequencer**
- `Workflow(Vec<WorkflowStep>)` action + sequencer with `Delay` timing.
- `WorkflowStep`: `TypeText`, `PressKey`, `Delay`, `RunAppleScript`, `RunShellCommand`.
- Deliverable: the timed multi-step "type, wait, Enter, wait, type, Esc" flows
  authorable in TOML without scripting.

**M3 — Media-key capture (gated on feasibility test)**
- *Before any design commitment:* empirically test whether volume/brightness/
  emoji keys are interceptable via an `NX_SYSDEFINED` system-event tap. If the
  OS grabs them below the tap (as the Fn investigation warned), this milestone
  is descoped or killed — do not assume.
- If feasible: new system-event tap subsystem; extend trigger list to media
  keys; deliverable: remap volume/brightness/emoji to any action.

## Risks (honest)

1. **Media-key capture (M3) is unproven.** macOS routes system media keys
   through `NX_SYSDEFINED`, a separate stream from `CGEventTap`. OpenLogi has
   zero of this today. The feasibility test gates M3; M1/M2 do not depend on it.
2. **Security surface.** `RunShellCommand` / `RunAppleScript` execute arbitrary
   code from config. This is a real escalation vs. today's action set. Mitigation:
   these variants are **never** in the default catalog; they must be hand-authored
   in config, and the loader warns on first use. (Matches how `CustomShortcut` is
   already a deliberate escape hatch.)
3. **Key-suppression correctness.** Remapping requires *consuming* the original
   key event (returning "drop this") so it doesn't also type. The mouse hook
   already does this via `EventDisposition`; the keyboard path must too, with
   care to avoid wedging input (the documented HID-tap-wedge failure mode).
4. **Capture vs. the existing mouse tap.** Adding keyboard event types to the
   existing tap broadens what it intercepts; the HID-location tap that outlives
   its permission wedges **all** input (mouse + keyboard). Extra care + testing
   needed here, given that documented failure mode.

## Out of scope

- Capturing the `Fn` modifier as a trigger (proven infeasible — Appendix A).
- Per-application keyboard profiles in M1 (mouse side has these; keyboard
  inherits later).
- Windows/Linux keyboard capture in M1 (port after macOS works).

---

## Appendix A: Why Fn is not a trigger (proven, not assumed)

Investigated empirically this session with an instrumented `CGEventTap`:

- **F1** arrives as keycode 122 **with** the `SecondaryFn`/`0x80000000` flag.
- **plain Q** and **Fn+Q** are byte-for-byte identical (keycode 12, `raw=0x100`,
  no Fn flag). Same for A.
- **plain Shift** and **Fn+Shift** are byte-for-byte identical (`raw=0x20102`,
  no Fn flag).
- Pressing **Fn alone** produces no event of any kind (no `FlagsChanged`).

**Conclusion:** the Fn flag attaches **only to function-row keys** (F1–F12),
never to letters, numbers, or other modifiers. The keyboard firmware holds Fn
internal unless the key has a dual function-row meaning. `Fn+<anything else>`
is indistinguishable from `<anything else>` at the `CGEventTap`. This is
firmware behavior, not a limitation OpenLogi can code around at this layer.
The only theoretical path to sensing Fn+letters is raw-HID reading below the
OS event system (Karabiner/driver-kit territory) — a large subsystem with no
guarantee the MX Keys S exposes Fn there. Not pursued.
