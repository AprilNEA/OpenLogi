# Disable Keys feedback implementation plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if available) or superpowers:executing-plans. Steps use checkbox syntax for tracking.

**Goal:** Match the project's shared feedback presentation, localize recovery errors, and isolate switch identity between physical devices.

**Architecture:** Keep transaction ownership in `AppState` and translate semantic errors at render time. Compose the keyboard panel from existing `status_line` / `retry_line` and Switch. No schema, IPC, dependencies, or hardware policy changes.

**Tech Stack:** Rust, GPUI, gpui-component, rust-i18n, existing shared TOML catalogs.

## Chunk 1: Scoped correction and delivery

### Task 1: Reproduce

Files: `crates/openlogi-desktop/src/features/keyboard/disable_keys/tests.rs`
and its parent `disable_keys.rs` (test-module declaration only).

- [x] Add an unavailable-agent test through AppState's normal read/write entrypoints:
  install a synthetic keyboard, answer the read, close IPC, request a write. Resolve
  the retained error using the production panel's presentation path and expect
  `Фоновая служба недоступна.` under Russian.
- [x] Add a GPUI harness rendering the production `load_content`. Focus Caps Lock,
  rerender with the same key, then a second physical DeviceKey. Assert retained
  focus identity is stable for the former and distinct for the latter. Supply only
  in-memory configuration; do not start any real agent.
- [x] Run `CARGO_TARGET_DIR=/private/tmp/openlogi-pr993-target RUSTFLAGS='-D warnings' cargo test -p openlogi-desktop --features gpui_platform/runtime_shaders disable_keys::`.
  Confirm assertion failures, not compilation/setup errors.

### Task 2: Correct presentation and state

Files: `crates/openlogi-desktop/src/state/disable_keys.rs`, `state.rs`,
`features/keyboard/disable_keys.rs`, `features/keyboard/disable_keys/tests.rs`,
and `crates/openlogi-ui/locales/*.toml`.

- [x] Add `DisableKeysError` with the six semantic failure categories from the spec.
  Replace each state error assignment with its category; retain raw details in
  tracing diagnostics. Update existing assertions to the typed error.
- [x] Replace the private status renderer with `status_line`; use `retry_line` for
  read/save/reload retry. Map each error to a semantic `keyboard.*` translation key
  at render time. Do not change recovery transition or command behavior.
- [x] Build switch identity from device key and key mask; scope retry IDs likewise.
  Add accessibility labels without duplicating the visible row label.
- [x] Add matching catalog keys/order, Russian translations for this block, and
  tests covering every recovery category, live language changes, read/unsupported
  text, disabled controls, and distinct key identities within the same device.
- [x] Rerun the reproducer and existing Disable Keys tests. Run catalog parity and
  desktop i18n tests, expecting all to pass.

### Task 3: Documentation, verification, push

Files: `docs/CONFIGURATION.md` and these spec/plan documents.

- [x] Document localized feature-local recovery and diagnostic logging without
  claiming schema compatibility or hardware validation.
- [x] Review the final code and test proof independently; fix actionable findings.
- [x] Run `cargo fmt --all -- --check`.
- [x] With `RUSTFLAGS='-D warnings'` and the existing temporary target, run
  `cargo clippy --workspace --all-targets --features gpui_platform/runtime_shaders -- -D warnings`,
  `cargo test --workspace --features gpui_platform/runtime_shaders`, and
  `cargo test --workspace --all-targets --features gpui_platform/runtime_shaders`.
- [x] Run `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --document-private-items --exclude openlogi-ui --exclude openlogi-desktop --exclude openlogi-overlay --exclude openlogi-agent` with the same compiler environment.
- [x] Run `cargo xtask ci typos ast-grep`; inspect `git diff --check`, changed paths,
  and final diff. Report unavailable native pixel/other-OS checks separately.
- [x] Verify remote branch still matches the inspected head and inspect the scoped
  delivery diff. Schema, IPC, hardware policy, and dependencies are unchanged.

Delivery follows the verified tree: stage only scoped files, commit, push normally,
and report the verified remote SHA in the task handoff. Do not post a public comment.


## Verification record — 2026-10-23

Host: macOS arm64, Rust 1.99.1. Reused `/private/tmp/openlogi-pr993-target`;
all Cargo checks used `RUSTFLAGS='-D warnings'`.

- The two regression proofs failed before the fix: Russian recovery displayed
  `agent is unavailable`, and different devices shared the same Switch focus handle.
- Focused `disable_keys::` tests: 27 passed, including seven new localization,
  rendered interaction, and identity tests.
- Independent source/test review: no remaining actionable findings.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --features gpui_platform/runtime_shaders -- -D warnings`: passed.
- `cargo test --workspace --features gpui_platform/runtime_shaders`: 3,398 passed,
  0 failed, 4 ignored (existing interactive tests and ignored documentation examples).
- `cargo test --workspace --all-targets --features gpui_platform/runtime_shaders`:
  3,397 passed, 0 failed, 2 existing interactive tests ignored.
- Non-GUI rustdoc command above with `RUSTDOCFLAGS='-D warnings'`: passed.
- `cargo test -p openlogi-ui locale`: catalog parity passed.
- `cargo test -p openlogi-desktop --features gpui_platform/runtime_shaders i18n`:
  3 passed.
- `cargo xtask ci typos ast-grep`: both jobs passed, including the explicit
  `.github` scan. The new guard rejected all four old private-feedback copies in
  a temporary baseline tree; the shared owner and unrelated source passed.
- `git diff --check`: passed.

The tests use in-process GPUI rendering and scripted IPC, not physical HID writes.
Native pixel inspection was unavailable because Computer Use access was blocked;
no screenshots or real-hardware validation are claimed. Linux/Windows runtime,
packaging, and dependency checks were not rerun for this GUI-only follow-up.
The dependency `block 0.1.6` still produces Cargo's existing future-compatibility
notice, but no compiler or Clippy warning fails the gate.
