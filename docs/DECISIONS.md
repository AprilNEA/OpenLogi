# Decision log

Durable "why we did it this way" records that are not obvious from the code.
Add a dated entry when a non-obvious architectural or dependency decision is
made or revisited.

## 2026-10: One application on every platform is an `[apps]` alias

A per-app profile was keyed by the identifier one platform reports: a macOS
bundle id, a Linux application id, or a Windows executable path. The same
application has a different identifier on each platform, so a profile written
on one did not apply on another. Logi Options+ 2.8 solves this with one
`applicationId` and a list of detection rules per platform
(`applications.json`). OpenLogi adds the same layer as a table in
`config.toml`, and accepts the config format change (schema 9).

- **A separate table, referenced by key.** `[apps.<alias>]` lists the
  identifiers of one application per platform. A profile references it as
  `app:<alias>`, the same way the existing `exe:<filename>` key names an
  executable. Profiles keyed by a platform identifier keep working unchanged,
  so no existing file needs to move. Later features that select an
  application, such as scene templates, can take the same `app:` key.
- **The most specific key wins.** The matcher tries the exact identifier, then
  `exe:<filename>`, then the alias that lists the exact identifier, then the
  alias that lists `exe:<filename>`. A profile keyed by the platform's own
  identifier therefore wins over an alias profile. That lets one platform
  differ from a shared profile.
- **One alias per identifier.** Two aliases that list the same identifier for
  one platform stop the load, because the matcher could not choose between
  them. An identifier can repeat across platforms.
- **An undefined alias stops the load.** A misspelled `app:` key would match
  nothing and silently do nothing, so loading rejects it, as the strict schema
  rejects other mistakes. Windows identifiers must be lower-case for the same
  reason: Windows reports lower-case paths.
- **One matcher.** `openlogi_core::app` owns both the `exe:` fallback and the
  alias lookup for button profiles and Actions Ring layouts, and an ast-grep
  rule keeps a second copy out.
- **The desktop app reads aliases but does not write them yet.** It shows an
  alias profile under the alias name and stops offering an application that
  already runs under a profile. Sharing a profile across platforms from the
  GUI, and a hint for an alias with no identifier on this platform, need new
  strings in every locale. A later change adds them.

Verified with unit tests, which CI runs on macOS, Linux, and Windows. Nobody
has used one config on two platforms with a real application yet.

## 2026-10: On Wayland, the ring opens a layer-shell surface on each display

