---
name: contributing-device-fixtures
description: "Guides OpenLogi device-fixture capture, privacy review, and offline verification with fixture contribute and verify. Use when recording device profiles or HID++ cassettes, contributing hardware fixtures, or reviewing fixture corpus changes."
---

# Contribute Device Fixtures

Produce a sanitized, verifiable fixture without treating replay as proof of hardware correctness.

## Prepare the capture

1. Read the [fixture architecture and recording workflow](../../../docs/MOCK_DEVICE_TESTING.md).
   Check current CLI support before using proposed features from that design doc.
   Use the repository CLI with `cargo run -p openlogi -- <arguments>` if the installed
   `openlogi` lacks the fixture commands.
2. Choose a synthetic specimen ID and name; do not embed serials or contributor
   identity. Use a new output directory whose basename equals `--id`.
3. Identify one physical target and transport. `--device` accepts a case-insensitive
   exact display name or exact rendered route; ambiguous selection must fail.
   Keep the same physical device and route for both phases.
4. Match the CLI's `OPENLOGI_PROFILE` to the agent that owns the target. Confirm
   permission to interrupt the user's agent/apps and perform direct capture before
   entering the raw phase. Use `--profile-only` when direct capture is not authorized.
   Load the [macOS permission skill](../../../.claude/skills/openlogi-macos-permissions/SKILL.md)
   on macOS; an agent grant does not authorize direct CLI access.

## Capture through the existing wizard

With a compatible real agent running, use this command with the selected values:

```sh
openlogi fixture contribute \
  --id mx-master-3s-001 \
  --name "MX Master 3S" \
  --device "MX Master 3S" \
  --output fixtures/devices/mx-master-3s-001
```

- Phase one reads semantic state through the agent; there is no direct fallback.
  Check that it is the real target's snapshot, not the mock agent's inventory.
  `--profile-only` finishes here; raw-HID standalone devices also use that mode.
- For HID++ raw capture, stop the matching agent and competing hardware clients,
  then rerun the same command. Inspect other profiles too: `agent.lock` protects
  only its own profile, and does not exclude Options+ or other direct diagnostics.
- Let the recorder hold `agent.lock` and reject an active or unhealthy IPC endpoint.
  Do not delete locks, bypass ownership checks, or change profiles to evade them.
- The current wizard records eight named read operations. Discovery can first
  enable receiver notifications and request arrival reports; flags are not restored.
  Do not describe the session as zero-write. Do not add setting writes or pairing.
- Prefer the wizard over hand-built manifests. Use lower-level `fixture record`
  commands only for a focused case after reading their current `--help` and
  [implementation](../../../crates/openlogi-cli/src/cmd/fixture.rs).

## Handle failures without losing evidence

Read [publication and resume behavior](../../../crates/openlogi-cli/src/cmd/fixture/contribute.rs)
before recovering a failed capture. Files are published individually, not as one
atomic directory. Publication errors can leave a partial case set; the resume
state is removed before final on-disk verification.

- If capture stops before publication, preserve the saved profile and resume state.
  After resolving the cause, rerun with the same metadata and physical target.
- If publication or final verification fails, inspect the directory and error first.
  Do not promise automatic resume, delete existing output, or add `--force` blindly.
  The contribution wizard has no `--force`; lower-level record commands can replace files.
- Refuse ambiguous routes, unclassified sensitive reports, ownership conflicts, and
  interrupted captures. Do not weaken sanitization or fabricate a successful case.

## Verify and review before sharing

```sh
openlogi fixture verify fixtures/devices/mx-master-3s-001
cargo test -p openlogi-cli fixture::verify
```

Require schema, privacy, relationship, and replay checks to pass. Strict verification
accepts only the declared manifest, profile, and case files; exclude logs and resume
state from the final fixture. These checks are offline and do not access hardware.

Review the model, transport, capabilities, and expected values against independent
device evidence. Inspect sanitized JSON before sharing; do not persist raw identities,
pairing secrets, host paths, or hashes of private identifiers. Do not regenerate
expectations from replay output merely to make verification pass.

Report capture mode, CLI/agent versions, transport, verification results, and remaining
semantic or hardware uncertainty. Record limitations outside the strict fixture
directory. Restore only the processes stopped for the authorized capture. Do not
upload, commit, or open a PR unless the task authorizes that action.
