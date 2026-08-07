# macOS Scroll Inversion Fallback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a persistent Logitech mouse such as Signature M650 use per-device scroll-direction inversion on macOS when it lacks native HID++ `0x2121` inversion support, without changing trackpad scrolling or double-inverting native-capable mice.

**Architecture:** Keep `Capabilities::scroll_inversion` as the native hardware capability. The agent publishes a separate in-process map of non-native pointer identities and configured inversion values; the macOS event tap resolves each physical scroll event and replaces only a matched vertical mouse-wheel event with a marked synthetic event. The GUI enables this fallback for persistent pointer devices on macOS; config and IPC formats remain unchanged.

**Tech Stack:** Rust workspace, `core-graphics` CGEventTap, IOKit sender identity, GPUI, TOML config, Cargo test/clippy/rustfmt.

---

### Task 1: macOS hook inversion disposition and safe event replacement

**Files:**
- Modify: `crates/openlogi-hook/src/lib.rs`
- Modify: `crates/openlogi-hook/src/macos.rs`

- [ ] **Step 1: Write failing conversion tests**

Add macOS unit tests beside `usable_scroll_delta` which create line- and pixel-unit `CGEvent::new_scroll_event` values. Assert a new helper negates the vertical delta, preserves the horizontal delta, and selects pixel units when either captured axis uses point/fixed-point data.

```rust
#[test]
fn synthetic_scroll_values_invert_vertical_and_preserve_horizontal() {
    let event = CGEvent::new_scroll_event(None, ScrollEventUnit::Line, 2, 7, -3).unwrap();
    let values = synthetic_scroll_values(&event);
    assert_eq!(values.vertical, -7);
    assert_eq!(values.horizontal, -3);
    assert_eq!(values.unit, ScrollEventUnit::Line);
}
```

- [ ] **Step 2: Verify the test fails**

Run `cargo test -p openlogi-hook synthetic_scroll_values_invert_vertical_and_preserve_horizontal`.

Expected: compilation fails because `synthetic_scroll_values` does not exist.

- [ ] **Step 3: Implement the minimal replacement path**

Add a macOS-only `EventDisposition::InvertScroll`. Make an independent `CGEventCreateCopy`, negate its vertical line/fixed/point fields, preserve horizontal and all metadata, set `openlogi_inject::SYNTHETIC_EVENT_USER_DATA`, and return it through `CallbackResult::Replace`. Return `Keep` if copying fails. Do not restructure the tap thread or run loop.

```rust
match cb(mouse_event) {
    EventDisposition::PassThrough => CallbackResult::Keep,
    EventDisposition::Suppress => CallbackResult::Drop,
    EventDisposition::InvertScroll => inverted_scroll_event(event)
        .map_or(CallbackResult::Keep, CallbackResult::Replace),
}
```

- [ ] **Step 4: Verify and format**

Run `cargo test -p openlogi-hook` and `cargo fmt --all -- --check`.

Expected: all hook tests pass and formatting is clean.

- [ ] **Step 5: Commit**

Run `git add crates/openlogi-hook/src/lib.rs crates/openlogi-hook/src/macos.rs` and `git commit -m "feat(hook): add macos scroll inversion disposition"`.

### Task 2: Per-device software decisions in the agent runtime

**Files:**
- Modify: `crates/openlogi-agent-core/src/hook_runtime.rs`

- [ ] **Step 1: Write failing identity and decision tests**

Add tests for product-ID preference, normalized/contained product-name fallback, conflicting identity removal, and disposition gating. The disposition test must prove a matched enabled M650 mouse yields `InvertScroll`, while trackpad, unknown source, zero vertical delta, and disabled match pass through.

```rust
#[test]
fn scroll_disposition_only_inverts_matched_mouse_wheels() {
    let inversions = Arc::new(RwLock::new(ScrollInversions::new([(
        ScrollDeviceKey { vendor_id: Some(0x046d), product_id: Some(0xb02a), product_name: Some("Logi M650".into()) },
        true,
    )])));
    let mouse = EventDevice { vendor_id: Some(0x046d), product_id: Some(0xb02a), product_name: Some("Logi M650".into()) };
    assert_eq!(scroll_disposition(1.0, false, Some(&mouse), &inversions), EventDisposition::InvertScroll);
    assert_eq!(scroll_disposition(1.0, true, Some(&mouse), &inversions), EventDisposition::PassThrough);
    assert_eq!(scroll_disposition(1.0, false, None, &inversions), EventDisposition::PassThrough);
}
```

