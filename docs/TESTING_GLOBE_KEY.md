# Keyboard keys and Globe / Fn holds

## Scope and prerequisites

Applies to keyboard-chord and double-click builds using IPC protocol 33.
Globe/Fn is macOS only; ordinary keyboard chords also support Linux and Windows. This is keyboard-event synthesis, not a voice
recognizer, microphone bridge, or system input-source configuration feature.

- Build and run the matching GUI and agent together. See [DEVELOPMENT.md](DEVELOPMENT.md).
  GUI builds require full Xcode with the Metal Toolchain; Command Line Tools
  suffice for the core/injector tests only.
- Grant the **agent** its normal Accessibility/input permissions. Use a dev
  profile so production configuration and permissions stay unchanged.
- Connect a Logitech mouse with a remappable button that reports both down and
  up (for example Back/Forward). Do not use a wheel pulse or capacitive tap.
- For voice testing, configure WeChat Input Method or another target tool to use
  **hold Fn to talk**, select its intended microphone, and focus a normal editable
  text field. Check for conflicting system/other-app Fn shortcuts yourself.
  OpenLogi does not read or change third-party configuration.

## UI and configuration

1. Open the mouse's button configuration, select a button and use **a single action**.
2. In the System category select **Hold Globe / Fn (macOS)**. In Simplified Chinese
   the label is **地球键 / Fn（按住，macOS）**. Searching `Fn` should find it.
3. Restart the dev app and agent. The binding must persist; TOML stores
   `Back = "HoldGlobeKey"` under the device's existing bindings table.
4. Test a per-app override, switch between that app and the default profile, and
   return to the original app. Verify each profile retains its own binding.
5. Inspect both light and dark UI: the Globe icon, label, selection indicator and
   scrolling must remain visible. Check the keyboard action picker too, since it
   shares the catalog. Gesture editors and Actions Ring must not offer this action.
6. On Linux/Windows the action must not be offered; a manually imported binding
   must log unsupported execution and must not inject a substitute key.

Failure: missing/untranslated label, blank icon, lost setting, incorrect profile,
cut-off controls, or offering the action in a context without a physical hold.

## Generic keyboard keys

The mouse action picker offers **Press keyboard keys…** and **Hold keyboard keys…**.
Both use the same checkbox-and-key-picker editor as double click. Select modifiers
with labeled checkboxes, choose the ordinary key from a searchable list, and inspect
the live chord preview. No special-symbol typing is needed. Supported examples include `T`,
`Fn` (also `Globe` or `🌐`), `Ctrl`, `Ctrl+Fn`, `Fn+T`, and `⌃⌥⇧T`.
A chord has zero or more modifiers and at most one ordinary key; modifier-only
chords are valid. Unknown keys, extra ordinary keys, and malformed separators fail
validation. Fn is a macOS modifier, never a fabricated USB usage; other platforms
reject the entire Fn-containing chord rather than send a partial shortcut.

`CustomShortcut` emits a balanced tap; `HoldShortcut` owns the down/up lifecycle.
Both share physical-key ownership, so releasing a tapped chord preserves modifiers
held by another binding. Legacy `HoldGlobeKey` configurations remain readable and
resolve to the same `KeyCombo::FN` input. Editing them writes the generic form.

## Double-click shortcut with a Globe hold

In the default mouse profile, select **Hold Globe / Fn (macOS)**, then use
**Double-click shortcut → Set shortcut…**. Check **Control**, **Option / Alt**,
and **Shift**, choose **T** from the ordinary-key list, and save. The preview
shows `Ctrl+Alt+Shift+T`. Saving an empty selection keeps the editor open.
Removing double click restores the original immediate Globe hold.

For an MX Anywhere 3S sensitivity switch, the device binding is:

```toml
[devices."<existing-device-key>".bindings.DpiToggle]
short = "None"
long = { HoldShortcut = "Fn" }
double_click = "Ctrl+Alt+Shift+T"
```

- Hold for **300 ms**: Globe/Fn goes down once, and stays down until release.
- Two short presses, with at most **200 ms from first release to second down**:
  the second release sends the shortcut exactly once, without Globe events.
- One short press: no output. A third click begins a new pair.
- Holding the second press triggers Globe/Fn and suppresses the double click.
- Configuration/profile changes, capture loss, and shutdown clear pending clicks
  and release owned held input. A queued release wins over an overdue hold timer;
  it must not produce a delayed Globe pulse after the physical button is up.
- Per-app action overrides still replace the whole default button binding.
- Existing short/long bindings without `double_click` retain their 500 ms delay.

