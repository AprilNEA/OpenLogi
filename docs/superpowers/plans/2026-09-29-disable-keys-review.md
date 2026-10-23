# Disable Keys Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:executing-plans to implement this plan. Steps use checkbox syntax for tracking.

**Goal:** Preserve migrated Disable Keys policy and prevent stale background writes from replacing newer choices.

**Architecture:** Extend the existing configuration fold. Reuse the Fn-lock write-order owner for independently ordered settings and route all Disable Keys writes through it. Keep IPC and GUI contracts unchanged unless reproduction demonstrates that their contracts cannot express a correct outcome.

**Tech Stack:** Rust, Tokio, scripted HID transport, Cargo, ast-grep.

## Chunk 1: Regression and repair

### Task 1: Migration

Files: `crates/openlogi-core/src/config/identity.rs` and its existing tests.

- [x] Add a table-driven test through `Config::adopt_route` for unset, empty and nonempty legacy/canonical values, TOML roundtrip and repeat adoption.
- [x] Run `cargo test -p openlogi-core disabled_keys_adoption`; expect lost empty/nonempty settings before the fix.
- [x] Add `fold_option_field!(disabled_keys)` to the existing fold owner.
- [x] Rerun the test and the surrounding identity tests; expect all pass.

### Task 2: Write ordering

Files: `crates/openlogi-agent-core/src/hardware.rs`, `hardware/fn_lock.rs` (rename shared owner), `orchestrator.rs`, `crates/openlogi-agent/src/server.rs`, and focused tests beside these owners.

- [x] Add a deterministic stale-reconnect regression using the scripted channel and the receiver gate. Add an observable completion handle at the worker seam only if needed; do not use sleeps or arbitrary yields to establish ordering.
- [x] Run the regression against the current unordered writer and record the failing HID write/state assertion.
- [x] Rename/generalize FnLockOrder and FnLockTicket, keeping one queue implementation with separate setting instances.
- [x] Route interactive and background Disable Keys writes through one owner; allocate tickets before queuing, retain the write turn through readback, and skip obsolete background work.
- [x] Separate policy intent from retry attempts; acknowledge matching successful writes on reload without assigning stale config a newer identity. Use RAII removal of pending requests and identity-checked success to cover overlapping failures/cancellation.
- [x] Return a typed superseded error for stale RPCs, append its wire variant and update version/goldens, so skipped writes are never persisted as successful.
- [x] Cover config changes/removal and device opt-out so queued old work cannot restore a retired policy; unchanged reloads must not add hardware writes.
- [x] Run focused migration, ordering, setting-policy, scripted-server and Fn-lock regressions, including independent routes/settings and a newer request arriving during an active write.
- [x] Add an ast-grep guard for the ordered-writer boundary; verify green final code and red previous bypass.

## Chunk 2: Review and delivery

- [x] Independently review the final diff against the design: migration semantics, every writer, queue lifetime, cancellation, reconnect, disabled/unmanaged state, and error/readback persistence.
- [x] Run fmt, full-workspace clippy/tests with `RUSTFLAGS=-D warnings`, non-GUI rustdoc, wasm and ast-grep. GUI host checks use `gpui_platform/runtime_shaders` as previously documented.
- [ ] Check final staged paths and untouched user files; commit focused fixes, push the existing PR branch, verify remote SHA and current-head CI.
- [ ] Report exact test results and hardware limitations. Prepare public review replies without posting them unless authorized.

## Verification evidence

- Before fixes: `disabled_keys_adoption` lost an explicit empty policy; an old
  confirmation changed the scripted mask back from `0x80` to `0x81`; a queued
  background write used the replaced channel; a canonical-key reload returned
  unmanaged policy instead of `Some(EMPTY)` after route adoption.
- All four regression paths pass after repair, along with cancellation,
  readback ordering, offline removal/opt-out, unchanged acknowledgment, physical
  identity replacement, and independent settings/routes.
- Full macOS Apple Silicon gate: formatter, workspace Clippy/tests with
  `RUSTFLAGS=-D warnings` and `gpui_platform/runtime_shaders`, non-GUI rustdoc,
  wasm, publish-closure and all ast-grep guards passed. The new guard rejects
  the previous server bypass. No build or dependency versions changed for GUI checks.
- `cargo-deny` was explicitly skipped by `cargo xtask ci` because the executable
  is unavailable. Windows/Linux, separate MSRV, typos and shell jobs were not run
  locally. These results are scripted/host checks, not new hardware validation.
- Independent read-only review approved the final lifecycle and identity changes.