- [ ] **Step 2: Verify the test fails**

Run `cargo test -p openlogi-agent-core scroll_disposition_only_inverts_matched_mouse_wheels`.

Expected: compilation fails because the software inversion types and helper do not exist.

- [ ] **Step 3: Implement the shared identity map**

Implement `ScrollDeviceKey`, `ScrollInversions`, and `SharedScrollInversions` with vendor-aware `BTreeMap`s. Treat a reported product ID as authoritative and require an exact vendor/product pair; only events without a product ID may compare lowercase trimmed names with a minimum four-character containment match. Discard an identity if two device entries assign conflicting values. Return `InvertScroll` only for an enabled, non-trackpad, non-zero vertical event matched to the map.

- [ ] **Step 4: Verify and format**

Run `cargo test -p openlogi-agent-core hook_runtime` and `cargo fmt --all -- --check`.

Expected: all focused tests pass and formatting is clean.

- [ ] **Step 5: Commit**

Run `git add crates/openlogi-agent-core/src/hook_runtime.rs` and `git commit -m "feat(agent): decide software scroll inversion per device"`.

### Task 3: Publish only non-native pointer devices

**Files:**
- Modify: `crates/openlogi-agent-core/src/orchestrator.rs`
- Modify: `crates/openlogi-agent/src/main.rs`

- [ ] **Step 1: Write failing orchestrator tests**

Extend the inventory fixture so tests can set model IDs, product names, pointer capability, and native inversion capability. Assert a persistent M650-like device with `pointer = true`, `scroll_inversion = false`, and saved `invert_scroll = true` appears in `shared.scroll_inversions`; assert native-capable and non-pointer devices do not.

```rust
let source = EventDevice {
    vendor_id: Some(0x046d),
    product_id: Some(0xb02a),
    product_name: Some("Logi M650".into()),
    ..EventDevice::default()
};
assert!(orchestrator.shared().scroll_inversions.read().unwrap()
    .inversion_for(Some(&source)).is_some_and(|(_, value)| value));
```

- [ ] **Step 2: Verify the test fails**

Run `cargo test -p openlogi-agent-core rebuild_publishes_only_non_native_pointer_scroll_fallbacks`.

Expected: compilation fails because `SharedRuntime` has no fallback map and `AgentDevice` retains no event-source identity.

- [ ] **Step 3: Wire inventory identity into rebuilds**

Retain nonzero `model.model_ids`, `paired.codename`, and receiver identity in `AgentDevice`. Add `scroll_inversions` to `SharedRuntime`, initialize it, and rebuild from devices satisfying `capabilities.is_some_and(|caps| caps.pointer && !caps.scroll_inversion)`. Publish vendor-aware device identity for every eligible device. Publish a Bolt/Unifying receiver identity only when the inventory has exactly one possible pointer; keep direct-device identity available. Include identity/capability/safety changes in rebuild detection. Pass `shared.scroll_inversions` into `hook_runtime::start` in `openlogi-agent/src/main.rs`.

- [ ] **Step 4: Verify and format**

Run `cargo test -p openlogi-agent-core orchestrator`, `cargo check -p openlogi-agent`, and `cargo fmt --all -- --check`.

Expected: focused tests pass, the agent compiles, and formatting is clean.

- [ ] **Step 5: Commit**

Run `git add crates/openlogi-agent-core/src/orchestrator.rs crates/openlogi-agent/src/main.rs` and `git commit -m "feat(agent): publish macos scroll fallback devices"`.

### Task 4: Enable and explain the fallback in the GUI

**Files:**
- Modify: `crates/openlogi-gui/src/state.rs`
- Modify: `crates/openlogi-gui/src/app/detail.rs`
- Modify: every `crates/openlogi-gui/locales/*.yml`

- [ ] **Step 1: Write failing AppState support tests**