Verify the shortcut at its destination and hold behavior on the physical mouse.
Check the inspector and shortcut dialog in light/dark appearances, invalid input,
Cancel/Escape, persistence after restart, and removal of double click.

## Physical and voice behavior

The immediate-start cases below apply when double click is disabled; with double
click enabled, expect the 300 ms hold threshold described above.
Run each case against the real mouse and each intended voice tool separately.
An observed Fn event is not proof that the tool began recording or inserted text.

| Case | Steps | Expected / failure criterion |
|---|---|---|
| Immediate start | Press the assigned button and begin speaking immediately. Hold 5–10 seconds, then release. | Recording begins without an OpenLogi long-press threshold; remains active while held; ends on release. Missing first words or remaining active is a failure. |
| Quick press | Press/release quickly, then repeat with a normal sentence. | One balanced Fn lifecycle per press; no delayed or stuck session. A very short press need not produce transcription. |
| Repeated sessions | Speak three distinct short phrases in rapid succession. | Each starts on its first press and ends once; no need for a second attempt. |
| Overlapping bindings | Bind two buttons to Fn. Press A, press B, release A, release B. | Only the first down and last up are injected; releasing A must not stop B's hold. |
| Held shortcut regression | Use another button's existing `HoldShortcut` while holding Fn; release in both orders. | Neither release clears the other's synthetic modifier flags. Account separately for shortcuts the target tool itself intercepts. |
| Focus/profile change | Change frontmost app or profile while holding, then release and retry. | Configuration invalidation releases the old output. The next press uses the new profile, with no stuck Fn. |
| Disconnect | Disconnect/power off the mouse during a hold; reconnect and retry. | When the capture backend reports cancellation, the hold releases. Record whether this hardware actually reports disconnect/lost release; do not infer it from unit tests. |
| Permission interruption | Revoke the agent's capture permission while held, restore it, and retry. | Capture interruption cleans up; no stuck voice session. Permission loss can also prevent OS event delivery, so observe the real target and collect logs. |
| Exit | Quit the agent normally during a hold; restart and retry. | Graceful shutdown releases held output. SIGKILL/process crash is outside RAII cleanup guarantees. |
| One-shot rejection | Try a hand-authored Actions Ring slot, pulse-only source, deferred gesture/short action, or direct `execute` with `HoldGlobeKey`. | Ring config is rejected; other unsupported contexts log rejection, not an instantaneous voice session. |
| Stable actions | Recheck native clicks, Back/Forward, Copy/Paste, volume and an existing held shortcut. | Behavior is unchanged after Fn use and after restart. |

OpenLogi only injects Fn events. It does **not** guarantee macOS's built-in
“Press Globe key to…” behaviors (input-source switching, Emoji or Dictation).
Test those separately if required; success with one voice tool does not prove them.

## Automated and opt-in event tests

```sh
cargo test --locked -p openlogi-core
cargo test --locked -p openlogi-inject
cargo test --locked -p openlogi-agent-core runtime
cargo test --locked -p openlogi-ipc --test wire_format
cargo test --locked -p openlogi-ui locale
cargo test --locked -p openlogi-desktop i18n
```

The injector's normal tests are host-free. The following opt-in test **posts real
Fn events** and can activate configured voice software; run it only in a prepared
macOS session with permission:

```sh
cargo test --locked -p openlogi-inject globe_live_event_tap_observes_balanced_guard_lifetimes -- --ignored
```

It exercises the production guard/injector, observes only OpenLogi-tagged Fn
`flagsChanged` events, and checks three rapid sessions with overlapping guards,
replacement and drop cleanup. It does not collect user keystrokes or text and
cannot prove physical mouse capture, audio, third-party recognition or text output.

## Logs and evidence

Record app/agent version, macOS version, mouse model/transport, target tool version,
profile, steps and timestamps. Save dev agent stdout/stderr with `RUST_LOG=info`.
Relevant lines include `HoldGlobeKey`, the process-local `press` correlation number,
terminal `reason`, and `Globe edge submission` with `phase` and `submitted`.
Rejections identify `pulse_only_source`, `deferred_trigger`,
`physical_release_required`, or `unsupported_platform`.

`submitted=true` means an OS event was posted, **not** that the target accepted it.
Record visible recording state and text insertion separately. Do not include user
speech/text, credentials or unrelated application data in shared evidence.

## Validation boundary

