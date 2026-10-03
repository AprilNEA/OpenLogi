# Peripheral setup

OpenLogi supports compiled device drivers, local TOML device descriptors, and sandboxed Wasm code plugins. The agent owns device access, configuration application, and cleanup. The desktop and CLI use the same agent inventory and settings.

## DJI Mic 3 on macOS

Connect the DJI Mic 3 USB receiver. Open its device page, select the linking button, save `F18`, then enable the mapping. Saving a target alone does not enable device writes. In Chinese, the control is labelled 「连接键」.

Short-press the transmitter's lower side button, number 5 in the [DJI manual](https://dl.djicdn.com/downloads/DJI%20Mic%203/202508282/DJI_Mic_3_User_Manual__EN.pdf). Do not use a long press or double press for this test. The verified event is a short Consumer Volume Increment pulse; it does not report the physical hold duration.

To bind Raycast, open Dictation → Commands → Dictate → Record Hotkey. Press the linking button inside the recording overlay. Raycast should record F18. OpenLogi does not modify Raycast settings. Other applications can bind F18 through their own shortcut controls.

The receiver matches USB VID `0x2CA3` / PID `0x4015`, with Consumer Control collection `0x0C/0x01`. The native mapping converts `0x0C/0xE9` to keyboard `0x07/0x6D`. F18's HID usage `0x6D` is different from CGEvent virtual keycode `79`.

The mapping applies to all connected receivers of this model. LocationID, RegistryID, and the unverified serial are not persistent device identities. These reports cannot distinguish TX1 from TX2. Volume Decrement is not mapped. Audio interfaces, pairing, firmware, and recording controls are not modified.

The macOS backend uses `/usr/bin/hidutil property` with mandatory VID/PID `--matching` and decimal JSON integers. No extra driver, virtual keyboard, permanent diagnostic listener, simulated input, or root process is required by this mapping path. A development sandbox's device-access denial does not establish a permission requirement for the installed app.

### Applied state and recovery

The device page distinguishes offline devices, failed writes, conflicts, and an applied mapping waiting for a short press. A successful property readback confirms configuration only. OpenLogi does not report Raycast success from that readback.

Disabling a mapping preserves unrelated entries. If OpenLogi replaced a same-source entry, it restores that original entry only while the source still equals OpenLogi's last value. An external change suspends automatic reapplication. Use the explicit conflict-resolution action after reviewing the current mapping; a subsequent enable captures a new baseline.

The agent journals restoration obligations before native writes. Restart recovery uses the same boot and live HID services as evidence, never as a persistent device key. Replugging creates a new attachment and captures its own baseline. Cleanup survives plugin removal and retains pending obligations while a device is unavailable.

The verified native setter updates all matching services together. If matching receivers have different mapping arrays or restoration requirements, OpenLogi reports a scope conflict without writing. `hidutil` has no demonstrated compare-and-swap operation: OpenLogi rereads and verifies writes, but concurrent independent writers still have a race window.

### Configuration and CLI

The desktop saves a schema 8 rule such as:

```toml
[[peripherals]]
id = "dji-linking-button"
enabled = true
scope = { kind = "model", model = "dji.mic3.rx" }
descriptor = "org.openlogi.dji-mic3-usb"

[peripherals.capabilities."input-remap/main"]
version = 1
bindings = { linking = { CustomShortcut = "F18" } }
```

With the agent running, inspect `session.endpoint` in `openlogi peripheral list`. Substitute that current value for `<endpoint>`:

```sh
openlogi peripheral list
openlogi peripheral bind '<endpoint>' input-remap/main linking F18
openlogi peripheral enabled '<endpoint>' true
openlogi peripheral enabled '<endpoint>' false
openlogi peripheral retry '<endpoint>'
openlogi peripheral resolve dji-linking-button
openlogi peripheral reload
```

The rule ID can differ when the desktop created it. Read the saved configuration before resolving a conflict by ID. Offline configuration remains editable; application waits for the device.

## Loadable descriptors

Place `.device.toml` files in the `devices.d` directory beside `config.toml`, then run `openlogi peripheral reload`. Descriptors contain exact endpoint selectors and parameters for an available driver; they contain no executable code. Files load on startup and explicit reload.

