---
paths:
  - "crates/openlogi-hook/**"
---

# Input hook (CGEventTap / evdev / WH_MOUSE_LL)

- macOS: the CGEventTap freeze-hazard state machine is load-bearing. The tap must
  self-disable when Accessibility is revoked, on its own thread, with the bounded
  run-loop slice — a stopped watcher after grant once froze all input on the machine.
  Don't restructure it casually, and don't migrate the tap to `objc2-core-graphics`
  (the `NSWorkspace` read is the only part that moved to `objc2`).
- The off-main `frontmost_bundle_id` read keeps its explicit `autoreleasepool` — the
  watcher thread has no run loop; that is the only place in this crate a pool belongs.
- This crate ships non-macOS implementations (evdev/uinput, WH_MOUSE_LL) that a
  macOS-green build never compiles. CI lints them; treat the linux/windows CI jobs as
  the check, not local builds.