- Core, injector, lifecycle and wire tests establish their respective code contracts.
- The opt-in event tap establishes delivery of the production synthetic Fn edges.
- UI screenshots establish presentation only; a GUI build alone is insufficient.
- Real Logitech capture, permission interruption, WeChat Input Method voice input,
  audio integrity and text insertion require the manual cases above.

### Historical 0.8.3 verification (before double-click support)

The implementation was checked on an Apple Silicon macOS host with Command Line
Tools, using `DEVELOPER_DIR=/Library/Developer/CommandLineTools` to override the
repository's default Xcode path. No third-party settings or production app were
modified.

- `cargo test --locked -p openlogi-core -p openlogi-inject -p openlogi-agent-core -p openlogi-ipc -p openlogi-ui`:
  551 tests passed; the real-event test is ignored by default.
- The opt-in `globe_live_event_tap_observes_balanced_guard_lifetimes` test above:
  passed separately against the production injector (six balanced Fn edges).
- `cargo clippy --locked -p openlogi-core -p openlogi-inject -p openlogi-agent-core -p openlogi-ipc -p openlogi-ui --all-targets -- -D warnings`:
  passed. Dependencies `block` and `proc-macro-error2` emit Cargo future-incompatibility
  notices, not new project Clippy failures.
- `cargo clippy --locked --target x86_64-pc-windows-msvc -p openlogi-core -p openlogi-inject --all-targets -- -D warnings`:
  passed. This compiles the unsupported-platform behavior; it is not Windows runtime
  testing. Linux changes were reviewed against the base, not cross-compiled.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- Desktop validation was initially blocked because `gpui_macos` could not find
  the Metal compiler. This build blocker is resolved by the Xcode follow-up below.
- Agent-run real mouse/voice-tool cases above: **not executed**. The user's
  subsequent smoke-test confirmation is recorded below; it is not a completed
  hardware/voice-tool regression matrix.

### Xcode follow-up

Xcode 26.6 (17F113) was installed. `xcrun --find metal` initially found a launcher
but executing it reported a missing optional toolchain. After
`xcodebuild -downloadComponent MetalToolchain`, Metal Toolchain 17F109 was available
(`xcrun metal --version` reports 32023.883).

With `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer` and the matching
`SDKROOT`:

- `cargo test --locked -p openlogi-desktop`: **215 passed**, including the i18n,
  mouse picker, gesture filtering and Actions Ring catalog tests.
- `cargo clippy --locked -p openlogi-desktop --all-targets -- -D warnings`: passed;
  the same dependency future-incompatibility notices remain.
- `cargo run --locked -p openlogi-desktop`: built and ran the production GUI
  through the repository's dev-bundle runner, against `openlogi-agent-mock` with
  the canonical synthetic fixture. Separate XDG directories under
  the local `../OpenLogi-globe-key-evidence/runtime/` directory kept this session
  away from production settings and hardware capture. Both preview processes
  were stopped after inspection; the installed production app was left running.
- The Chinese Fn action and Globe icon were observed in the running light-theme
  action catalog at 1280 × 852 points. The Back-button inspector was also opened.
  This does not establish binding persistence, dark-theme presentation, keyboard
  picker interaction, per-app switching, or physical mouse/voice behavior; those
  manual cases remain pending. The baseline Sleep action logged a missing
  `action-icons/moon.svg` asset; this unrelated mapping was not changed.
- Screenshot (original PNG, 2560 × 1704):
  local evidence file `../OpenLogi-globe-key-evidence/screenshots/fn-action-light.png`.
  SHA-256: `a098b5649e234c2adb582c02024bb0fc36949b676123c03f2de0eb752f290705`.
- `OPENLOGI_LOCAL_CODESIGN_IDENTITY=- cargo run --locked -p xtask -- macos bundle --channel dev`:
  built a self-contained development app with the real agent, overlay and CLI at
  `target/release/bundle/osx/OpenLogi.app`.
  The native bundler verified the dev identities and component signatures.
  This is a **local ad-hoc-signed development build, not a notarized distribution**.
  The release-profile build also reported pre-existing unused imports in
  `windows/settings/diagnostics.rs`; that file is outside this change.

The agent did not launch the self-contained app against real hardware. To test it, first
quit the installed OpenLogi from its menu-bar Quit command (closing only the
settings window leaves its agent running), then open the development app directly
without overwriting `/Applications/OpenLogi.app`. Grant **OpenLogi Agent Dev** its
own permissions and configure the single-button action. Do not run both input
agents concurrently. No installation replacement, commit, push or release was
performed during this follow-up.

### User confirmation and local DMG