Use [media-button.device.toml](../examples/descriptors/media-button.device.toml) as a complete native-mapping example. Its VID/PID is synthetic. Replace those values only with verified device facts. The built-in native mapper is available on macOS. Missing exact interface or report metadata never satisfies a selector.

Selection uses strict selector refinement. Equal or incomparable candidates cause a conflict before probe or write. A saved rule selects an exact descriptor. An unavailable selected descriptor causes an error, without selecting a replacement silently. Invalid edits retain the last accepted source and show a diagnostic; independent valid files can still activate.

Existing HID++, Litra, and camera drivers participate in the same catalog. They remain selected unless a saved rule explicitly chooses a replacement. Before replacing a HID++ or Litra owner, the agent withdraws runtime dispatch, restores temporary firmware state, waits for admitted I/O, and retires the old channel. The replacement cannot open the device before those acknowledgements complete. A model-scoped rule must name a descriptor for that same model; a session-scoped rule can select another model while retaining the current attachment.

Composite HID grouping uses USB device ancestry on macOS, USB sysfs ancestry on Linux, and the Plug and Play container on Windows. These topology identifiers are connection metadata, not saved physical identities. Missing exact interface or report metadata remains unavailable. External camera descriptors can reuse the native UVC control adapter for supported Logitech cameras. Camera discovery and the macOS control backend still restrict this adapter to Logitech; a descriptor alone cannot add another camera vendor. V1 Wasm plugins cannot access UVC controls or camera capture streams. Camera preview stays on the existing native media path.

## Wasm plugins

A local plugin package contains `plugin.toml`, referenced `.device.toml` files, and a standard Wasm component. Use the [counter-button example](../examples/plugins/counter-button/README.md) to build a stateful decoder without rebuilding OpenLogi. The canonical ABI is [openlogi:peripheral/driver@1.0.0](../crates/openlogi-plugin/wit/peripheral.wit).

Open Settings → Plugins, enter the package directory, and install it. Review the exact digest, device selectors, and requested operations. Select the descriptors to grant, then enable the package. Installing or listing a package does not grant device access. A package with no connected matching device can be enabled without an active session.

The equivalent CLI workflow is:

```sh
openlogi peripheral plugin install /absolute/path/to/package
openlogi peripheral list
openlogi peripheral plugin enable '<digest>' --descriptor dev.example.counter-button.test-device
openlogi peripheral plugin disable dev.example.counter-button
openlogi peripheral plugin rollback dev.example.counter-button
openlogi peripheral plugin remove '<digest>'
```

Use the descriptor ID and digest printed by `peripheral list`. Grants bind the complete descriptor fingerprint to an exact package digest. Changed matching rules or permissions require another grant. Upgrades run settings migration on a copy without device services. Failed activation restores the previous selection unless a concurrent configuration edit prevents restoration; that edit is preserved. Rollback restores the retained package and its settings. Removing a package preserves unavailable settings and host-owned recovery obligations.

The host runs Wasmtime 49.0.2 with `std`, `runtime`, `component-model`, `cranelift`, and `pulley`, explicitly targeting interpreted Pulley bytecode. Wasmtime and Cranelift use `Apache-2.0 WITH LLVM-exception`; `cap-std` 4.0.3 provides confined package reads under `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT`. No WASI imports, ambient filesystem/network access, native SDK loading, or plugin UI code are provided. `cranelift` compiles Wasm to Pulley bytecode here, not native JIT code.

Each device session has a separate store. Four bounded workers execute guests away from the input hook. Limits include 16 stores, 64 MiB linear memory per store, 256 MiB aggregate linear memory, 1,000,000 fuel units and a 100 ms deadline per guest call, 256 queued events per session, and separately bounded host I/O. A trap or quota failure revokes the affected session and cancels its queued input. Runtime bugs and process-wide allocation failures are not isolated from the agent process.

A native HID deadline reports an uncertain outcome and faults the session. The host retains submitted native work, buffers, and the device claim until the operation actually completes. Cleanup releases the claim through the existing inventory reconciliation; a timeout does not authorize a second driver to open the same device.

