---
paths:
  - "crates/openlogi-gui/**"
---

# GUI (GPUI + gpui-component)

- The UI stack is GPUI + gpui-component — a settled choice; don't propose alternatives.
- `gpui`/`gpui_platform` track zed's default branch on purpose; the compatible zed
  commit is pinned **only in `Cargo.lock`**, in lockstep with the `gpui-component` rev.
  After any `cargo add`/`cargo update`, check the pins didn't move; restore with
  `cargo update -p gpui --precise <rev>`.
- Two color systems must agree: the bespoke `theme.rs` `Palette` (hand-painted
  surfaces) and gpui-component's `cx.theme()` (widget chrome). Only the `ThemeMode` is
  shared between them. A "white box under dark UI" or a surface that doesn't flip with
  the OS appearance is a ThemeMode wiring bug — fix that, not per-element `bg()`.
- Trait imports must be unconditional for cross-platform widgets: a
  `#[cfg(target_os = "macos")]`-gated `use gpui::StatefulInteractiveElement as _;`
  compiles fine locally but breaks the Linux/Windows CI jobs the moment an ungated
  element calls `.id(..).on_click(..)`. When adding such an element, ungate the import.
- Icons are not limited to gpui-component's `IconName`: vendor any SVG (must use
  `stroke="currentColor"`) into `action-icons/`, register it in `app_assets.rs`'s
  `ACTION_ICONS`, render via `Icon::empty().path("action-icons/….svg")`.
- Config panels/tabs gate on `Capabilities` (derived from the HID++ feature table),
  **never** on device `kind` — kind is identity-only (icon/label). A new panel means a
  new capability in `Capabilities::from_feature_ids` plus a `tabs_for` arm.
- Mouse-diagram hotspots come from Logi metadata; if the metadata omits a button
  marker, omit the button — never synthesize hotspot positions.
- Verifying UI changes needs the running app: re-`cargo run -p openlogi-gui` (a plain
  `cargo build` leaves the dev bundle stale) after quitting the previous instance
  (singleton lock). The GUI shows only the empty state unless the agent is running.