Add tests proving that on macOS a persistent pointer without native inversion supports the toggle, a native-capable persistent device supports it, and a non-pointer or transient device does not. Keep a separate accessor reporting native support for the explanatory text.

```rust
assert!(state.current_scroll_inversion_supported());
assert!(!state.current_native_scroll_inversion_supported());
state.commit_invert_scroll(true);
assert!(state.current_invert_scroll());
```

- [ ] **Step 2: Verify the test fails**

Run `cargo test -p openlogi-gui scroll_inversion_supports_persistent_macos_pointer_fallback`.

Expected: the support assertion fails because the current gate accepts only native HID++ capability.

- [ ] **Step 3: Implement the platform-aware gate and descriptions**

Make `current_scroll_inversion_supported` accept native capability everywhere and, under `cfg!(target_os = "macos")`, a persistent pointer device. Add `current_native_scroll_inversion_supported`. Update `commit_invert_scroll` wording for either path. In the scrolling card, use the native description for native devices, a new source key `"Reverse this mouse's scroll wheel in macOS. Your trackpad keeps the system scroll direction."` for fallback, and the existing unavailable description otherwise. Add the same key to all locale files so parity tests remain valid; untranslated locales may temporarily use the English source string.

- [ ] **Step 4: Verify and format**

Run `cargo test -p openlogi-gui scroll_inversion`, `cargo test -p openlogi-gui locale_files_have_the_same_keys`, and `cargo fmt --all -- --check`.

Expected: focused GUI tests and locale parity pass. If GPUI fails because `xcrun` cannot find `metal`, preserve the exact environment failure and validate non-GPUI crates separately.

- [ ] **Step 5: Commit**

Run `git add crates/openlogi-gui/src/state.rs crates/openlogi-gui/src/app/detail.rs crates/openlogi-gui/locales` and `git commit -m "feat(gui): expose macos scroll inversion fallback"`.

### Task 5: Verification, documentation, and publication

**Files:**
- Verify: all modified files
- Include: `docs/superpowers/specs/2026-08-06-macos-scroll-inversion-fallback-design.md`
- Include: `docs/superpowers/plans/2026-08-06-macos-scroll-inversion-fallback.md`

- [ ] **Step 1: Run focused verification**

Run `cargo test -p openlogi-hook`, `cargo test -p openlogi-agent-core`, `cargo check -p openlogi-agent`, and `cargo fmt --all -- --check` with the task-local Rust toolchain and Command Line Tools SDK.

Expected: all commands pass.

- [ ] **Step 2: Run the workspace gate**

Run `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.

Expected: pass with full Xcode/Metal. If GPUI again reports that `xcrun` cannot find `metal`, record this environment limitation and do not claim the GUI workspace gate passed.

- [ ] **Step 3: Review scope and invariants**

Run:

```bash
git diff --check
git diff --stat upstream/master...HEAD
git diff upstream/master...HEAD -- crates/openlogi-hook crates/openlogi-agent-core crates/openlogi-agent crates/openlogi-gui docs/superpowers
```

Confirm native devices are excluded, unknown/trackpad events pass through, the original is dropped only after synthetic creation succeeds, no IPC/config schema changed, and no unrelated files are included.

- [ ] **Step 4: Commit documentation**

Run `git add docs/superpowers/specs/2026-08-06-macos-scroll-inversion-fallback-design.md docs/superpowers/plans/2026-08-06-macos-scroll-inversion-fallback.md` and `git commit -m "docs(scroll): document macos inversion fallback"`.

- [ ] **Step 5: Push without force**

Run `git push -u origin feat/macos-scroll-inversion-fallback`.

Expected: the branch is created on `zinego/OpenLogi`. Never use a force-push option.

- [ ] **Step 6: Perform real-M650 acceptance separately**

Install/run the patched build with Accessibility and Input Monitoring permissions. Verify the M650 toggle is available; Off and On produce opposite vertical scrolling; trackpad direction is unchanged; horizontal scrolling is unchanged; and side-button mappings still work. Report this as hardware validation only after observing it.

- [ ] **Step 7: Draft the PR and wait for approval**

Prepare an English title/body explaining the M650 reproduction, native-capability semantics, fallback identity matching, automated checks, and Metal/hardware validation status. Show it to the user and wait for explicit approval before `gh pr create`.
