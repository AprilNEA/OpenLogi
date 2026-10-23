# Disable Keys recovery review

Approved by the user on October 23, 2026. Addresses review 5626187878 on PR #993.

## Invariants and owners

The desktop is an IPC client. Requested masks, hardware readings and confirmed
policy are distinct. A write error does not establish unchanged hardware.

1. Desktop `DisableKeysReads` owns reading validity. After an uncertain write,
   retire stale reads and cached confirmed snapshots, request a fresh read, and
   keep controls unavailable until it succeeds. Exhaustion exposes Retry read.
   A reconciliation read never persists the unconfirmed request. Route generations
   fence late responses. A command rejected before dispatch need not invalidate.
2. Agent write-order ownership and the inventory confirmation scheduler own
   restoration. When an unsuccessful or cancelled manual request releases its
   pending intent, schedule reconciliation of the current confirmed policy.
   Cancellation cleanup signals work but does not perform HID I/O. Newer manual
   requests retain priority; a confirmed newer choice survives until config catches
   up. Policy removal, opt-out and route/physical replacement retire old work.
   Do not add a competing retry loop or capture an obsolete desired mask.
   Arm route-scoped recovery even after initial reapply budgets drain. A confirmation
   while a newer manual intent owns priority must not consume recovery permanently.
   Recover B in saved A -> confirmed unsaved B -> unconfirmed C, not saved A.
3. Inventory scheduling must distinguish a delivered confirmation snapshot from
   failed enumeration. A failed confirmation retains delayed, bounded work; never
   leave a past-due deadline causing a busy loop, or restart a budget on each failed
   repair scan. Ordinary HID/hotplug snapshots do not consume confirmation attempts.
   On exhaustion, ordinary low-frequency recovery remains available.
   Keep logical pending confirmation after fast exhaustion; a later successful
   recovery scan delivers it or rearms a delayed confirmation. Repair/confirmation
   deadline ties cannot reset the budgets indefinitely.

## Scope and integration

Keep current master mock-console commands and existing Disable Keys mock RPCs.
No new dependency, wire method, firmware behavior, permission or production-device
change is intended. Document the capability-gated card, firmware-supported keys,
confirmed-state persistence, uncertain writes, save/reload recovery, reconnect/wake
reapplication and bounded retry limitations in existing user/HID++ documentation.

## Proof

- Desktop: write takes effect but confirmation fails; no config save, no stale
  toggle; reread success enables a toggle based on fresh state; reread failure
  remains blocked and retryable; old-route responses remain fenced.
- Agent: older write reaches hardware, successor fails or is cancelled; production
  scheduling restores policy without test-invoked reapply. A successful successor
  wins, and removed/replaced policies cannot be restored by stale work.
- Watcher: initial success, failed confirmation, failed repairs; deadline remains
  delayed, budgets bounded, eventual successful confirmation delivered. Frequent
  lifecycle events do not consume or postpone the settings deadline.
- Observe focused failures before implementation, then pass the same tests and
  neighboring cases. After conflict resolution, run full workspace fmt, Clippy,
  tests and non-GUI rustdoc plus affected CI jobs before push. Separate local,
  mocked, remote CI and physical-hardware evidence.
