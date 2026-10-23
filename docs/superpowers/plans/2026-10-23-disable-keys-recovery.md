# Disable Keys recovery implementation plan

> For agentic workers: use executing-plans; isolated tasks may be delegated and reviewed.

**Goal:** Fix three recovery findings, document the feature, reconcile master, verify and update PR #993.

**Architecture:** Retain desktop read ownership, agent write-order ownership and inventory scheduling. No parallel retry engine or new wire contract.

**Tech Stack:** Rust, Tokio, GPUI/SWR, scripted HID++ channels, Git.

## Chunk 1: integration and recovery

- [x] Review the approved spec and plan independently; address actionable gaps.
- [x] Fetch fresh upstream/master and origin PR branch, preserve a backup ref, rebase
  in the existing PR worktree. Resolve mock_agent.rs by preserving stdin controls
  and Disable Keys state/RPCs. Check both mock test groups after integration.
- [x] Desktop: in `crates/openlogi-desktop/src/state/disable_keys.rs` tests and
  `crates/openlogi-desktop/src/services/disable_keys_reads.rs`, reproduce a write error retaining Ready.
  Run `cargo test -p openlogi-desktop disable_keys --features gpui_platform/runtime_shaders`.
  Require a behavior assertion failure, then invalidate stale data at the read owner,
  trigger normal reads and cover successful/failed rereads and old generations.
- [x] Agent: extend `crates/openlogi-agent-core/src/orchestrator/tests/disabled_keys.rs`
  to drive production scheduling instead of calling reapply. Observe failure with
  `cargo test -p openlogi-agent-core disabled_keys`; then add cancellation-safe
  completion signaling at the existing owner. Explicitly arm route-scoped recovery
  even after the boot followup budget drains, consuming it through the production
  orchestrator/inventory path. Recover the committed (possibly unsaved) choice,
  not only the persisted policy ticket. Cover failed/cancelled/successful
  successors and removal/replacement. Keep policy identity checks at execution.
  Test saved A -> confirmed unsaved B -> unconfirmed C restores B, and a confirmation
  while a successor is pending cannot permanently consume recovery work.
- [x] Watcher: extend inventory `schedule.rs` tests and `replay_tests.rs` with failed
  confirmation and repair scans. Run `cargo test -p openlogi-agent-core watchers::inventory`;
  observe failing deadline/event assertions. Pass typed delivery outcome to scheduling,
  preserve bounded delayed confirmation, rerun focused and neighboring tests.
  Keep logical pending work after fast exhaustion and test later recovery success;
  test repair/confirmation ties cannot reset budgets indefinitely.

## Chunk 2: documentation and delivery

- [x] Update existing user docs and HID++ reference: capability gating, supported
  keys, switch meaning, confirmed persistence, retry/read/save/reload errors, lifecycle
  reapplication and limitations. Check links, spelling and consistency with code.
- [x] Review complete final diff and new tests for bypassed production paths, stale
  results, cancellation, overlapping requests, bounded retries and mock-console parity.
- [x] With `RUSTFLAGS=-D warnings`, reuse `/private/tmp/openlogi-pr993-target`:
  fmt check, workspace all-targets Clippy with runtime_shaders, workspace tests,
  macOS all-targets tests, strict non-GUI rustdoc. Run ast-grep (root and .github),
  typos, wire goldens, locale/i18n, wasm, publish closure and cargo-deny as applicable
  to the full rebased PR. Record counts and limitations. Do not skip hooks.
- [x] Inspect available isolated mock/native UI testing without touching production
  devices. Record exact limitations if native evidence is unavailable.
- [ ] Verify fresh base and fork SHA, commit focused fixes and push with an exact
  lease if rewritten. Check remote head/mergeability/current-head CI; update PR body
  with evidence, not an unverified claim of maintainer approval or hardware success.

## Evidence and limitations

- Rebased onto upstream `2ac50ad5`, then onto `9af2530e` after master moved
  during verification; repeated the full gate on the latter. The only conflict was the mock agent's
  state/online handling. Retained stdin control, battery overrides, faults and
  Disable Keys scenarios. Mock tests: 38 passed, including console faults/links.
- Observed desktop red: stale confirmed snapshot and enabled controls after
  error. Final focused tests: 17 passed, including real SWR stale-read fencing
  and same-route reconnect during reconciliation.
- Observed agent red: manual failure did not arm recovery after boot retries
  drained. Final Disable Keys tests: 14 passed; write-order tests: 19 passed.
  Production scheduling is exercised; no direct reapply call drives restoration.
- Observed watcher red: failed confirmation selected RepairRetry instead of
  SettingsConfirmation. Final inventory tests: 24 passed. Enumeration errors
  are injected at the production completion boundary, because ReplayBackend
  does not provide enumeration-failure injection. Successful scans use replay;
  scheduler timing uses logical time, not sleeps.
- Final macOS arm64 / Rust 1.99.1 gate: fmt, workspace all-targets Clippy,
  workspace tests (3387 passed, 4 ignored), macOS all-targets tests (3386 passed,
  2 ignored), strict non-GUI rustdoc, ast-grep root/.github, typos, wasm,
  publish closure and cargo-deny pass. Counts are separate runs, not additive.
  Cargo-deny retains duplicate/unused-allowance warnings; Clippy reports the
  existing dependency future-incompatibility notice for block 0.1.6.
- Workspace tests include IPC wire goldens, locale parity, desktop i18n and
  fixture corpus tests. Strict standalone verification of the canonical
  synthetic fixture passes; it declares no HID cassettes and is not hardware
  evidence. No hooks were disabled; this checkout has no active Git hooks.
- The native dev GUI built and connected to an isolated mock using temporary
  XDG directories. Computer Use permission was unavailable, so rendered
  interaction/screenshots could not be verified. Both test processes stopped.
- Windows/Linux execution, exact-MSRV, shell jobs and physical-device checks
  were not run. No changed shell files; no new platform-specific implementation
  in this recovery fix. The full PR's platform label branch was manually audited.
- HID++ integration is documented in the vendored crate README and linked user
  configuration docs. No tracked reference claiming DisableKeys was unused
  outside its wrapper was found; an external reference is not edited here.
