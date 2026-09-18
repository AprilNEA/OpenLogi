---
name: diagnosing-openlogi-devices
description: "Diagnoses OpenLogi HID/HID++ failures across enumeration, open, probe, IPC, and UI layers. Use for missing devices, failed opens or pairing, stale inventory, disconnect/reconnect failures, unsupported controls, or CLI/GUI disagreement."
---

# Diagnose OpenLogi Devices

Find the first failing layer before proposing a permission change or code fix.

## Establish the observation

1. Record the app/agent/CLI versions, OS, installation source, and active profile.
   Record the model and connection type: Bolt, Unifying, Bluetooth-direct, or wired.
   Distinguish an absent receiver from a paired device that is asleep or offline.
2. Read the existing report and logs before asking for new captures. Record the
   failing action, expected result, and whether the failure repeats.
3. Compare `openlogi list` stdout **and stderr** with the GUI, using the same profile.
   Read [list provenance](../../../crates/openlogi-cli/src/cmd/list.rs) if ambiguous:
   - `inventory read from the running agent` uses the GUI's HID inventory source.
   - Direct fallback uses the CLI's own permission identity and HID stack.
     A timeout or incompatible protocol can cause fallback, not just an absent agent.
   - Camera enumeration is direct in both modes; do not infer HID permission from it.
4. Use the failing agent's logs. If more detail is required, use `OPENLOGI_LOG=debug`
   with the correct launch path. A new foreground process can change permission
   attribution. Missing debug lines in an info-level log prove nothing.

## Locate the failure

| Evidence | Investigate next |
| --- | --- |
| No matching HID interfaces | Connection, model identity, transport, registry/enumeration filters |
| Interface found, open fails | Permission status of the opener, exclusive ownership, host transport |
| Channel opens, feature probe times out or fails | HID++ request/response and transport; do not reset permissions by default |
| Direct CLI works, agent inventory fails | Compare process identity, versions, profiles, and agent logs |
| Compatible agent snapshot has the device, GUI does not | IPC delivery, desktop state, capability filtering, presentation |
| Device appears, one feature fails | Reported feature ID/version, capability gating, then the owning operation |

For macOS symptoms, load the existing
[permission skill](../../../.claude/skills/openlogi-macos-permissions/SKILL.md).
Use its identity map and read-only diagnosis; do not duplicate TCC procedures.
For Linux access failures, inspect the relevant `hidraw`, `input/event`, and
`uinput` permissions using [Linux access guidance](../../../docs/INSTALL-linux.md).
Do not install rules, change ACLs, reset permissions, or stop another app without
authorization. Do not diagnose a Windows failure from a macOS permission model.

## Choose the smallest diagnostic

Read the [diagnostic command definitions](../../../crates/openlogi-cli/src/cmd/diag.rs)
and the selected subcommand before running it. Explain direct hardware access and
coordinate with competing clients. Do not present enumeration as a zero-write session:
receiver discovery can enable notifications and request arrival reports.

| Diagnostic | Boundary |
| --- | --- |
| `openlogi diag features` | Reads feature/firmware tables for all online devices; has no `--device` flag |
| `diag controls`, `diag battery` | Read the selected device; use `--device` and inspect the printed route |
| `diag wheel` without `--resolution` | Reads wheel mode; adding `--resolution` writes hardware |
| `diag dpi` | Writes a test DPI and attempts restoration; failures can prevent restoration |
| `diag smartshift` without `--sensitivity` or `--leave-flipped` | Toggles mode and attempts restoration; failures can prevent restoration |
| `diag smartshift --sensitivity N` | Sets sensitivity without restoring the previous value; preserves the current mode |
| `diag smartshift --leave-flipped` | Toggles mode and intentionally skips restoration |
| `diag lighting` | Writes lighting; not an observation-only diagnostic |

Diagnostic `--device` uses the first case-insensitive substring match. If names
overlap, do not treat it as unique selection. Resolve the target before writes.
Get authorization for setting writes or pairing/unpairing; do not run them as
default triage. Stop if ownership, target selection, or restoration is uncertain.

## Reproduce in the owning layer

Use [change verification](../verifying-openlogi-changes/SKILL.md#reproduce-regressions-before-fixing-them)
for the before/after test requirements. Start with an existing test seam:

- Enumeration, stale channels, and recovery: [device replay tests](../../../crates/openlogi-device/src/inventory/replay_tests.rs).
- Agent lifecycle: [injected inventory tests](../../../crates/openlogi-agent-core/src/watchers/inventory/replay_tests.rs)
  or [pairing tests](../../../crates/openlogi-agent-core/src/watchers/pairing/replay_tests.rs).
- IPC delivery and replacement: [IPC client tests](../../../crates/openlogi-ipc/src/client.rs)
  or [desktop IPC tests](../../../crates/openlogi-desktop/src/services/ipc.rs).
- Presentation only: [UI testing](../testing-openlogi-ui/SKILL.md), not a second mock framework.

Keep node presence, channel connectivity, paired-slot state, and published inventory
distinct. Assert intermediate cleanup and replacement, not only the final online state.

## Return actionable evidence

- Separate observed facts, suspected cause, and the next discriminating check.
  Quote the decisive error with its process, profile, and transport context.
- Redact serials, receiver IDs, Bluetooth addresses, pairing secrets, usernames,
  and host paths before sharing. Preserve protocol fields needed for diagnosis.
  Do not request full config or raw traffic when a sanitized excerpt is enough.
- For code changes, follow the owning crate's rules and
  [verification workflow](../verifying-openlogi-changes/SKILL.md).
  Use sanitized fixtures; derive expected values independently of the parser under test.
- If a new capture is justified, use
  [fixture contribution](../contributing-device-fixtures/SKILL.md).
  Report hardware verification separately. Checks without the affected physical
  device do not establish that the physical failure is fixed.
