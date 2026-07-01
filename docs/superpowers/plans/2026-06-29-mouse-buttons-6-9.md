# Mouse Buttons 6–9 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add four pickable actions (`MouseButton6`–`MouseButton9`) that synthesize mouse buttons 6–9 on macOS and Linux, surfaced in the action picker under the MOUSE section.

**Architecture:** Pure data-driven addition. The `Action` enum in `openlogi-core` is the single source of truth — the GUI picker, category grouping, TOML schema, and per-platform injection all derive from it. Four new unit variants mirror the existing `MouseBack`/`MouseForward` pattern. On macOS the existing `post_other_button(n)` already accepts any button number; on Linux the evdev `BTN_*` family covers it; on Windows `SendInput` caps at button 5, so 6–9 log-and-skip there (documented gap, same pattern as existing platform-limited actions).

**Tech Stack:** Rust (workspace), serde/TOML for config, GPUI for the GUI, core-graphics / evdev / windows-sys for injection, rust-i18n for locale strings.

**Spec:** `docs/superpowers/specs/2026-06-29-mouse-buttons-6-9-design.md`

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/openlogi-core/src/binding.rs` | The `Action` enum + `label()`/`category()`/`catalog()` | Add 4 variants + their 3 match arms + extend 2 tests |
| `crates/openlogi-inject/src/inject.rs` | Per-platform `Action` → OS event synthesis | Add arms in `execute_macos` / `execute_linux` / `execute_windows` |
| `crates/openlogi-gui/src/mouse_model/picker.rs` | Picker icon mapping (exhaustive `match`) | Add 4 arms (compiler-forced) |
| `crates/openlogi-gui/locales/*.yml` (20 files) | i18n translation keys, keyed by English label | Add `"Button 6"`–`"Button 9"` keys after line 147 |

No new files. The exhaustive `match` arms across the codebase are the safety net — the compiler refuses to build if any variant is missed.

---

## Task 1: Add the `Action` variants and core metadata

This task adds the four variants and threads them through `label()`, `category()`, and `catalog()`. Tests fail first, then pass.

**Files:**
- Modify: `crates/openlogi-core/src/binding.rs`

- [ ] **Step 1: Write the failing test (extend `category_mouse_variants`)**

In `crates/openlogi-core/src/binding.rs`, find the test at line ~1346 and replace it:

```rust
    #[test]
    fn category_mouse_variants() {
        assert_eq!(Action::LeftClick.category(), Category::Mouse);
        assert_eq!(Action::RightClick.category(), Category::Mouse);
        assert_eq!(Action::MiddleClick.category(), Category::Mouse);
        assert_eq!(Action::MouseBack.category(), Category::Mouse);
        assert_eq!(Action::MouseForward.category(), Category::Mouse);
        assert_eq!(Action::MouseButton6.category(), Category::Mouse);
        assert_eq!(Action::MouseButton7.category(), Category::Mouse);
        assert_eq!(Action::MouseButton8.category(), Category::Mouse);
        assert_eq!(Action::MouseButton9.category(), Category::Mouse);
    }
```

- [ ] **Step 2: Add a label test (append to the `#[cfg(test)] mod tests` block, after `category_mouse_variants`)**

```rust
    #[test]
    fn extra_mouse_button_labels() {
        assert_eq!(Action::MouseButton6.label(), "Button 6");
        assert_eq!(Action::MouseButton7.label(), "Button 7");
        assert_eq!(Action::MouseButton8.label(), "Button 8");
        assert_eq!(Action::MouseButton9.label(), "Button 9");
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p openlogi-core --lib binding::tests::category_mouse_variants binding::tests::extra_mouse_button_labels`
Expected: FAIL with `cannot find variant MouseButton6` (and 7/8/9) — compile error.

- [ ] **Step 4: Add the four variants to the `Action` enum**

In `crates/openlogi-core/src/binding.rs`, find the `MouseForward` variant (line ~371) and add the four new variants immediately after it, before the `// ── Editing ───` comment:

```rust
    /// Mouse "forward" side button (extra button 5). Native counterpart to
    /// [`Action::MouseBack`]; see [`Action::BrowserForward`] for the ⌘] form.
    MouseForward,
    /// Extra mouse button 6. Emitted as the real button-6 event for apps, games,
    /// and CAD software that bind it. macOS/Linux only — Windows `SendInput`
    /// caps at button 5, so this logs-and-skips there.
    MouseButton6,
    /// Extra mouse button 7. See [`Action::MouseButton6`].
    MouseButton7,
    /// Extra mouse button 8. See [`Action::MouseButton6`].
    MouseButton8,
    /// Extra mouse button 9. See [`Action::MouseButton6`].
    MouseButton9,
```

- [ ] **Step 5: Add the four labels to `Action::label()`**

In the `label()` match (around line ~686, after the `MouseForward` arm), add:

```rust
            Action::MouseForward => "Forward (Button 5)".into(),
            Action::MouseButton6 => "Button 6".into(),
            Action::MouseButton7 => "Button 7".into(),
            Action::MouseButton8 => "Button 8".into(),
            Action::MouseButton9 => "Button 9".into(),
```

- [ ] **Step 6: Add the four to `Action::category()`**

In the `category()` match (around line ~737), extend the existing Mouse arm:

```rust
            Action::LeftClick
            | Action::RightClick
            | Action::MiddleClick
            | Action::MouseBack
            | Action::MouseForward
            | Action::MouseButton6
            | Action::MouseButton7
            | Action::MouseButton8
            | Action::MouseButton9 => Category::Mouse,
```

- [ ] **Step 7: Add the four to `Action::catalog()`**

In the `catalog()` vec (around line ~793, after `Action::MouseForward`), add:

```rust
            // Mouse
            Action::LeftClick,
            Action::RightClick,
            Action::MiddleClick,
            Action::MouseBack,
            Action::MouseForward,
            Action::MouseButton6,
            Action::MouseButton7,
            Action::MouseButton8,
            Action::MouseButton9,
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p openlogi-core --lib binding`
Expected: PASS — `category_mouse_variants`, `extra_mouse_button_labels`, and `all_catalog_variants_roundtrip_toml` (which iterates `catalog()`) all pass.

- [ ] **Step 9: Commit**

```bash
git add crates/openlogi-core/src/binding.rs
git commit -m "feat(core): add MouseButton6-9 actions to the binding vocabulary"
```

---

## Task 2: Inject the buttons on macOS

macOS already has `post_other_button(n)` which stamps `MOUSE_EVENT_BUTTON_NUMBER`. Buttons 6–9 map to numbers 5–8 (0-indexed: Back=3, Forward=4).

**Files:**
- Modify: `crates/openlogi-inject/src/inject.rs` (the `execute_macos` function, around line 186–187)

- [ ] **Step 1: Add the macOS injection arms**

Find the macOS extra-button arms (line ~186–187):

```rust
        Action::MouseBack => macos::post_other_button(3),
        Action::MouseForward => macos::post_other_button(4),
```

Add immediately after them:

```rust
        Action::MouseBack => macos::post_other_button(3),
        Action::MouseForward => macos::post_other_button(4),
        // Buttons 6–9 (button numbers 5–8, 0-indexed). Same path as 4/5 —
        // post_other_button stamps MOUSE_EVENT_BUTTON_NUMBER to address any
        // button ≥ 3.
        Action::MouseButton6 => macos::post_other_button(5),
        Action::MouseButton7 => macos::post_other_button(6),
        Action::MouseButton8 => macos::post_other_button(7),
        Action::MouseButton9 => macos::post_other_button(8),
```

- [ ] **Step 2: Verify the macOS build compiles**

Run: `cargo build -p openlogi-inject`
Expected: BUILD SUCCEEDS (on macOS the `execute_macos` arms are the ones compiled).

- [ ] **Step 3: Commit**

```bash
git add crates/openlogi-inject/src/inject.rs
git commit -m "feat(inject): synthesize mouse buttons 6-9 on macOS"
```

---

## Task 3: Inject the buttons on Linux

evdev 0.13.2 exposes `BTN_BACK`, `BTN_FORWARD`, `BTN_TASK`, and `BTN_0` as `KeyCode` constants. These are the conventional codes for extra mouse buttons beyond the side pair.

**Files:**
- Modify: `crates/openlogi-inject/src/inject.rs` (the `execute_linux` function, around line 77–78)

- [ ] **Step 1: Add the Linux injection arms**

Find the Linux extra-button arms (line ~77–78):

```rust
        Action::MouseBack => linux::click(KeyCode::BTN_SIDE),
        Action::MouseForward => linux::click(KeyCode::BTN_EXTRA),
```

Add immediately after them:

```rust
        Action::MouseBack => linux::click(KeyCode::BTN_SIDE),
        Action::MouseForward => linux::click(KeyCode::BTN_EXTRA),
        // Buttons 6–9 use the evdev extra-button codes beyond the side pair.
        Action::MouseButton6 => linux::click(KeyCode::BTN_FORWARD),
        Action::MouseButton7 => linux::click(KeyCode::BTN_BACK),
        Action::MouseButton8 => linux::click(KeyCode::BTN_TASK),
        Action::MouseButton9 => linux::click(KeyCode::BTN_0),
```

- [ ] **Step 2: Verify it compiles on Linux (cross-check)**

This is a `#[cfg(target_os = "linux")]` block. On macOS it won't be compiled, so to verify the `KeyCode::BTN_*` constants resolve, run a Linux target check:

Run: `cargo check -p openlogi-inject --target x86_64-unknown-linux-gnu`
Expected: If the target is installed, CHECK SUCCEEDS. If not installed, this step is skipped — the constant names (`BTN_FORWARD`/`BTN_BACK`/`BTN_TASK`/`BTN_0`) are confirmed present in evdev 0.13.2 (see spec's button-number table), and CI will catch any mismatch on Linux.

- [ ] **Step 3: Commit**

```bash
git add crates/openlogi-inject/src/inject.rs
git commit -m "feat(inject): synthesize mouse buttons 6-9 on Linux via evdev BTN_*"
```

---

## Task 4: Log-and-skip on Windows

Windows `SendInput` mouse input carries flags for buttons 1–5 only; there is no flag for button 6+. The codebase's established pattern for "no platform equivalent" is a `tracing::debug!` log and skip (see the macOS-only navigation actions at `inject.rs:104`). Mirror that.

**Files:**
- Modify: `crates/openlogi-inject/src/inject.rs` (the `execute_windows` function, around line 290–291)

- [ ] **Step 1: Add the Windows log-and-skip arms**

Find the Windows extra-button arms (line ~290–291):

```rust
        Action::MouseBack => windows::post_click(windows::MouseButton::Back),
        Action::MouseForward => windows::post_click(windows::MouseButton::Forward),
```

Add immediately after them:

```rust
        Action::MouseBack => windows::post_click(windows::MouseButton::Back),
        Action::MouseForward => windows::post_click(windows::MouseButton::Forward),
        // Windows SendInput carries flags for buttons 1–5 only; there is no
        // flag for button 6+, so these log-and-skip (same pattern as the
        // macOS-only navigation actions). macOS/Linux emit them natively.
        Action::MouseButton6
        | Action::MouseButton7
        | Action::MouseButton8
        | Action::MouseButton9 => {
            tracing::debug!(
                action = action.label(),
                "mouse buttons 6-9 are not supported on Windows — press ignored"
            );
        }
```

- [ ] **Step 2: Verify it compiles on Windows (cross-check)**

Run: `cargo check -p openlogi-inject --target x86_64-pc-windows-msvc`
Expected: If the target is installed, CHECK SUCCEEDS. If not, skip — CI covers Windows. (No new types are referenced; `tracing::debug!` and `action.label()` already exist.)

- [ ] **Step 3: Commit**

```bash
git add crates/openlogi-inject/src/inject.rs
git commit -m "feat(inject): log-and-skip mouse buttons 6-9 on Windows"
```

---

## Task 5: Add picker icons (compiler-forced)

The picker's `action_icon_path` is an exhaustive `match` with no wildcard. After Task 1, the macOS/GUI build will fail here until the four variants are mapped. Reuse the existing generic mouse icon (`action-icons/mouse.svg`, already used by `MiddleClick`).

**Files:**
- Modify: `crates/openlogi-gui/src/mouse_model/picker.rs` (the `action_icon_path` match, line ~304–305)

- [ ] **Step 1: Add the four icon arms**

Find the MouseBack/MouseForward arms (line ~304–305):

```rust
        Action::MouseBack => "action-icons/circle-arrow-left.svg",
        Action::MouseForward => "action-icons/circle-arrow-right.svg",
```

Add immediately after them:

```rust
        Action::MouseBack => "action-icons/circle-arrow-left.svg",
        Action::MouseForward => "action-icons/circle-arrow-right.svg",
        // Buttons 6–9 have no canonical glyph; reuse the generic mouse icon
        // (same as MiddleClick). The button number is in the label.
        Action::MouseButton6
        | Action::MouseButton7
        | Action::MouseButton8
        | Action::MouseButton9 => "action-icons/mouse.svg",
```

- [ ] **Step 2: Verify the full workspace builds (this is the compile-time gate)**

Run: `cargo build -p openlogi-gui`
Expected: BUILD SUCCEEDS. If any variant is still missing an arm anywhere, this fails — that's the safety net working.

- [ ] **Step 3: Commit**

```bash
git add crates/openlogi-gui/src/mouse_model/picker.rs
git commit -m "feat(gui): pick icons for mouse buttons 6-9 in the action picker"
```

---

## Task 6: Add i18n keys to all 20 locale files

The picker translates action labels via `t!(action.label())` — keyed by the English string. The existing `"Back (Button 4)"` / `"Forward (Button 5)"` keys live at line ~146–147 of every locale file. New keys `"Button 6"`–`"Button 9"` must be added so the labels translate. For non-English locales, the translation mirrors the English form plus the localized "Button" word where the locale already uses one — but since the English label is intentionally number-only, the safest correct default is to leave the translated value identical to the key (English fallback) except where the locale clearly localizes "Button" (most do not for raw button numbers).

**Files:**
- Modify: all 20 files in `crates/openlogi-gui/locales/*.yml`

- [ ] **Step 1: Add the four keys to `en.yml` (the source)**

In `crates/openlogi-gui/locales/en.yml`, after line 147 (`"Forward (Button 5)": "Forward (Button 5)"`), add:

```yaml
"Forward (Button 5)": "Forward (Button 5)"
"Button 6": "Button 6"
"Button 7": "Button 7"
"Button 8": "Button 8"
"Button 9": "Button 9"
```

- [ ] **Step 2: Add the same four keys to each of the other 19 locale files**

For each file in `crates/openlogi-gui/locales/` except `en.yml`, after the `"Forward (Button 5)"` line (which exists at line ~147 in every file — confirmed), append:

```yaml
"Button 6": "Button 6"
"Button 7": "Button 7"
"Button 8": "Button 8"
"Button 9": "Button 9"
```

The 19 files are: `da.yml de.yml el.yml es.yml fi.yml fr.yml it.yml ja.yml ko.yml nb.yml nl.yml pl.yml pt-BR.yml pt-PT.yml ru.yml sv.yml zh-CN.yml zh-HK.yml zh-TW.yml`.

(Values are left as the English string — these are raw button numbers with no semantic name to localize. A crowdin pass can refine later; the project already uses crowdin per `crowdin.yml`. Leaving them English-identical is the correct fallback and matches how `en.yml` itself is authored.)

- [ ] **Step 3: Verify the GUI still builds and the i18n test passes**

Run: `cargo test -p openlogi-gui --lib i18n`
Expected: PASS. (The i18n test at `i18n.rs:185+` checks specific known strings, not exhaustively, so it won't break — but it confirms the locale loader still parses all files.)

- [ ] **Step 4: Commit**

```bash
git add crates/openlogi-gui/locales/*.yml
git commit -m "feat(gui): add Button 6-9 translation keys to all locales"
```

---

## Task 7: Final whole-workspace verification

Confirm everything builds and tests pass end-to-end.

- [ ] **Step 1: Build the whole workspace**

Run: `cargo build --workspace`
Expected: BUILD SUCCEEDS on macOS (the dev platform). This compiles `execute_macos`, the picker, the core — everything reachable on this host.

- [ ] **Step 2: Run the whole test suite**

Run: `cargo test --workspace`
Expected: ALL PASS. Key tests: `binding::tests::category_mouse_variants`, `binding::tests::extra_mouse_button_labels`, `binding::tests::all_catalog_variants_roundtrip_toml` (now exercises the 4 new variants' TOML roundtrip).

- [ ] **Step 3: Manual smoke check (optional but recommended)**

Build and run the GUI, open a device, click a rebindable button (e.g. Gesture Button), and confirm "Button 6"–"Button 9" appear in the MOUSE section of the action picker. Bind one and confirm it fires (e.g. an app that binds MB6).

Run: `cargo run -p openlogi-gui`

- [ ] **Step 4: Final commit if any fixups were needed**

If steps 1–2 surfaced anything to fix, commit it. Otherwise this task produces no commit.

---

## Self-Review Notes

**Spec coverage:** Every layer in the spec's "Per-layer changes" table maps to a task — enum/label/category/catalog (Task 1), macOS inject (Task 2), Linux inject (Task 3), Windows log-and-skip (Task 4), picker icons (Task 5), i18n keys (Task 6). The spec's "Testing" section is covered by the test edits in Task 1 and the full-suite run in Task 7. Platform-coverage table matches (Windows gap explicit in Task 4).

**No placeholders:** Every code step shows the exact code. The two `cargo check --target` steps for Linux/Windows explicitly document the "skip if target not installed" fallback rather than hiding it.

**Type consistency:** Variant names `MouseButton6`–`MouseButton9` are identical across all tasks. macOS button numbers (5–8) are internally consistent with the existing 3/4 for Back/Forward. Linux `KeyCode` constants are confirmed in evdev 0.13.2.
