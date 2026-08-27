# openlogi-hook — OS input capture

The event-tap crate: CGEventTap on macOS, evdev+uinput on Linux, WH_MOUSE_LL
on Windows. Cross-platform cfg discipline is
[`.claude/rules/cross-platform.md`](../../.claude/rules/cross-platform.md), and
the macOS FFI contract is
[`.claude/rules/objc-ffi.md`](../../.claude/rules/objc-ffi.md); this file is the
crate's own load-bearing behavior.

- macOS: the CGEventTap freeze-hazard state machine is load-bearing. The tap must
  self-disable when Accessibility is revoked, on its own thread, with the bounded
  run-loop slice — a stopped watcher after grant once froze all input on the machine.
  Don't restructure it casually, and don't migrate the tap to `objc2-core-graphics`.
  The `NSWorkspace` read and the Accessibility-trust check/prompt are the parts that
  did move to the objc2 framework crates — see `.claude/rules/objc-ffi.md` for the rule that
  every TCC call uses a typed binding rather than a hand-written `extern` block.
- `AXIsProcessTrusted()` is **not** a revocation signal: it keeps returning `true`
  after the user deletes the app's row from System Settings, which is how #674 froze
  clicks machine-wide. `has_accessibility` pairs it with a throwaway filtering tap
  (`CGEventTapCreate` → NULL when the grant is gone); keep both, in that order — the
  trust read is the cheap short-circuit, the probe is the truth. Never probe with
  `ListenOnly`: that asks about Input Monitoring, a different grant.
- Re-enabling the tap each slice is idempotent and recovers a disable the OS never
  reported. Charge `RearmBudget` from both `TapDisabledBy*` and
  `CGEventTapIsEnabled`: a tap the system keeps disabling must be let go instead of
  fought over, even when CoreGraphics omits the callback.
- Keep the `CGEventTap` owned by its run-loop thread. Normal teardown disables it
  synchronously there and `Drop` invalidates its Mach port; a watchdog whose tap
  thread is wedged force-exits so the OS destroys that process-owned port. Do not
  add cross-thread tap ownership solely to pre-invalidate it during process exit —
  Core Graphics does not document that operation as thread-safe.
- The tap callback must never block and never panic: use `try_read`/`try_lock` only,
  queue bound actions off-thread, wrap the user callback in `catch_unwind`, and keep
  the stuck-callback watchdog that force-exits the agent if the budget is exceeded.
  An active HID-level tap serialises every pointer event; a hang freezes clicks
  machine-wide. Only suppress events from remappable Logitech sources
  (`source_is_remappable`) — never the built-in trackpad.
- A macOS tap stop request is not proof of teardown. Keep the independent lifecycle
  watchdog armed until the tap thread reports the tap destroyed (and, for an explicit
  stop, the thread exited). The watchdog must not call the Accessibility trust API —
  that query can stall during TCC revocation; monitor tap-thread progress instead, and
  force-exit the agent if revocation or shutdown stalls so macOS releases the HID tap.
- The off-main `frontmost_application` read keeps its explicit `autoreleasepool` — the
  watcher thread has no run loop; that is the only place in this crate a pool belongs.
  Every string it copies out (bundle id *and* localized name) must be owned before the
  pool drops.
- This crate ships non-macOS implementations (evdev/uinput, WH_MOUSE_LL) that a
  macOS-green build never compiles. CI lints them; treat the linux/windows CI jobs as
  the check, not local builds.
