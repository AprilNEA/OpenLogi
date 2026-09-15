---
paths:
  - "crates/openlogi-desktop/**"
  - "crates/openlogi-ui/**"
  - "crates/openlogi-overlay/**"
---

# GUI (GPUI + gpui-component)

## Use the upstream skills

- Load [`gpui-kit`](../../.agents/skills/gpui-kit/SKILL.md) for GPUI implementation and review.
- Load [`gpui-kit-design-guides`](../../.agents/skills/gpui-kit-design-guides/SKILL.md)
  before layout, styling, interaction, copy, or design review.
- Follow their Coding Guides and Design Guides as the default practice. Prefer
  those guides over conflicting reference examples or existing OpenLogi patterns.
  The constraints below cover only OpenLogi integration; do not duplicate general
  GPUI rules here.

## OpenLogi integration

- **Process ownership:** desktop and overlay are IPC clients; the agent owns HID
  and input I/O. The overlay must not depend on `openlogi-desktop`. Put shared
  presentation in `openlogi-ui`; keep desktop-only widgets and assets in desktop.
  Evaluate shared dependencies against both consumers.
- **Dependency compatibility:** use `Cargo.toml` and `Cargo.lock` to verify API
  signatures, imports, and test support. OpenLogi currently uses direct Kit crates
  rather than the umbrella `gpui-kit` crate. Treat migrations as explicit, tested
  changes, not a side effect of copying bootstrap examples. Keep GPUI core/platform
  versions aligned, Kit crates on one release tag, and one GPUI package identity.
- **Device state:** route settings through `AppState`, not direct `Config` writes.
  Use measured or last-good `Capabilities` for hardware features. Extend `tabs_for`
  for panel selection, including device-kind presentation and offline fallbacks.
  When a device image has Logi metadata, do not invent missing button markers.
- **Presentation owners:** use desktop's `ui::components` and `ui::theme` for
  existing screens. `control_button`, `control_input`, and `control_select` apply
  `CONTROL_H` (30 px); `Palette` and the component theme must agree on `ThemeMode`.
  Adopt upstream improvements in these owners rather than patching each call site.
  Shared SVGs use `openlogi-ui/action-icons/` and its `ActionIcons` asset source;
  keep shared paths such as `RING_CANCEL_ICON` identical in the preview and overlay.
- **Live language changes:** follow [i18n.md](i18n.md). Each localized binary generates
  its own backend for `openlogi-ui/locales/`. Use `localize_placeholder` for cached
  input placeholders and `StateEvent::LanguageChanged` for other cached text.
  Run menu/window effects after `AppState` updates or defer them to avoid re-entry.

## Verify in this repository

Use the root [AGENTS.md](../../AGENTS.md) for checks and the
[desktop guide](../../crates/openlogi-desktop/AGENTS.md#running-and-verifying) for
the dev bundle, mock agent, and component gallery. Do not substitute upstream
`script/check-ai` or starter-package commands. Exercise representative affected
states, inspect fresh screenshots for visible changes, and report untested OS or
hardware states. Update the gallery when a reusable component changes.