A Wayland client cannot read the global cursor position or place its own
windows. Since #1004 the overlay has opened one ring window per display, and
each window draws the ring where the pointer enters it. On GNOME 50 with two
monitors, both rings appeared on one monitor (#1901). An ordinary window
cannot name its output, Mutter put both windows on one display, and each
window that heard the pointer drew a ring of its own.

- **Layer-shell surfaces first.** A `wlr-layer-shell` surface names its
  output. Each display gets one on the overlay layer, anchored to all four
  edges and without keyboard focus. The surface has no size of its own, so
  the compositor sizes it to the display. GPUI derives display bounds from an
  integer scale, which is wrong under fractional scaling, so the overlay does
  not use them here.
- **Ordinary windows as the fallback.** GNOME does not offer
  `wlr-layer-shell`, so there the overlay opens the maximized windows it
  opened before. The overlay remembers the first refusal for the life of the
  process, for rings and notices alike. GPUI binds the compositor's globals
  once, and each refused request leaves behind the `wl_surface` GPUI created
  for it.
- **One window draws the ring.** The windows of one ring share one record of
  which window heard the pointer last, and only that window draws the ring.
  When the windows are stacked on one display, the last to hear the pointer
  is the one on top, the only one a click can reach. A window that loses the
  ring also drops its hovered slot.

Verified with unit tests and a GPUI test that opens two ring windows for one
ring. Nobody has run the Wayland paths on screen yet. CI builds and tests
them on Linux.

## 2026-10: The lock-key indicator follows Logi Options+ 2.8, without Fn Lock

Logi Options+ 2.8 shows lock-key changes in its own on-screen indicator. Its
`0x4220` handler compares each broadcast bitmask with the last one and posts
one indicator per changed bit. Its `osd_resources.json` shows Caps Lock on
every platform and Num Lock and Scroll Lock on Windows only. OpenLogi follows
both rules.

- **A baseline before the first change.** The agent reads the keyboard's lock
  state (`GetLockState`) when it learns the device, and announces only bits
  that change after that. Num Lock is usually on under Windows, so a keyboard
  that connects must not announce it. A keyboard that goes offline, or that
  reconnects between two scans, loses its baseline and is read again. Options+
  probably reads a baseline at start too; that part of the disassembly is not
  verified.
- **The agent sees every change; the indicator shows the latest.** The device
  layer hands changes to the agent in a small queue, not a latest-value cell,
  so a change the platform rule drops (Num Lock on macOS) cannot hide one it
  keeps (Caps Lock) reported with it. The indicator shows only the latest
  change, so presses in quick succession can skip the states between them.
- **The agent owns the indicator.** It shows a change for 1.5 seconds, the
  backlight indicator's time, and a newer change replaces it. The overlay
  hides the indicator when the connection to the agent drops, as it does for
  notices.
- **On by default.** One global switch, `lock_key_notifications`, turns it
  off. The Options+ default for its setting was not found in the static
  analysis.
- **No Fn Lock yet.** Options+ also shows "Media Keys" and "F-Keys". OpenLogi
  writes the Fn Lock state back to a keyboard when it reconnects, and the
  keyboard reports that write like a key press, so the indicator would show
  on every reconnect. That needs its own decision.

Verified with unit tests. Nobody has seen the indicator on screen with a real
keyboard yet.

## 2026-10: Low-battery notices are drawn by the overlay, not sent as OS notifications

Logi Options+ 2.8 shows a low battery in its own on-screen toast
(`logi::overlay::battery_notification`, the `low_battery` entry of its
`notification_resources.json`), on by default. It sends OS notifications only
for macros and integrations. OpenLogi follows the same split.

- **No notification permission.** An OS notification needs authorization on
  macOS, a registered app identity on Windows, and a notification daemon on
  Linux. The overlay runs whenever the agent is armed, with or without the
  settings app, and already places windows per display.
- **One rule for low.** `BatteryInfo::needs_attention` in `openlogi-core`
  decides it for the agent's notice and the settings app's battery colour, and
  an ast-grep rule keeps a second copy out.
- **The agent owns a notice.** It decides when to post one and when it ends
  (8 seconds), so an overlay that starts or reconnects sees only the notices
  that are current. Notices travel in their own observation cell
  (`observe_notices`), not in the agent snapshot, so a notice does not wake
  every state observer. When the connection to the agent drops, the overlay
  clears the cards at once, because nothing else would ever end them. The
  backlight indicator follows the same rule.
- **Once per discharge.** A device is announced when it becomes low, and
  again only after it charges or reads 30% or more, so a reading that wobbles
  around 20% does not repeat the notice.
- **Not click-through.** The card sits at the bottom centre of the usable area
  of the display under the cursor (macOS visible frame, Windows work area,
  Wayland layer-shell surface anchored to the bottom edge, X11 display). It
  opens without taking focus, and a click closes it.

Verified with unit tests, and on screen on macOS against the mock agent's
demo: the card appears at the bottom centre above the Dock and closes after 8
seconds. A click to close is covered by unit tests only. The Windows and Linux
windows are checked only by CI builds; nobody has seen them on screen yet.

## 2026-10: Litra is reached over HID++ `0x1990`, with the raw report as fallback

The "raw" Litra report `[0x11, 0xff, 0x04, …]` was always a HID++ 2.0 long
report: device `0xff`, feature index 4, the `0x1990 Illumination` setters.
Hard-coding the index sent writes to the wrong feature on any firmware that
lists `0x1990` elsewhere, and nothing was ever read back. Logi Options+ 2.8
resolves the index through the root feature, reads the light's ranges and
state, and writes a value only when it differs from the one it read.

- **HID++ first.** A command opens the light's own node as HID++, pings it,
  looks `0x1990` up through the root feature and reads the light's ranges and
  state. It writes only a changed value, reads the written control back, and
  checks values against the ranges the light reports rather than the static
  per-model ones.
- **The effective maximum is gated.** `getBrightnessEffectiveMax` is read only
  for feature version 1 with the dynamic-maximum flag set, as the
  specification and Options+ gate it.
- **The raw report stays as the fallback.** It is the path every earlier
  release shipped and the only one proven on hardware. A light that opens but
  does not answer the ping, list `0x1990` or have its state read within one
  second gets it instead; without the deadline it would hold the command for
  the channel's five-second request timeout. That answer is remembered per
  route and OS node for the life of the process, so a light that comes back as
  a different OS node is asked again. A refused open says nothing about the
  light, so it falls back for that command only.
- **No raw retry after a HID++ write.** Once the command has gone out over
  HID++, a failure is returned rather than repeated as a raw report.
- **One link per light.** The agent keeps the light's HID++ channel open and
  listens on it, so commands and notifications share one channel and the
  feature lookup runs once. A channel that stops answering is replaced on the
  next command, and the listener follows the new one.
- **A change on the light is the new setting.** Options+ broadcasts the state
  a `0x1990` notification reports and stores it as the light's settings. The
  agent keeps a value changed with the light's buttons or by another program
  in place of the saved one, so a reconnect writes it back instead of undoing
  it, and publishes it so the settings app can save it. A save of that control
  or a write from OpenLogi replaces it; the power of a camera-linked light is
  a manual choice until the next camera transition instead. The desktop app
  saves each published change, except on a light with one of its own writes in
  flight. The agent takes the controls a write sets before it writes and gives
  them back only if the write fails, so a state published during the write
  never offers the old change, and the app cannot save it over the value the
  user just chose. The agent holds what a write took until the write settles,
  so a reload during the write replaces it as it replaces any change, and a
  failed write gives back only what is left. The app reloads its own saves
  with `reload_saved_config`.
  It writes each control it changes before it saves it, and the write takes
  the change, so a saved value that differs from the one before is the app's
  copy of an earlier report. That save leaves a newer change on the light in
  place, so a press that reaches the agent after the app saved the previous
  one is not written over. The save names the controls the user set without a
  write, as on an offline light (`ChosenLight`): those, and the values the
  save removed, replace the change. The save also names the revision of the
  file it wrote (`ConfigRevision`, a digest of the text). A file the agent
  reads at another revision was edited after the save, so it loads as a plain
  reload. A plain `reload_config`, after a hand edit of `config.toml`, a CLI
  edit or at startup, replaces the change wherever the saved value differs,
  even when it matches an earlier report.
- **OpenLogi's own writes are not changes on the light.** The light echoes
  every setter as a notification. A notification that matches the value the
  agent just wrote is the echo. A brightness the light's effective maximum
  explains is the light's limit, not the user's choice, even when another
  notification is read first: lowered to a limit below the old brightness or
  the old limit, or raised from the limit it sat at once that limit rose. A
  press read together with a limit it stays under is still the user's. Nor is
  a brightness a request was clamped to: the light sends the clamp when a
  request asks for more than the limit, and holds the limit, which nobody
  chose. A clamp the light's button made still follows a press on it. A clamp
  stops explaining the brightness once the button makes a clamp, or once a
  later brightness differs from the clamped one. A notification that arrives
  while the light is read makes the watcher read it again, at most three times
  per notification; a light that keeps notifying is read again on the next
  call without waiting. The published state still follows the write: the
  watcher reports the state a write read back once, whether or not the light
  echoes it, unless a later read replaced it.

Verified against scripted lights only; the repository has no Litra capture yet.

## 2026-10: RGB software control is held per session and handed back on exit

A volatile `0x8071` effect shows only while the host holds software control,
so a successful write keeps it. The agent used to keep it forever, which left
a keyboard frozen on the last colour after OpenLogi quit, with no other host
able to take it until a power cycle.

- **Hand it back on a final exit.** Tray quit, uninstall, and SIGTERM return
  cluster control to the firmware before the HID++ fleet stops, so lighting
  goes back to the onboard effect. G HUB behaves this way, and Solaar's RGB
  control restores firmware control on close to match it. It is also the rule
  this agent already follows for diversion: leave the device in its native
  state. Logi Options+ is no reference here: it builds the `0x8070`/`0x8071`
  wrappers but never drives them, and claims only `0x1300` for the
  precision-mode DPI LED.
- **Not on a handover or replacement.** The successor repaints on arrival, so
  releasing would only flash the onboard effect, and the successor's own
  final exit still hands control back. A disconnect or crash cannot send
  anything; the firmware drops volatile state on power loss.
- **Clear only the bit that was set.** The release clears `ALL_CLUSTERS` and
  keeps the other control and event flags from before the session's first
  claim. Restoring that snapshot verbatim would leave control held forever
  after an agent that never released it, which every earlier build was.
- **The standalone CLI keeps the colour.** Leaving a colour set is the point
  of `openlogi diag lighting`, so only the agent's exit consumes the leases.
- **Lighting off is not a release.** It writes black under the same control;
  releasing there would light the onboard effect instead.
- **The exit waits for writes already running.** It refuses new lighting
  writes first, then gives running ones a bounded wait to record their
  leases, so a first write that claims while the agent quits is still handed
  back. A write still running at the deadline keeps its control, and the
  exit logs it.
- **A lease belongs to one device.** It is bound to the connection the claim
  went through and released only through that connection while inventory
  still publishes it, which covers a replug. A receiver slot keeps the
  receiver's connection when another device is paired into it, so every
  write also records the unit ID the device reports (`0x0003`), including
  one that finds control already held: the replacement may be held by this
  session's earlier claim or by another host. A write with another unit ID
  replaces the lease, and the release re-reads the unit ID and leaves a
  different device alone. Inventory snapshots do not void leases: they trail
  the channel a write goes through, so dropping a route's lease when a
  snapshot shows its device gone could erase a claim already taken on the
  replacement. A device without `0x0003` is identified by having none.
- **`0x8081` per-key frames go under the same claim.** While the firmware
  owns a cluster it draws its own effect over host frames. On a device with an
  `0x8071` engine a per-key write therefore claims (with rollback), sets
  Static as the base on every cluster that offers it, paints, and replays the
  first frame after a fresh claim, as Solaar does. A device with only `0x8081`
  has no engine to take over and is written as before.

Verified against scripted devices only; the repository has no `0x8071` or
`0x8081` capture yet.

## 2026-10: macOS updates go through Sparkle, which never polls

The built-in engine replaces the running bundle in place, so a standard user
whose copy sits in an `/Applications` they cannot write to gets a permission
error instead of an update, and there is no install on quit. Sparkle asks for
administrator rights there and installs on quit. The signed macOS bundle now
embeds Sparkle 2 (through `gpui-updater-sparkle`, loaded at run time) and the
engine stays the fallback everywhere else.

- **The privacy promise outranks Sparkle's defaults.** Updates are opt-in and
  OpenLogi does not poll. The bundle ships `SUEnableAutomaticChecks = NO` and
  the app switches the schedule off should the user defaults turn it on; the
  one launch check is OpenLogi's own, behind **Check for updates**, and opens
  no window. `SUAllowsAutomaticUpdates = YES` is what lets **Automatic updates**
  work without the schedule: Sparkle's settings otherwise tie automatic
  downloads to automatic checks (`SPUUpdaterSettings.m`). The config owns the
  preference and Sparkle's `automaticallyDownloadsUpdates` mirrors it; a change
  made with the checkbox in Sparkle's update window is adopted back.
- **One feed per architecture, not a universal build.** The release ships an
  arm64 and an x86_64 DMG from a build matrix; `SUFeedURL` points each bundle
  at `appcast-<arch>.xml`, which keeps the matrix and the DMGs as they are. A
  universal bundle would allow one feed but doubles every binary in the
  download and changes what the release builds.
- **Signed on Linux, in Rust.** `release appcast` signs in the publish job with
  `ed25519-compact`, checked against signatures Sparkle's own `sign_update`
  made, instead of running Sparkle's macOS-only tools there. It refuses a
  private key that does not match the public key the bundles embed.
- **`CFBundleVersion` is the crate version.** Sparkle compares the appcast's
  `sparkle:version` against it, and cargo-bundle writes the build's UTC
  timestamp there, which would rank above every release. xtask stamps the
  crate version, as it already did for the helpers, and refuses a Sparkle
  bundle without it. Bundles from before Sparkle keep their timestamps, but
  they update through the engine, which compares the version compiled into
  the binary.
- **Kept: `latest.json` and the DMG minisigs.** Bundles from before Sparkle read
  only the manifest, and they reach a Sparkle bundle through it.
- **Rejected: Sparkle's XPC services.** OpenLogi is not sandboxed, so they never
  run; xtask removes them rather than sign and ship them.

## 2026-09: Grant probes run on notification, with a heartbeat backstop

The hook re-checked the Accessibility grant between every 500 ms run-loop
slice — `AXIsProcessTrusted` plus a throwaway `CGEventTapCreate` — and the
agent polled both privacy grants every 1.2 s for the GUI. Each probe is a
WindowServer round trip that takes seconds around a sleep transition, which is
where the probe-budget work (#952, #1282) came from. macOS posts two
undocumented notifications when the list is edited, and the
[`axwatch`](https://crates.io/crates/axwatch) crate observes both. Measured
against tccd's event log on macOS 26: adding a row, switching it off and
switching it on post both; removing the row posts only the Darwin one.

- **Probe on cue.** `grant::ProbeCue` raises a flag from the watch's handler,
  and the tap thread probes only on a raised flag; the two agent-core grant
  watchers read on the same wakes. Detection latency for an announced edit is
  unchanged (one slice); an unannounced one is bounded by the heartbeat.
- **Keep the heartbeat and the probe.** The notifications are undocumented, and
  a missing run loop, a code-signing rule or a future macOS can swallow them;
  the reconciled probe remains the authority, at 5 s instead of 500 ms. A watch
  that cannot start falls back to the old cadence with a warning, never
  silently.
- **Rejected: `AXIsProcessTrusted` alone on wake.** It stays `true` after the
  row is removed — the very edit only the Darwin notification announces — so
  the read cannot replace the probe.
- **Not changed: `TAP_PROBE_BUDGET`.** Rarer probes make a collision with a
  sleep transition rarer, not impossible; shrinking the budget waits for a
  measured revoke→teardown on hardware.

## 2026-09: A crash respawn re-arms by login session, not by exit code

With `launch_at_login` off, the macOS dormancy gate read every launchd start as
a login the user opted out of, and left 60 s later with the `exit(0)` launchd
never respawns. A crash respawn takes the same path — the plist has one trigger
(`SuccessfulExit` implies `RunAtLoad`) — so one hook-watchdog `exit(78)` on a
lid close silently ended remapping until the user opened the GUI (#952).

- **The gate asks the session, not launchd.** Arming records the login session
  (kernel boot session UUID + audit session id) in the runtime dir; every final
  `exit(0)` — tray Quit, uninstall, SIGTERM — erases it; a handover to a
  scheduled successor (binary update, Input Monitoring relaunch) keeps it. A
  start that finds its own session recorded re-arms at once; a login finds a
  stale session and stays dormant. The boot half is required: audit ids repeat
  across boots.
- **Rejected: a second, crash-only plist** (`KeepAlive = {Crashed: true}`, no
  `RunAtLoad`). `Crashed` does not cover `exit(78)` or a panic's `exit(101)`,
  which are exactly the exits that need recovery, and it would reintroduce the
  two-label registration the 2026-08 lifecycle work removed.
- **SIGTERM is final.** launchd's logout and dev tooling mean "stop". The
  stale-agent takeover (`takeover.rs`) sends it too, and there the successor
  would rather find the record; it makes no difference, because the only
  starts that reach the takeover are GUI kickstarts, and that GUI's
  declaration arms the successor anyway.

## 2026-09: The macOS device-I/O gate follows powerd, through an SPI, and fails open

The gate that keeps HID access off while the Mac is dark (#656) was first built
on `NSWorkspace` notifications (#1142): `WillSleep` / `ScreensDidSleep` /
`SessionDidResignActive` closed it, `ScreensDidWake` / `SessionDidBecomeActive`
reopened it. Those are edges, and macOS does not guarantee the wake edge —
Apple DTS (developer forums thread 796109) confirms `ScreensDidWake` is skipped
on some MacBooks for a lid-close/open cycle and that the system may not even
run the app's run loop to deliver it. One dropped edge paused device I/O until
the agent was restarted (#1281). The screen-sleep source was never a
requirement in its own right: it stood in for "not a DarkWake", the state
AppKit cannot name (`NSWorkspaceDidWakeNotification` fires for both).

The gate now reads the thing it is actually about. `activity_macos` subscribes
to powerd through `IOPMConnection`, which reports every Sleep / DarkWake /
FullWake transition as the complete capability set of the new state and reads
the current set on demand, so every input is a level: nothing pairs, a missed
event is corrected by the next, no reconciler timer exists. The console level
(fast user switching) comes from `CGSessionCopyCurrentDictionary`, with the
AppKit session edges forwarded as hints and the level re-read on every power
transition. A launch reads both levels before releasing its hold, so a
watchdog relaunch during a sleep transition no longer guesses "awake" (#952).

Three things decided the API:

- **Only `IOPMConnection` names a DarkWake.** DTS states plainly that the
  public API does not expose DarkWake at all: `IORegisterForSystemPower`
  delivers `kIOMessageSystemHasPoweredOn` for a DarkWake as for a full wake,
  so a gate on it cannot tell the one-second DarkWake blip #1281 was reported
  with from the full wake that followed it. The undocumented `IOPMrootDomain`
  "System Capabilities" registry key carries the same level but no events,
  which is what forced #1283 into polling.
- **It is an SPI, and that is accepted.** The functions are exported by IOKit
  since 10.6 and prototyped only in Apple's open-source `IOKitUser`; the
  shipping `pmset` is built on them. The hand-written `extern` is confined to
  `activity_macos.rs` and inventoried in `.agents/rules/objc-ffi.md`. OpenLogi
  is not App Store distributed.
- **Losing the SPI fails open.** If the subscription cannot be made the gate
  is told "permanent full wake" and follows the console alone, at error level.
  That is pre-0.8.2 behaviour (hardware usable, #656 unprotected); failing
  closed would leave an agent that can never touch hardware, which is the
  outage #1281 describes.

## 2026-08: The agent stays one process; a crossing edge gets a wire, not an event layer

The 2026-06 daemon split (#165) put the resident input machinery in
`openlogi-agent` and left the GUI an on-demand IPC client. Its two
cross-process abstractions have been production infrastructure since:
`openlogi-ipc` is the versioned, append-only wire, and `succession` owns the
one-process-per-role lifetime pattern (the agent negotiates as a peer, the
overlay serves one run as a subordinate, the GUI only reports mismatches).
This entry records the deliberate end of that road: the agent itself does not
split further, and no transport-agnostic event abstraction is built inside it.

Two facts decide it:

- **Every candidate cut crosses the hot path.** The agent's two
  permission-bearing roles — the CGEventTap hook (Accessibility) and HID++
  device I/O (Input Monitoring) — are not adjacent subsystems but two ends of
  one input loop: the hook feeds the action dispatcher, which performs HID++
  writes (DPI cycle) and OS injection, and HID++ gesture capture feeds the
  same dispatcher in the other direction. A hook/device process boundary puts
  IPC latency and a new failure mode inside button-press handling — the path
  the product exists to keep fast. The GUI/agent split worked precisely
  because its crossing edges are cold (config saves, snapshots, long-polls);
  no cold cut of the agent exists.
- **The isolation benefits already arrived by other means.** Crash recovery is
  launchd supervision (`KeepAlive = {SuccessfulExit: false}`); prompt
  containment is the dormancy gate (nothing user-visible before arming);
  orphan lifetime is `succession`. What a split would still add — the event
  tap surviving the seconds of a device-stack respawn — does not pay for a
  second TCC identity: bundle identity is the TCC primary key, so a new
  helper process is a new identity with zero existing grants, a mass
  re-grant event of the 0.6.24 class.

The standing doctrine for any edge that does someday cross a process boundary:
give it the treatment the overlay got — a versioned method on the
`openlogi-ipc` contract (append-only, golden-tested, `PROTOCOL_VERSION`
bumped) plus a `succession` role for the new process's lifetime — and cut
along the named seams (`startup::StateWatchers` is the message list of a
watcher-side wire, `lifecycle::Armed` is the consumer-side state list). What
is deliberately not built is an in-process event bus or transport-agnostic
event-source layer: it would tax every in-process edge with serialization
bounds, fallibility, and reconnection semantics that shared memory does not
have, in preparation for a boundary this analysis says should not exist.

Revisit if a platform forces a privilege split (a packaging world where
uinput access requires a system service), if sandboxing the device stack
becomes a requirement, or if a genuinely cold cut appears. The first move is
still a wire, not an abstraction.

## 2026-08: Device settings are keyed by identity, not by transport

A device's config key was derived from how it was reached, so the same mouse
on a receiver and on a cable became two devices: two entries, two carousel
cards, settings left behind on whichever link set them. The correlating value
was already read on both paths and thrown away — `DeviceStableId::from_parts`
took `serial` and `unit_id` and discarded both for receiver routes.

- Keys are now `unit:<hex>` or `serial:<s>`, with a persisted `links` table
  naming the routes the device has been seen on. That table is also the index
  that identifies a sleeping device when only its route is known, which is why
  it is stored rather than recomputed. It fixes a second bug in passing:
  unpairing mouse A from a slot and pairing B into it no longer hands B all of
  A's bindings, because the key is no longer the slot. Not retroactively,
  though — a pre-upgrade config records nothing about which device wrote a slot
  entry, so the first sighting after the upgrade still folds it into whatever
  now occupies the slot. Only from that point on is the entry keyed by unit and
  the slot empty.
- A device whose unit id reads all-zero keeps its route key and never
  correlates — degradation, not an assumption about hardware we cannot sample.
- Capabilities are measured per link, because they genuinely differ per link:
  a G502 LIGHTSPEED publishes `0x2121 HiResWheel` over its receiver and
  `0x00c2 DfuControlSigned` over USB, same firmware image either way (#660).
  The probe was never wrong; one device simply could not own both readings.
  Each link's capabilities are rewritten from the sighting that reached it, so
  the table describes the hardware rather than whatever a migration happened to
  leave behind. Settings that disagree between links become per-link overrides
  rather than one link overwriting the other.
- Migration is two-phase because a v4 direct key carries the unit id in the key
  string while a receiver key says nothing about the device: schema 4 → 5
  renames direct keys mechanically at load, and receiver entries fold into
  their canonical entry on the next online sighting. Until that sighting the
  legacy entry stays orphaned. Closing that window at load time by matching
  entries on identical `model_ids` was rejected — model ids are model-scoped,
  so two identical mice, one on a receiver and one cabled, would merge into one
  device. That is the property slot-keying protected, and it must not regress.

## 2026-08: Suppressions are `expect`, and tests are exempt by config

A sweep of every lint suppression in the tree (207 attributes, 247 lints) found
two systemic problems, both now fixed.

- **Tests restated the same exemption 78 times.** `unwrap_used`/`expect_used`
  stay at warn so product code has to state its panics, but every test module
  had copied `#[allow(…, reason = "idiomatic in tests")]` to opt out — about
  40% of all suppressions in the workspace, carrying no information. A root
  `clippy.toml` with `allow-unwrap-in-tests` / `allow-expect-in-tests` replaces
  all of them. Clippy's exemption covers `#[cfg(test)]` modules and `#[test]`
  functions; a free helper in a `tests/` integration file is the one shape it
  cannot see, so `openlogi-ipc`'s wire-format test keeps a file-level
  suppression. Build scripts are not tests and keep theirs too.
- **`allow` rots silently.** 20 suppressions no longer suppressed anything,
  including three module-wide `dead_code` blankets in `openlogi-assets` that
  had been inert since those modules became `pub mod` — and would have hidden
  real dead code the moment they went private again. Suppressions are now
  `#[expect]` by default, which fails the build once it stops being needed.
  `allow` survives only where `expect` cannot work: a lint that fires under
  some `cfg` but not others, one whose fulfilment differs between a crate's
  lib and test targets, and one raised inside a macro expansion (rustc does not
  credit an expectation with those, so it both suppresses the warning and
  reports itself unfulfilled). Each such site carries a comment saying which.
- **Both rules are now machine-checked.** `allow_attributes` and
  `allow_attributes_without_reason` join the lint table, costing three
  annotated exceptions and nothing else — the sweep had already left the tree
  compliant. One blind spot to remember: `allow_attributes` only sees outer
  `#[allow]`, so a module-wide `#![allow(…)]` — precisely the shape that rotted
  in `openlogi-assets` — still passes it. Adding them also disproved the
  first-pass rule that a `cfg_attr`-wrapped suppression always needs `allow`:
  two of the four turned out to work fine as `expect`.

Related: the seven file-wide `cast_*` blankets were narrowed to the functions
that need them, and most of their sites turned out not to need a suppression at
all — `cast_signed`/`cast_unsigned`, `&raw const`/`&raw mut`, `to_le_bytes`, or
a shared conversion helper. Linux CI showed `capture_linux`'s file-level
blanket was two-thirds dead (`cast_possible_truncation` /
`cast_possible_wrap`); the remaining `cast_sign_loss` sits on `clamp_u8`.

Supersedes the "`openlogi-hidpp` stays out on purpose (vendored)" note in the
shared-lint-set entry below: the hard-fork ruling retired that, and the crate
inherits `[lints] workspace = true` like every other.

Sweeping for rot is mechanical and worth repeating: rewrite every
non-`cfg_attr` `allow(` to `expect(`, run clippy, and each "lint expectation is
unfulfilled" is a suppression to delete. Run it on all three lanes — native,
`--target x86_64-pc-windows-gnu`, `--target aarch64-unknown-linux-musl` —
because CI has no macOS clippy job and a platform-gated suppression is only
evaluated on its own platform.

## 2026-08: Shared clippy lint set

The workspace adopted the shared ten-lint set (`assertions_on_result_states`,
`cast_possible_truncation`, `cast_possible_wrap`, `cast_sign_loss`,
`error_impl_error`, `exit`, `or_fun_call`, `ptr_as_ptr`,
`tests_outside_test_module`, `undocumented_unsafe_blocks`) on top of the
existing `pedantic` + `unwrap_used`/`expect_used` table.

- One table, inherited everywhere. `openlogi-desktop`, `openlogi-camera` and
  `openlogi-hook` carried hand-copied duplicates of `[workspace.lints]`, so any
  lint added to the workspace would have silently skipped them — three of the
  crates holding most of the FFI. Cargo rejects `[lints] workspace = true`
  alongside local overrides, so `openlogi-hook` moved its `unsafe_code` opt-out
  into its three platform modules. `openlogi-hidpp` stays out on purpose
  (vendored).
- `tests_outside_test_module` only recognises a literal `#[cfg(test)]`. Compound
  gates are written as stacked attributes (`#[cfg(test)]` then `#[cfg(unix)]`);
  an integration test under `tests/` carries a file-level `#![expect(…)]`
  because that file is already a test-only crate. Splitting the attribute also
  wakes `items_after_test_module`, so such a module belongs last in its file.
- `exit` gets a real `ExitCode` wherever the call site can return — `openlogi
  list` hands status 2 back to `main` — and a reasoned `#[expect]` where it
  cannot (the AppKit run loop, the watchdog threads, the update handover).
  Clippy does not look inside `define_class!`, so the menu-bar Quit body moved
  out of the macro rather than escape the lint by accident.
- Not adopted: the policy's `unexpected_cfgs` / `check-cfg = ['cfg(kani)']`
  entry. Nothing here uses Kani, and `unexpected_cfgs` already warns by default,
  so it would be dead configuration.

## 2026-08: Standalone raw-light boundary

Standalone lights such as Litra stay outside the HID++ receiver/paired-device
model and are normalized only at the shared agent and GUI device-record
boundary. This keeps the existing HID++ wire and routing semantics unchanged
while allowing future light drivers to share capability-driven controls.

- Persist brightness as a normalized percentage and temperature as Kelvin;
  native units and report encoding remain driver responsibilities.
- Use device serials for persistent raw-device keys. OS-node identifiers are
  runtime-only hints and must not silently become physical configuration keys.
- Advertise optional light controls through `LightCapabilities`; the GUI gates
  controls from those capabilities rather than from `DeviceKind::Light`.
- Serialize and coalesce per-device light writes in the agent so reconnect,
  camera automation, config reload, and manual commands cannot interleave at
  packet level.

## 2026-07: Infrastructure we keep custom instead of using a crate

A dependency audit replaced most general-purpose infrastructure code with
mature crates (`tempfile`, `which`, `plist`, `walkdir`, `xshell`, `sysinfo`,
`fs-err`, `backon`, `opener`, `etcetera`, and others — see the git history of
`FIXDRY.md` for the full list). The following stayed custom, deliberately:

- `openlogi-core::single_instance`: the `single-instance` crate uses different
  backends (for example abstract Unix sockets on Linux) and does not preserve
  OpenLogi's data-dir lock-file path, per-role names, and error classification
  closely enough to be a safe deletion.
- Agent tray Quit's `openlogi://quit` dispatch keeps
  `std::process::Command::output()` intentionally: it blocks until
  LaunchServices accepts the Apple Event, while generic opener crates only
  guarantee process spawn.
- GUI helper launch keeps `/usr/bin/open -g -n` intentionally: it needs
  LaunchServices-specific flags to start the packaged agent under its own TCC
  identity, which generic opener crates do not expose. (Since 2026-08 it is
  the *fallback*: a registered login item is kickstarted through `launchctl`
  instead, which supervises the agent as well — `open` remains for
  unregistered installs and stays custom for the same reason.)
- Agent autostart install keeps direct `systemctl` calls because it is managing
  systemd user units, not merely opening or spawning an arbitrary program.
- Self-restart and `disclaim` launches stay custom because they are process
  identity / update lifecycle boundaries, not generic command orchestration.
- `openlogi-hook`: event suppression/rewriting and foreground-app lookup are
  OpenLogi-specific and not covered cleanly by generic input crates.
- `openlogi-inject`: platform-specific action synthesis may overlap with
  `enigo`, but current semantics are narrower and more controlled.
- `openlogi-hid` / vendored `openlogi-hidpp`: the right path is upstreaming
  OpenLogi-specific fixes, not replacing the fork blindly.

## 2026-06: Fn is not a capturable trigger (macOS)

An instrumented `CGEventTap` probe during the function-key remapper design
(the spec lived in `docs/superpowers/` until 2026-08; full text at
[91fe5d80](https://github.com/AprilNEA/OpenLogi/blob/91fe5d80f3a2c16cf16061b3abb5a01d47fc8637/docs/superpowers/specs/2026-06-30-function-key-remapper-design.md))
settled why the Fn modifier cannot join the keyboard-remap trigger vocabulary:

- F1 arrives as keycode 122 **with** the `SecondaryFn` flag, but plain `Q` and
  `Fn+Q` are byte-for-byte identical, as are plain Shift and `Fn+Shift`;
  pressing Fn alone produces no event at all (not even `FlagsChanged`).
- The keyboard firmware holds Fn internal unless the key has a dual
  function-row meaning, so the flag attaches only to F1–F12.
  `Fn+<anything else>` is indistinguishable at the tap — firmware behavior,
  not something OpenLogi can code around at this layer.
- The only theoretical path is raw-HID reading below the OS event system
  (Karabiner/DriverKit territory) — a large subsystem with no guarantee a
  given keyboard exposes Fn there. Not pursued.
