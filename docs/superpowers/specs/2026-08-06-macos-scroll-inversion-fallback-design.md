# macOS Software Scroll Inversion Fallback

## Scope

OpenLogi will support per-device scroll inversion on macOS when a Logitech mouse does not report the native HID++ `0x2121 HiResWheel` inversion capability. Devices that report native inversion will keep the existing firmware-backed path. Linux and Windows behavior will not change.

The motivating device is a Signature M650 L connected directly over Bluetooth LE on macOS 26.5.1. OpenLogi 0.6.23 identifies it as `direct:046d:b02a:serial:2401lz05xrb8` and persists `pointer = true`, `scroll_inversion = false`, and `hires_wheel = false`. The installed CLI could not produce a raw feature table because macOS Input Monitoring permission belongs to the background agent rather than the standalone CLI, but both the GUI and persisted inventory agree that the device does not expose native inversion. The design therefore does not fabricate a `0x2121` capability or send unsupported HID++ writes.

## Capability Semantics

`Capabilities::scroll_inversion` will continue to mean that the device reports native HID++ wheel inversion. It will not be widened to mean that OpenLogi can provide a host-side fallback. This preserves the feature-table contract and keeps the native hardware writer correctly gated.

The GUI will compute whether inversion is available from the platform and the selected device. On macOS, a persistent pointer device may use the toggle whether or not native inversion is present. On other platforms, availability remains equal to `Capabilities::scroll_inversion`. The UI description will distinguish native inversion from the macOS software fallback so users understand that the fallback requires the Accessibility-backed input hook.

## Configuration and Agent Data Flow

The existing per-device `invert_scroll` field remains the only persisted setting. The GUI will continue to save the config and call the existing append-only `reload_config` RPC. No IPC type, method, protocol version, or config schema change is needed.

When configuration or inventory changes, the agent will keep applying native wheel modes only to devices whose `Capabilities::scroll_inversion` value is true. Independently, macOS agent state will publish a software inversion lookup for configured pointer devices whose native inversion capability is false. A device with native support must never enter the software lookup because applying both paths would restore the original direction.

The software lookup will use the physical identity available from the inventory and config. Vendor-and-product ID is the preferred exact match. A normalized product name, still constrained by vendor when both sides report one, is a fallback for event sources that do not expose a product ID. If two configured devices produce the same lookup identity with conflicting inversion values, that identity will be discarded rather than tied to the currently selected GUI device. A Bolt or Unifying receiver identity is published only when its inventory contains exactly one possible pointer; mixed native/software devices and incomplete capability probes therefore cannot leak one mouse's software setting to another mouse behind the shared receiver.

## macOS Event Handling

The current macOS hook already resolves an `IOHIDEvent` sender to an `EventDevice`, distinguishes trackpads from Logitech free-spin wheels, and reads point, fixed-point, and line scroll fields. The fallback will reuse this source attribution instead of phase-only detection, because a free-spin mouse wheel may carry phase fields that resemble a trackpad.

For each physical scroll event, the agent policy will first look up the source device. A trackpad event, an unknown source, an ambiguous identity, a device without inversion enabled, or a native-inversion device will pass through unchanged. A matched software-fallback mouse with `invert_scroll = true` will request a macOS-only inversion disposition.

The macOS hook will make an independent `CGEventCreateCopy` of the captured event, reverse all three vertical delta fields (line, fixed-point, and point), and preserve the horizontal fields plus phase, momentum, count, timestamp, and fractional precision. The copy will carry OpenLogi's existing synthetic-event marker and will be returned directly through `CallbackResult::Replace`, avoiding a second HID-tap pass. If copying fails, the original event will pass through so scrolling is not lost.

The Accessibility revocation and bounded run-loop state machine in `openlogi-hook/src/macos.rs` will remain structurally unchanged. The work will add the transform at the existing event-disposition boundary rather than restructure tap lifecycle code.

## User Interface

The Pointer tab will enable “Invert scroll direction” for the M650 and other persistent pointer devices on macOS. A native-capable device will retain the existing native description. A fallback device will explain that OpenLogi reverses the mouse wheel in software on macOS and leaves trackpad scrolling unchanged. Wheel resolution controls remain disabled when `hires_wheel` is false; software inversion does not imply resolution support.

## Failure Behavior

The fallback is deliberately fail-open. Missing identity, ambiguous matches, poisoned shared state, synthetic event allocation failure, or absent Accessibility permission leaves the original scroll event unchanged. These conditions may disable inversion, but they must not disable ordinary scrolling or affect a trackpad.

Native HID++ write errors remain on the existing logging and retry path. Software fallback does not mask or replace native support, and it does not address the separate native-inversion sleep/wake persistence report in issue #516.

## Automated Verification

Implementation will begin with failing tests. Agent-core tests will show that a macOS M650-like pointer device with `scroll_inversion = false` enters the software lookup while a native-capable device does not. Lookup tests will cover product-ID precedence, normalized-name fallback, unknown sources, and conflicting identities. Policy tests will cover enabled mouse events, disabled mouse events, trackpads, and zero vertical deltas.

Hook tests will cover synthetic-loop rejection, an independent replacement object, all three vertical delta encodings, fractional precision, and preservation of horizontal and scroll metadata. Agent tests will also cover vendor isolation and shared-receiver ambiguity. GUI state tests will show that macOS pointer devices can persist inversion without native capability, while non-macOS gating remains unchanged through cfg-specific helpers. Existing native wheel-mode tests must continue proving that unsupported devices receive no HID++ inversion write.

The local completion gate is `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`. macOS success does not prove Linux or Windows cfg-gated code; their behavior is intentionally unchanged and CI remains the authoritative cross-platform check.

## Hardware Acceptance

Automated tests can prove policy, matching, transformation, and regression behavior, but they cannot prove CoreGraphics attribution for this physical M650. Hardware acceptance will use a locally built OpenLogi app with the existing persistent macOS permissions. The M650 toggle must become available, enabling it must reverse vertical wheel scrolling, disabling it must restore the original direction, horizontal behavior must remain unchanged, and trackpad scrolling must remain unchanged. The test will also confirm that side-button mappings still work and that the event tap remains responsive after repeated toggles.

Any hardware observation will be reported separately from automated verification. No GitHub issue, pull request, comment, or other public artifact will be created without a separate English draft and explicit approval.
