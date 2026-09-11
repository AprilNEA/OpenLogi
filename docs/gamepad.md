# Virtual gamepad (auxiliary mouse → controller)

OpenLogi can publish an opt-in OS-visible standard HID gamepad for pointing
devices that expose remappable extras (MX Master–class Back/Forward, gesture
button, haptic panel, thumb wheel). The physical pointer and primary clicks
stay native.

## Enable

```toml
[devices."unit:…".gamepad]
enabled = true
rumble = true
```

Reload config (GUI save or agent restart). Creation failures are logged; there
is no desktop toggle in this first cut.

## Platform notes

| OS | Backend | Runtime requirement |
|---|---|---|
| macOS | `IOHIDUserDevice` | Agent must be codesigned with `com.apple.developer.hid.virtual.device` (Apple-restricted). See [`OpenLogiAgent.entitlements`](../crates/openlogi-agent/bundle/OpenLogiAgent.entitlements). Without it, create returns a clear entitlement error. Host→device rumble is not wired on macOS yet (input still works). |
| Linux | `uinput` joystick + optional `FF_RUMBLE` | User needs write access to `/dev/uinput` (same class of permission as OpenLogi's existing inject path). |
| Windows | ViGEmBus (stub in this PR) | Install [ViGEmBus](https://github.com/nefarius/ViGEmBus); until the client is wired the agent soft-fails with "driver missing". |

## Browser Gamepad API

Chromium only exposes pads after a user gesture (press a mapped button). Expect
`mapping: "standard"` when the report descriptor usages are conventional.

## Rumble → haptics

When `rumble = true` and the device has HID++ `0x19b0`, host dual-rumble is
translated to `DampStateChange` / `SubtleCollision` waveforms.
