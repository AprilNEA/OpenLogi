---
name: openlogi-macos-permissions
description: Decide whether a macOS problem is a privacy-permission (TCC) problem, and act on it correctly. Use when triaging a report that no devices appear / the GUI is empty while `openlogi list` works / "Failed to open device" / "Pairing failed" on macOS; when a user asks which permission OpenLogi needs or why they were never prompted; when reasoning about Input Monitoring, Accessibility, kTCCServiceListenEvent, kTCCServiceAccessibility, code-signing identity, responsible process, or bundle identifiers; and before changing anything under openlogi-permissions, openlogi-hid/permissions.rs, the agent's launch/self-restart path, the GUI's settings permission rows, or the macOS bundling and signing code in xtask.
---

# macOS permissions (TCC) in OpenLogi

One sentence to keep: **TCC does not authorize processes, it authorizes
identities.** Almost every "permission bug" here is an identity bug — the user
ticked a box, but not for the identity that actually opens the device.

## 1. First decide whether it is TCC at all

Three unrelated failures reach the user as the same symptom (an empty device
list). Classify before doing anything else. The discriminator is one log line:
**did the channel open?**

| Agent log | Layer | TCC? |
|---|---|---|
| `HID++ candidate interfaces count=0` | enumeration | **No.** The device's HID++ collection never matched — unsupported device, or not connected. |
| `failed to open HID++ channel — retrying next tick error=Message("Failed to open device")` | open | **Yes, usually.** `IOHIDDeviceOpen` was denied. Same string also covers exclusive access and a device that vanished — see §5. |
| `opened HID++ channel` … then `Device::new failed` / `enumerate_features failed` with `Channel(Timeout)` or `report writer callback error: 0xE00002D6` | probe | **No.** The open succeeded, so permissions are fine. This is the async-hid macOS write bug (upstream `sidit77/async-hid#45`). |

`openlogi list` showing the device while the GUI does not is **not** evidence
either way: the CLI is a separate code-signing identity running its own HID
stack, so it can succeed where the agent fails, for either reason.

If there is no log at all, go to §3 — getting a log is the whole job.

## 2. The identity map

One install ships four TCC identities. Only one of them may hold device
permissions.

| Identity | Binary | Needs |
|---|---|---|
| `org.openlogi.agent` | `…/Contents/Library/LoginItems/OpenLogiAgent.app` | **Input Monitoring** (opens HID) and **Accessibility** (owns the event tap) |
| `org.openlogi.openlogi` | `…/Contents/MacOS/openlogi-desktop` | Camera only. It is a pure IPC client and needs neither of the above. |
| `openlogi` | `…/Contents/MacOS/openlogi` (embedded CLI) | Input Monitoring, because it opens HID directly today |
| `org.openlogi.overlay` | `…/LoginItems/OpenLogiOverlay.app` | Nothing |

Two consequences that drive most reports:

- The identity the user must grant is **OpenLogiAgent**, which lives inside the
  app bundle. The System Settings `+` picker will not browse into a bundle, so
  they have to use Go-to-Folder. Say this explicitly; do not tell someone to
  "grant OpenLogi permission".
- A grant to the GUI does nothing for the agent, and vice versa. There is no
  bundle-wide grant.
- **Every copy is its own identity.** A dev build (`org.openlogi.agent-dev`), a
  second install, or a bundle still sitting in `~/Downloads` each get their own
  row. Confirm which binary is actually running before trusting any grant — the
  diagnose script warns when the running agent is not the one being inspected.

## 3. Diagnose

Run `scripts/diagnose.sh` from this skill, or the same steps by hand. It is
read-only and safe to hand to a reporter.

The one step it cannot do for them: **the agent's log has nowhere to go.** The
generated LaunchAgent plist sets no `StandardOutPath`/`StandardErrorPath` and
there is no file logger, so launchd discards everything. Until that changes, the
only way to see the agent's own log is to stop it and run it in the foreground:

```sh
OPENLOGI_LOG=debug \
  /Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiAgent.app/Contents/MacOS/openlogi-agent
```

Note what that costs: run from a terminal, the agent's responsible process
becomes the terminal (§4), so the run you are observing is not the run that
failed. Compare identities before concluding anything from it.

Reading the TCC database directly is not an option for a normal user — it is
itself TCC-protected and returns `authorization denied` without Full Disk
Access.

## 4. Responsible process

macOS attributes a TCC request to the *responsible* process, which for a plain
child process is the parent. An agent spawned directly by the GUI therefore asks
with the **GUI's** identity, and the user's grant to `OpenLogiAgent` appears to
do nothing.

Two ways to break the chain, both already in the tree
(`openlogi-desktop/src/services/ipc.rs`):

- packaged helper → `/usr/bin/open -g -n <bundle>` (LaunchServices parents it
  under launchd, so it is its own responsible process)
- bare dev binary → `disclaim::Command` (wraps
  `responsibility_spawnattrs_setdisclaim`)

Never spawn a helper with plain `std::process::Command` on macOS. Check with:

```sh
sudo launchctl procinfo $(pgrep -x openlogi-agent) | grep -i responsible
```

It must name the agent itself. `OpenLogi.app` or `Terminal.app` there means
every grant on that machine is being ignored.

## 5. What the APIs actually do

- `IOHIDCheckAccess` — queries only. **Never prompts, and never registers the
  app in System Settings**, so an app that only ever calls this cannot be
  granted: the user has no row to tick.
- `IOHIDRequestAccess` — prompts. **Blocks the calling thread** until the user
  answers, so it must not run on the async runtime. It is also not real-time:
  after a grant or revoke the calling process keeps seeing the old answer until
  it restarts. That is why the agent calls
  `self_restart::relaunch_after_input_monitoring_grant()`.
- `IOHIDDeviceOpen` — **denial is silent**. There is no TCC-specific error; the
  failure is indistinguishable from exclusive access or a disconnect. Never
  report `Failed to open device` to a user as-is; check
  `openlogi_hid::permissions::has_access()` and say which one it was.

## 6. Invariants — do not break these

1. **Only the agent holds device permissions.** Any UI that reports permission
   state must read it from the agent over IPC, never by querying its own
   process. The GUI never opens a HID device, so its own grant means nothing.
2. **Every helper launch establishes its own responsible process** (§4), and the
   spawn result is checked — a silently failed `disclaim` leaves the agent
   running under the GUI's identity, which is invisible until a user reports it.
3. **Bundle identifiers are the TCC primary key.** Changing one voids every
   existing user grant. Releases 0.6.24–0.6.26 shipped `.dev` identifiers and
   did exactly that. The `Verify production bundle identities` step in
   `.github/workflows/build.yml` is load-bearing; do not weaken it.
4. **Sign inside-out.** The helper needs its own stable designated requirement
   so its grant survives updates; `--deep` cannot give it one. See
   `xtask/src/commands/macos/bundle/signing.rs`.
5. **Prompt from the process that needs the permission**, not from whichever
   process happens to be running. A TCC grant is scoped to the identity that
   asked.

## 7. Known-broken right now

Check these before filing a new diagnosis:

- The Settings → Input Monitoring row queries the **GUI's** grant, not the
  agent's (#606, PR #760 open). It can read "Granted" while the agent has
  nothing. The Accessibility row above it is already correct — it goes over IPC.
- The agent's launchd log has no destination (§3). Open.
- `disclaim`'s spawn result is discarded in `ipc.rs` (`.map(|_| ())`).

## 8. What this cannot fix

Say so plainly rather than promising a fix:

- Apple's `+` picker not browsing into bundles.
- `IOHIDDeviceOpen`'s silent denial — we can only infer it by checking access
  separately.
- A stale `tccd` decision that needs a full logout, or an MDM policy.
- Ad-hoc-signed local builds: their designated requirement is cdhash-based, so
  the grant goes stale on **every** rebuild. Use an Apple Development identity
  for dev bundles.
