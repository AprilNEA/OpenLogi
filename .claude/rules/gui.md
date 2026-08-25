---
paths:
  - "crates/openlogi-desktop/**"
  - "crates/openlogi-ui/**"
  - "crates/openlogi-overlay/**"
---

# GUI (GPUI + gpui-component)

- The UI stack is GPUI + gpui-component — a settled choice; don't propose alternatives.
- **Three crates, not one.** `openlogi-desktop` is the settings app; `openlogi-overlay` is
  the Actions Ring helper, a separate process and a pure IPC client; `openlogi-ui` is
  what they share — ring geometry/icons, the GPUI asset source, locale negotiation.
  The overlay must never depend on `openlogi-desktop`. Before putting anything in
  `openlogi-ui`, check both binaries actually need it: every dependency added there is
  also added to the overlay, which is why `gpui-component` is *not* one of them.
- One catalog, in `openlogi-ui/locales/`, beside the `locale` module that negotiates
  over it. `t!` resolves against a backend the invoking crate must generate itself, so
  each binary still expands its own `rust_i18n::i18n!` over that shared directory by
  relative path. A wrong path there compiles to an **empty catalog** rather than an
  error, and every string silently renders as its English key —
  `the_shared_catalog_is_wired_up` in `openlogi-overlay` is what makes that fail loudly.
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
  `stroke="currentColor"`) into `crates/openlogi-ui/action-icons/`, register it in that
  crate's `app_assets.rs` `ACTION_ICONS`, render via
  `Icon::empty().path("action-icons/….svg")`. Both binaries serve the same set.
- Config panels/tabs gate on `Capabilities` (derived from the HID++ feature table),
  **never** on device `kind` — kind is identity-only (icon/label). A new panel means a
  new capability in `Capabilities::from_feature_ids` plus a `tabs_for` arm.
- Mouse-diagram hotspots come from Logi metadata; if the metadata omits a button
  marker, omit the button — never synthesize hotspot positions.
- Keep render helpers statically typed (`impl IntoElement` or a concrete element) until
  a genuinely heterogeneous branch, collection, callback, or stored field requires
  `AnyElement`. Prefer one typed `.when()` / `.when_some()` / `.children()` pipeline to
  branching early and erasing each result.
- A view, entity, or app service owns every `Task` and `Subscription` whose work belongs
  to its lifetime. Use `.detach()` only for true process-lifetime work or bounded
  one-shots whose completion is safe after the initiating view disappears.
- When an async UI operation captures inputs that may change before it completes
  (device, route, query, selection, or request), capture their identity or generation
  at launch and compare again before committing the result. Cancellation is useful,
  but is not the stale-result fence.
- When changing reusable controls, prefer focused `#[gpui::test]` behavior contracts
  over screenshot coverage: keyboard activation, disabled no-op, controlled selected
  state/callbacks, and independent parent/child interaction targets.
- Verifying UI changes needs the running app: re-`cargo run -p openlogi-desktop` (a plain
  `cargo build` leaves the dev bundle stale) after quitting the previous instance
  (singleton lock). The GUI shows only the empty state unless the agent is running.
