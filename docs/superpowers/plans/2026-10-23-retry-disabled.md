# Shared retry disabled appearance implementation plan

> Use executing-plans; subagent-driven-development is not installed.

**Goal:** Restore semantic disabled appearance and prove retry interactions.
**Architecture:** Button owns styling and activation; AppState owns persistence.
**Tech stack:** Existing Rust, GPUI and gpui-component; no new dependencies.

## Chunk 1: Fix and verify

- [x] Add red rendered-style test in `src/ui/status/tests.rs` with stock text
  Button comparison in both themes. Run desktop `ui::status::` tests with
  `--features gpui_platform/runtime_shaders`.
- [x] Remove explicit foreground and palette argument in `src/ui/status.rs`;
  update all `retry_line` callers and add enabled/disabled gallery entries.
- [x] Cover Enter/Space enabled and disabled callbacks; add production-panel
  Save Retry tests for pointer/Enter/Space under a temporary config conflict.
- [x] Run focused status/Disable Keys/gallery tests and independent review.
- [x] With `CARGO_TARGET_DIR=/private/tmp/openlogi-pr993-target` and
  `RUSTFLAGS='-D warnings'`, run fmt, workspace all-targets Clippy, workspace
  tests and all-targets tests (GUI commands use runtime_shaders).
- [x] Run non-GUI rustdoc with warnings denied and the repository's standard
  UI/desktop/overlay/agent exclusions; run `cargo xtask ci typos ast-grep`.
- [ ] Inspect staged diff, unchanged schema/dependencies and remote SHA; commit,
  push normally, and verify published SHA. Preserve unrelated drafts.
