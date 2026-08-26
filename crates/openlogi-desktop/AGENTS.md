# openlogi-desktop — the settings app

The GPUI + gpui-component window users actually open: device gallery, per-device
panels, Settings, pairing. It is one of three processes in the bundle and the
only one with a settings UI.

This file is the crate's own contract and map. The cross-cutting rules for
writing GPUI code — components, theming, element IDs, icons, task ownership —
live in [`.claude/rules/gui.md`](../../.claude/rules/gui.md) and are **not**
restated here; read that file before editing any `.rs` in this crate. Workspace
standards (lints, module layout, commits, the local gate) are in the root
[`AGENTS.md`](../../AGENTS.md).

## The hard contract: this crate never touches a device

`openlogi-desktop` is a **pure IPC client**. The agent owns the input hook and
every byte of HID++ I/O; this crate reads and writes device state only by
calling the agent over tarpc.

When a panel needs a device value the app cannot currently see, the fix is a new
call in `openlogi-ipc` plus a handler in `openlogi-agent`/`openlogi-agent-core`
— never an `openlogi-hid` call from here, and never a "just this once" direct
read. Wire changes are append-only and versioned; see
[`.claude/rules/ipc-protocol.md`](../../.claude/rules/ipc-protocol.md).

Two more things this crate is not:

- **Not the overlay's parent.** `openlogi-overlay` is a *sibling* process. It
  links `openlogi-ui`, never this crate. Anything both frontends need moves into
  `openlogi-ui` — and every dependency added there is added to the overlay too,
  which is why `gpui-component` is not one of them.
- **Not a home for shared presentation.** Ring geometry, the shared asset
  source, locale negotiation: those are `openlogi-ui`'s. What only the settings
  app draws stays here.

## Map of `src/`

| Path | What lives there |
|---|---|
| `main.rs` | Process bootstrap **only** — logging, single-instance guard, config, locale, the IPC client, then `gpui::run`. It also defines the `tr!` macro above the `mod` declarations, which is why every submodule gets `tr!` without an import (textual macro scope). |
| `runtime.rs` | Everything the app does that isn't a render: one task, one `select!` arm per source that can change long-lived state (agent updates, camera scan, asset commands, finished downloads, `openlogi://` deeplinks). |
| `app.rs`, `app/` | The main window's shell — home gallery, device detail, menu bar, status line, deeplink handling. |
| `windows.rs`, `windows/` | The windows themselves plus the registry that keeps each a singleton. About and Updates are **pages inside Settings**, not windows of their own. |
| `features/` | One module per device-feature panel: `mouse`, `pointer`, `keyboard`, `lighting`, `camera`, `action_ring`, `profile_scope`. |
| `state.rs`, `state/` | `AppState`, the GPUI global every view reads. Anything two views share belongs here; per-component scratch (hover index, open popover) stays in the owning entity. |
| `services/` | Infrastructure, not UI: the IPC client, asset resolution and download, device reads, diagnostics, i18n. |
| `ui/` | Shared components and the hand-painted `Palette` (`theme.rs`). |
| `platform/` | OS integration — app icon, OS facts, updater. |
| `app_assets.rs` | The GPUI asset source, composed in order: embedded logo → `openlogi-ui`'s `action-icons/` → gpui-component's bundled lucide set. A new icon path that resolves nowhere renders blank rather than failing to build. |

Panels gate on `Capabilities`, never on device `kind`; commits go through
`AppState`, never straight to `Config`. Both rules are spelled out in
`.claude/rules/gui.md`.

## Running and verifying

- `cargo run -p openlogi-desktop` — a cargo runner wraps the build into
  `target/dev/OpenLogi.app` with the identity, helpers and plist tables
  packaging uses. **`cargo build` does not refresh that bundle**, and a second
  instance exits on the singleton lock: quit the running app and re-`run` before
  judging a UI change "not applied".
- The window shows only the empty state unless an agent is running. With no
  hardware, `cargo run -p openlogi-agent --bin openlogi-agent-mock` serves a
  scripted inventory.
- `OPENLOGI_COMPONENT_GALLERY=1` on a debug build opens `ui/gallery.rs` instead
  of the app — every shared component rendered without app state. **When you
  change a reusable component, update its gallery entry in the same commit**;
  the gallery is not covered by tests and silently drifts from production
  otherwise.
- The macOS build needs full Xcode for GPUI's Metal shaders. devenv sets the
  environment when Xcode is present (`direnv reload` if the shader compile
  fails).

## Build inputs that are not source

- `build.rs` asks `cargo metadata` where gpui-component's source actually sits
  and copies its `themes/` into `OUT_DIR`, because the compiled crate does not
  ship them. **Do not vendor copies of those themes into this repo.**
  `OPENLOGI_THEMES_DIR` overrides the lookup.
- `themes/openlogi.json` is the app's own theme, layered on that upstream set.
- `bundle/` holds `OpenLogi.entitlements` and one `Info.plist` per bundled
  binary (`desktop-dev`, `agent-release`, `overlay-release`). `cargo xtask`
  packaging reads them; they are the app's macOS identity, not decoration —
  changing one changes what TCC sees. See
  [`.claude/rules/xtask.md`](../../.claude/rules/xtask.md).

## Related rules

| When you touch | Read |
|---|---|
| any `.rs` here (GPUI house style) | [`.claude/rules/gui.md`](../../.claude/rules/gui.md) |
| `services/i18n.rs`, any user-facing string | [`.claude/rules/i18n.md`](../../.claude/rules/i18n.md) |
| anything crossing the agent boundary | [`.claude/rules/ipc-protocol.md`](../../.claude/rules/ipc-protocol.md) |
| `platform/**` or any macOS FFI | [`.claude/rules/objc-ffi.md`](../../.claude/rules/objc-ffi.md) |
| a permission symptom or the bundle identity | [`.claude/skills/openlogi-macos-permissions/SKILL.md`](../../.claude/skills/openlogi-macos-permissions/SKILL.md) |
