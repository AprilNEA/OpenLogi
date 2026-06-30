# Design: Mouse buttons 6–9

**Status:** Approved
**Date:** 2026-06-29
**Scope:** Add four pickable actions that synthesize mouse buttons 6–9, so apps
that bind those buttons (CAD, games, Blender, MMO-mouse emulators) receive them.

## Problem

OpenLogi's `Action` vocabulary caps mouse output at "button 5"
(`MouseForward`). There is no way to emit mouse buttons 6–9, even though the
underlying injection layer on macOS and Linux already supports arbitrary button
numbers. Users who want a physical button to produce a button-6-or-higher event
have no path today.

## Goal / non-goals

**Goal:** Let any rebindable `ButtonId` be mapped to an emitted mouse button
6, 7, 8, or 9, selectable from the action picker like any existing action.

**Non-goals:**

- No new physical button capture — these are *output* actions bound to existing
  physical buttons (e.g. Gesture Button → button 6).
- No new picker UI. The catalog auto-surfaces new variants under the MOUSE
  section.
- No Windows support for buttons 6–9. `SendInput`'s mouse path carries flags
  for buttons 1–5 only; 6–9 are a documented macOS/Linux-only gap (Windows
  remains an "untested preview" per the README). See
  [Platform coverage](#platform-coverage).

## Background: why this is small

Every layer of the action stack is data-driven from the `Action` enum:

- The GUI picker (`crates/openlogi-gui/src/mouse_model/picker.rs`) builds its
  rows by calling `Action::catalog()` and grouping by `Action::category()`.
- The picker's icon mapping (`picker.rs:298`) is an exhaustive `match` with
  **no wildcard arm**, so the compiler refuses to build if a new variant lacks
  an icon entry.
- `Action::label()` / `category()` / `catalog()` drive the picker text,
  grouping, and TOML roundtrip tests.
- The injection layer on macOS (`post_other_button(n)`) already accepts any
  button number via the `MOUSE_EVENT_BUTTON_NUMBER` field; Linux `evdev`
  exposes `BTN_BACK`/`BTN_FORWARD`/`BTN_TASK`/`BTN_0`…

So the work is: four enum variants, each threaded through the data-driven
machinery that already exists for `MouseBack`/`MouseForward`.

## Design

### New variants

Append four unit variants to `Action` in `crates/openlogi-core/src/binding.rs`,
directly after `MouseForward`, mirroring that pattern exactly:

```rust
/// Extra mouse button 6. Emitted as the real button-6 event for apps/games/CAD
/// that bind it. macOS/Linux only — Windows SendInput caps at button 5.
MouseButton6,
MouseButton7,
MouseButton8,
MouseButton9,
```

**Naming:** variant identifiers `MouseButton6`..`MouseButton9`; display labels
`"Button 6"`..`"Button 9"`. Unlike `Back`/`Forward`, these numbers have no
universal semantic meaning, so they carry no semantic name.

### Per-layer changes

| Layer | File | Change |
|---|---|---|
| Enum | `openlogi-core/src/binding.rs` | +4 variants after `MouseForward` |
| `Action::label()` | same | `"Button 6"` … `"Button 9"` |
| `Action::category()` | same | all 4 → `Category::Mouse` |
| `Action::catalog()` | same | append all 4 to the catalog (Mouse group) |
| Picker icon map | `openlogi-gui/src/mouse_model/picker.rs:298` | map all 4 to an existing generic icon (`action-icons/mouse.svg`) — the existing exhaustive `match` forces this |
| Inject — macOS | `openlogi-inject/src/inject.rs` (`execute_macos`) | `MouseButton6..9` → `macos::post_other_button(5..=8)` |
| Inject — Linux | `openlogi-inject/src/inject.rs` (`execute_linux`) | → `BTN_FORWARD` / `BTN_BACK` / `BTN_TASK` / `BTN_0` |
| Inject — Windows | `openlogi-inject/src/inject.rs` (`execute_windows`) | log-and-skip (`tracing::debug!`, same pattern as the macOS-only navigation actions at `inject.rs:104`) |

### Button-number mapping

The macOS convention (0-indexed, from existing `MouseBack`=3, `MouseForward`=4)
extends naturally:

| Action | macOS `post_other_button` arg | Linux evdev `KeyCode` |
|---|---|---|
| `MouseButton6` | 5 | `BTN_FORWARD` |
| `MouseButton7` | 6 | `BTN_BACK` |
| `MouseButton8` | 7 | `BTN_TASK` |
| `MouseButton9` | 8 | `BTN_0` |

> **Open question for implementation:** the Linux `BTN_*` assignment above is a
> reasonable convention (the evdev `BTN_BACK/FORWARD/TASK/0..9` family is how
> multi-button mice report extras), but exact code choice is a convention call
> that should be confirmed against how target apps read buttons on Linux.
> macOS numbers are unambiguous.

### TOML / config schema

Unit variants serialize as bare strings via serde's default external tagging —
identical to `MouseBack`/`MouseForward`:

```toml
[devices."<addr>".bindings]
GestureButton = "MouseButton6"
# In gesture form:
Back = { Click = "MouseButton7" }
```

**Stability contract preserved:** existing variant names are frozen; these are
purely additive new names. **No `schema_version` bump, no migration.** Older
OpenLogi builds reading a config containing `MouseButton6` will error on the
unknown variant (acceptable — same as any newer-schema config on older code).

### Platform coverage

| Platform | Buttons 6–9 | Notes |
|---|---|---|
| macOS | ✅ Full | `post_other_button` already takes any number |
| Linux | ✅ Full | evdev `BTN_*` family |
| Windows | ❌ Log-and-skip | `SendInput` mouse path (`inject.rs:1416`) has flags for buttons 1–5 only; no flag exists for 6+. Documented gap, matches the codebase's existing "no platform equivalent → debug log + skip" pattern. |

Windows users who bind these actions see nothing on press and a debug log line;
no crash, no misfire. Given Windows is an untested preview and the requester is
on macOS, this boundary is acceptable and explicitly out of scope to fix here.

## Testing

- **TOML roundtrip:** `all_catalog_variants_roundtrip_toml` already iterates
  `catalog()`, so the four new entries are covered automatically once in the
  catalog.
- **Category:** extend `category_mouse_variants` to assert all four map to
  `Category::Mouse`.
- **Compile-time guarantee:** the exhaustive picker icon `match` (no wildcard)
  fails to build if any variant is missed — this is the primary safety net.

## Risks

- **Linux `BTN_*` choice** — convention rather than correctness; see open
  question above. Low impact (target apps are the test).
- **Pickup-row clutter** — four more entries in the MOUSE group. Acceptable;
  matches user intent for a "full set".
- None to the input-capture path — these are pure output/synthesis actions.

## Out of scope

- Windows support for buttons 6–9.
- A parameterized `MouseButton(n)` variant (Approach B) — rejected as
  disproportionate picker UI for a fixed set of four.
- Capturing buttons 6–9 as *input* from exotic hardware.