V1 plugins expose logical input controls and bounded scalar settings. Declared native key effects go through the same host mapping journal. Existing specialized HID++, light, and camera panels remain available through the normalized capability inventory. The GUI and overlay do not link the Wasmtime runner.

## Storage and validation

Production paths follow OpenLogi's existing XDG directories:

| Content | Default path on macOS/Linux |
| --- | --- |
| Desired settings | `~/.config/openlogi/config.toml` |
| Descriptors | `~/.config/openlogi/devices.d/` |
| Installed immutable packages | `~/.local/share/openlogi/plugins/<digest>/` |
| Grants, rollback state, native recovery | `~/.local/state/openlogi/peripherals/` |

The `dev` profile uses `openlogi-dev` directories. Do not delete recovery state to clear an error: the journal can contain the original mapping needed for safe restoration.

Code tests verify descriptor selection, package admission, actual component execution and fault containment, input cancellation, configuration migration, native merge/restore, and IPC bytes. Mock scenarios `mic-only`, `mic-offline`, `restore-conflict`, and `plugin-fault` exercise the desktop without device I/O:

```sh
OPENLOGI_PROFILE=dev cargo run -p openlogi-agent --bin openlogi-agent-mock -- --scenario mic-only
OPENLOGI_PROFILE=dev OPENLOGI_DEV_AGENT=0 cargo run -p openlogi-desktop
```

Hardware acceptance is separate. Use the [real DJI checklist](PERIPHERAL_ARCHITECTURE.md#real-dji-acceptance) to verify one F18 down/up pair, Raycast recording, original-mapping restoration, other devices, reconnect, agent restart, and microphone audio. A passing mock or mapping readback does not complete those checks.

The following developer checks use the production mapping manager and plugin runner:

```sh
cargo run -p openlogi-hid --example peripheral-inspect
cargo run -p openlogi-agent-core --example native-mapping-check -- /absolute/path/to/recovery.json
cargo run -p openlogi-plugin --features runner --example runtime-probe
```

`peripheral-inspect` only reads. `native-mapping-check` briefly maps the DJI linking button to F18, recreates the manager from the supplied journal, restores the original mapping, and compares service properties. Keep the journal if the command fails. Run the same command with `--restore` after the journal path to retry recovery. This check does not validate physical button presses or Raycast.

`runtime-probe` executes the example component against synthetic reports without opening hardware. It reports cold initialization, compilation, attachment, and worker round-trip latency. These measurements exclude device I/O and do not measure the native input hook.

To compare the existing hook callback against an earlier implementation, run the same ignored probe in each worktree:

```sh
cargo test -p openlogi-agent-core hook_button_latency -- --ignored --nocapture --test-threads=1
```

The probe captures dispatcher events without OS input injection. On macOS arm64, three alternating debug/test runs against design base `be23014a` produced these medians of each run's percentiles, in microseconds:

| Callback | Baseline p50 / p95 | Peripheral implementation p50 / p95 |
| --- | --- | --- |
| Button down | 1.667 / 3.667 | 1.666 / 3.000 |
| Button up | 1.625 / 3.583 | 1.584 / 2.917 |

Each run measured 10,000 callbacks after 2,000 warmups. These local measurements showed no material increase; they do not establish release performance or end-to-end device latency.

With Wasmtime 49.0.1, the example component processed 1,000 synthetic reports in an ad hoc hardened debug executable without JIT entitlements: 500 triggers, 219 µs p50 and 295 µs p95 worker round-trip latency. Initialization took 370 µs, compilation 289,124 µs, and attachment 1,617 µs. Maximum resident memory was 27,934,720 bytes; peak footprint was 9,552,328 bytes. This is not signed/notarized release-agent acceptance.

The production discovery adapter identified the attached DJI receiver and its HID interface 2 with a 3-byte maximum input report. The production mapping-manager probe applied F18, recreated its manager from the journal, restored the original empty mapping, and verified the original service arrays. Physical F18 events, Raycast recording, receiver replug, packaged application restart, unrelated keyboard behavior, and audio input remain unverified for this implementation. Native UI inspection is also pending: the computer-use connection fails with `Sky Computer Use native pipe startup failed`.
