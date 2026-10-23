# Disable Keys feedback and control identity

## Scope

Use the existing desktop status/retry components for Disable Keys, localize
recoverable errors, and scope retained control identity to the physical device.
Do not change configuration schema, IPC, hardware writes, or retry scheduling.

## Owners and presentation

`AppState` remains the per-device transaction owner. Replace its presentation-ready
error strings with a small semantic error enum: agent unavailable, connection
changed, write failed, save failed, refresh failed, and reload failed. Log raw
diagnostic details at the failure boundary; do not show them as ordinary UI copy.
Translate the semantic error when the keyboard panel renders, not when it occurs,
so a retained error follows live language changes.

The panel uses `ui::status::{status_line, retry_line}`, as the neighboring backlight
and SmartShift panels do. Remove its private status-text renderer. Read failures
and unsupported states show localized product messages rather than raw errors.
Keep Save Retry, Reload Retry, and Read Retry as distinct commands with their
existing guards. A feature-local recoverable failure must not replace the window
with the global configuration-error screen.

Use stable IDs containing `DeviceKey` and the control kind/key bit. They must not
depend on locale, checked state, list index, or route generation. Scope recovery
buttons to their device as well. Preserve pointer/keyboard behavior and all existing
write/confirmation fences. Switch accessibility names use the visible key labels.

Add semantic message keys to every shared locale catalog, in matching order. English
is the source, Russian recovery copy must be translated and exercised; other locales
may retain English fallback per the repository's localization policy.

## Proof and limits

Before production edits, reproduce an unavailable-agent recovery showing English
under Russian, and shared switch focus identity when the same retained panel changes
devices. After the fix, test EN/RU changes on the retained error, exact key/device
identity separation, stable identity across rerenders and checked-state changes,
localized read/unsupported/recovery text, and disabled control behavior. Keep existing
state-machine regression coverage green, including save/reload retry without new HID
writes. Use the actual GPUI components in headless rendering/interaction tests.

Run the complete local macOS gate, catalog parity, i18n wiring, spelling and ast-grep
before pushing. Inspect final diff and unchanged schema/IPC/dependency files. Do not
claim native screenshot, Windows/Linux runtime, or hardware verification from
headless tests. Existing Computer Use permission restrictions may prevent native
pixel inspection; report that limit explicitly rather than changing system settings.
