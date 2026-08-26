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
| `failed to open HID++ channel … Failed to open device: Input Monitoring is NOT granted to this process…` | open | **Yes.** The message classifies itself — grant Input Monitoring to the identity named in §2. |
| `… Failed to open device: Input Monitoring is granted to this process — another app may hold the device exclusively, or macOS is serving a stale permission session (log out and back in)` | open | **The grant is fine.** Quit the other app (usually Logi Options+), or log out and back in. See §5. |
| `opened HID++ channel` … then `Device::new failed` / `enumerate_features failed` with `Channel(Timeout)` or `report writer callback error: 0xE00002D6` | probe | **No.** The open succeeded, so permissions are fine. This is the async-hid macOS write bug (upstream `sidit77/async-hid#45`). |

All of these lines are `debug`-level except the open failure, which is a
`warn`. A log captured without `OPENLOGI_LOG=debug` can only ever show the
middle row — in such a log, the absence of the other lines is not evidence
of anything.

`openlogi list` showing the device while the GUI does not is **not** evidence
either way: the CLI is a separate code-signing identity running its own HID
stack, so it can succeed where the agent fails, for either reason.

If there is no log at all, go to §3 — getting a log is the whole job.

## 2. The identity map

One install ships four TCC identities. Only one of them may hold device
permissions.

| Identity | Binary | Needs |
|---|---|---|
| `org.openlogi.agent` | `…/Contents/Library/LoginItems/OpenLogi Agent.app` | **Input Monitoring** (opens HID) and **Accessibility** (owns the event tap) |
| `org.openlogi.openlogi` | `…/Contents/MacOS/openlogi-desktop` | Camera only. It is a pure IPC client and needs neither of the above. |
| `openlogi` | `…/Contents/MacOS/openlogi` (embedded CLI) | Input Monitoring, because it opens HID directly today |
| `org.openlogi.overlay` | `…/LoginItems/OpenLogiOverlay.app` | Nothing |

Two consequences that drive most reports:

- The identity the user must grant is **OpenLogi Agent**, which lives inside the
  app bundle. Bundles built before the rename spell that directory
  `OpenLogiAgent.app`; both are the same identity (`org.openlogi.agent`), and
  the grant survived the rename because TCC keys on the identifier, not the
  path. The System Settings `+` picker will not browse into a bundle, so
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

**Ask for the agent's log file first.** launchd discards the agent's stderr,
so the agent also writes a daily-rotated file (7 kept) a reporter can attach:

```sh
case ${XDG_STATE_HOME:-} in
  /*) state_home=$XDG_STATE_HOME ;;
  *) state_home=$HOME/.local/state ;;
esac
ls "$state_home/openlogi/"
```

It carries panics too. Only when that file is missing or predates the failure
is a foreground run worth its cost:

```sh
OPENLOGI_LOG=debug \
  "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent"
```

Note what that costs: run from a terminal, the agent's responsible process
becomes the terminal (§4), so the run you are observing is not the run that
failed. Compare identities before concluding anything from it — in
particular, a successful open under the terminal's grant does not clear the
copy launchd runs.

Reading the TCC database directly is not an option for a normal user — it is
itself TCC-protected and returns `authorization denied` without Full Disk
Access.

## 4. Responsible process

macOS attributes a TCC request to the *responsible* process, which for a plain
child process is the parent. An agent spawned directly by the GUI therefore asks
with the **GUI's** identity, and the user's grant to `OpenLogi Agent` appears
to do nothing.

Three ways to break the chain, all in the tree
(`openlogi-desktop/src/services/ipc.rs`), tried in this order:

- registered login item → `launchctl kickstart gui/<uid>/<service label>`
  (launchd spawns it directly: its own responsible process, plus crash
  respawn from the service plist's `KeepAlive`). The registration itself is
  `SMAppService` in `openlogi-desktop/src/platform/login_item.rs`, driven by
  the `launch_at_login` setting.
- packaged helper, not registered → `/usr/bin/open -g -n <bundle>`
  (LaunchServices parents it under launchd, so it is its own responsible
  process — but unsupervised)
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
  `binary_watch::relaunch_after_input_monitoring_grant()`.
- `IOHIDDeviceOpen` — **denial is silent**. There is no TCC-specific error, so
  the transport pairs every open failure with
  `openlogi_hid::permissions::has_access()` and says which case it is (§1).
  Keep it that way: a bare `Failed to open device` is not reportable.

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

Nothing in the launch, permission-reporting, or diagnosis path is known
broken right now: the Settings row reads the agent's own grant over IPC
(#606), the agent writes a log file (§3), open failures classify themselves
(§1), and the `open -g -n` handoff checks its exit status. This is a
snapshot — re-check the tracker before telling anyone a gap is still open.

## 8. What this cannot fix

Say so plainly rather than promising a fix:

- Apple's `+` picker not browsing into bundles.
- `IOHIDDeviceOpen`'s silent denial — we can only infer it by checking access
  separately.
- A stale `tccd` decision that needs a full logout, or an MDM policy.
- Ad-hoc-signed local builds: their designated requirement is cdhash-based, so
  the grant goes stale on **every** rebuild. Use an Apple Development identity
  for dev bundles.
