# Disable Keys save retry projections

Approved by the user on October 23, 2026, following review comment 4359263717.

## Owner and invariant

`ConfigState::refresh_feature` adopts the entire external configuration. The
owning `AppState` must then rebuild its existing config-derived projections and
announce them through `StateEvents`. Updating only the Disable Keys card leaves
the keyboard binding map stale, even after ordinary inventory refreshes.

Reuse `restore_config_projections`; do not add another binding cache or copy its
rebuild logic. Return events from the retry mutator and emit them via `AppState::apply`.
After a successful refresh, announce settings and the current device's bindings
and DPI projections, alongside the originating device's Disable Keys status.
This must also hold when the following save fails: the refreshed revision is
already the rollback authority. A failed refresh must retain old projections and
announce only the local recovery error. No recovery token means no work or events.

Keep the confirmed Disable Keys choice and feature-local recovery status. Do not
repeat HID writes, change IPC, reset editor selection, or force a device switch.
When selection changes before retry, binding events belong to the device now on
screen, not the device that originated the save failure.

## Proof and scope

Drive an actual tracked temporary config through external F-key edits, a failed
feature save, and the production retry action. Assert the public binding reader,
saved config, emitted events, and a single reload command without a second write.
Cover failed refresh, retry without recovery, changed selection, and closed IPC.
Use a GPUI context test to verify event delivery, in addition to state tests.
Run the regression red before production changes, then green and nearby tests.
No native layout or hardware behavior changes are intended; automated state/event
tests are not physical keyboard or native screenshot verification.
