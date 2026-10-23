# Disable Keys save retry implementation plan

> **For agentic workers:** Use executing-plans to implement this plan, with independent review before delivery.

**Goal:** Keep GUI configuration projections coherent after Save Retry adopts external edits.

**Architecture:** Reuse AppState's existing projection rebuild and StateEvents emission. Keep ConfigState responsible for file revision and rollback, and retain feature-local recovery without new HID writes.

**Tech Stack:** Rust, GPUI context tests, tracked temporary TOML configuration.

## Chunk 1: Reproduction, correction, and delivery

Implementation and independent review completed on October 23, 2026. Delivery
is recorded by the Git commit and remote branch rather than this checklist.

- [ ] In `crates/openlogi-desktop/src/state/disable_keys.rs`, extend the existing save-conflict test with an external F-key binding. Assert `keyboard_bindings()` matches the saved config after retry. Run `cargo test -p openlogi-desktop --features gpui_platform/runtime_shaders save_failure_is_local` and observe the stale binding failure.
- [ ] Change `retry_disable_keys_save_inner` to return `StateEvents`, not bool. Return no events without a recovery token; otherwise start with `DisableKeysChanged(key)`. After successful `refresh_feature`, call `restore_config_projections` and combine `SettingsChanged` with current-device `BindingsChanged` and `DpiChanged`. Preserve those events on a subsequent save error. Use `AppState::apply` in the public retry action.
- [ ] Add tests for malformed external config, no recovery token, changed current device, and production GPUI event delivery. Hold the real config sidecar lock with `File::try_lock` to prove successful refresh followed by failed persistence still refreshes projections and emits the matching events; release the handle and retry to verify recovery without a second HID write. Retain existing closed-IPC and no-second-write assertions; verify refreshed projection coherence does not depend on successful agent reload.
- [ ] Document refreshed settings/bindings in `docs/CONFIGURATION.md`. Run focused `disable_keys` tests, then review the diff and event subscribers.
- [ ] Fetch remotes and verify the published head/base. Preserve unrelated untracked docs. If rebase is needed, resolve it before the final gate and use the full tier.
- [ ] Run fmt, workspace all-target Clippy, workspace tests and all-target tests, strict non-GUI rustdoc, typos and ast-grep on the final tree with `RUSTFLAGS=-D warnings`. Reuse the prior target/tool directories. Report other-host/hardware validation separately.
- [ ] Independently review final code/tests, fix actionable findings, commit only scoped paths, and push to `origin/feat/keyboard-disable-keys-controls` without bypassing hooks. Verify the remote SHA and report delivery and actual results.

## Verification results

- Original regression: one failing test, GUI binding map stayed empty while the
  external F1 mapping was saved. The same test passes after the correction.
- Focused Disable Keys suite: 21 passed, including four added regressions.
- Final macOS arm64 / Rust 1.99.1 gate: fmt, full workspace all-target Clippy,
  workspace tests (3391 passed, 4 ignored), all-target tests, and strict non-GUI
  rustdoc passed. GPUI builds use `--features gpui_platform/runtime_shaders`;
  all compilation uses `RUSTFLAGS=-D warnings`.
- `cargo xtask ci typos ast-grep`: two passed, none skipped.
- Sandbox cannot read the macOS login session required by one existing agent
  test. That test and the full gate passed outside the sandbox, without changing
  or ignoring the assertion.
- No new native-window, physical-keyboard, Windows, or Linux runtime validation.