The user subsequently reported testing the feature successfully and requested a
DMG. Record this as **user-confirmed smoke testing**, not agent hardware testing:
no mouse model/transport, target-tool version, latency measurements, detailed
case list or logs accompanied that confirmation. Unspecified manual cases above
remain pending.

The DMG was created from that same already-tested app, without recompiling,
re-signing, changing bundle IDs, or changing its configuration namespace:

```sh
cargo run --locked -p xtask -- macos dmg \
  --app target/release/bundle/osx/OpenLogi.app \
  --output target/release/OpenLogi-0.8.3-globe-key-arm64-dev.dmg
```

`OPENLOGI_SIGN_IDENTITY` was unset for this local packaging operation. The DMG
uses the native branded background/layout and retains the app's existing ad-hoc
development signature. It is **not a Developer ID-signed or notarized release**.
No release was published, and the DMG is a local build artifact, not a Git source
file. Do not use its drag-to-Applications shortcut to overwrite a production
installation unintentionally.

- Artifact: `target/release/OpenLogi-0.8.3-globe-key-arm64-dev.dmg`
- Size: 18,794,241 bytes; Apple Silicon / arm64.
- SHA-256: `8ed81136f5f7f228e3759a04b8f474be0237d35469d33d363b0dbc34956af59f`
- Sidecar: `target/release/OpenLogi-0.8.3-globe-key-arm64-dev.dmg.sha256`
- `hdiutil verify`: passed.
- Read-only remount: all 36 app-bundle entries matched the source, including
  file hashes, permissions and symlink targets. Desktop, real Agent, Overlay
  and CLI executables were present; the Applications link was correct.
- `codesign --verify --deep --strict` on the mounted app and strict verification
  of both helper apps: passed. This proves signature integrity, not notarization.
- Packaged desktop executable SHA-256 remained
  `574ddc7e37565c7762852466475fff2732b8a241a75b0587a47b2a03fda56125`.
- The verification mount was detached after checking.

### Pre-push verification

Before pushing the feature branch, the full macOS local gate was run with
`RUSTFLAGS="-D warnings"` and the Xcode environment above:

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: passed.
- `cargo test --locked --workspace --all-targets`: **1,584 passed**, one opt-in
  real-event test ignored by default (already run separately above).
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --document-private-items --exclude openlogi-ui --exclude openlogi-desktop --exclude openlogi-overlay --exclude openlogi-agent`:
  passed.
- `cargo xtask ci wasm clippy-windows`: both jobs passed, none skipped. The Windows
  job is the repository's cross-lint proxy, not native full-workspace Windows CI.
- The documented `aarch64-unknown-linux-musl` cross-Clippy recipe for hook,
  injector, HID/HID++, core, agent/agent-core, IPC and permissions: passed.
- `typos --config .config/typos.toml .` and `git diff --check`: passed.

The first full test run exposed CLI fixture-test assertions hardcoded to protocol
v29/v30. They now assert the intentionally mismatched and required versions using
`PROTOCOL_VERSION`; the targeted test and full rerun passed. This is test-only:
no runtime logic or bytes in the already-verified App/DMG changed.

These checks do not establish macOS Intel runtime behavior, native Linux/Windows
CI, or completion of the manual hardware/voice-tool matrix.


### Double-click and keyboard editor verification (2026-09-17)

Rebased onto upstream master `b9c8fede`. On Apple Silicon macOS, with
`RUSTFLAGS="-D warnings"`:

- `cargo fmt --all -- --check` and full-workspace Clippy: passed.
- `cargo test --workspace --all-targets`: **1,695 passed**, one opt-in real-event
  test ignored by default. Includes 234 desktop tests, main-window dialog rendering, real checkbox interactions,
  299/300 ms hold boundaries, 199/200/201 ms click gaps, cancellation, second-click
  holds, modifier ownership, configuration round trips and wire-format goldens.
- Non-GUI rustdoc with `RUSTDOCFLAGS="-D warnings"`: passed.
- `cargo xtask ci wasm clippy-windows`: both passed, none skipped.
- The Linux-musl cross-Clippy recipe above: passed.
- Locale checks, `typos --config .config/typos.toml .`, and `git diff --check`: passed.

The additional opt-in Fn event-tap test was attempted separately and received no
injected events. Read-only preflight checks on the command process reported
Accessibility, event-posting and event-listening access as false. This is not a
successful live-event test; no permissions were changed. The installed agent has
its own identity, so the command process's result does not establish the agent's
permission state. Physical mouse timing and the destination application's response
still require the manual cases above.
