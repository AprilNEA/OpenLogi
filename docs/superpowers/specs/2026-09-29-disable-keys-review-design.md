# Disable Keys review fixes

## Approved scope

Repair PR #993's lost configuration during identity adoption and stale reconnect
writes. The user approved reusing the project's migration and ordering owners,
regression tests first, followed by a pre-push review. No UI or protocol redesign.

## Configuration

`Config::adopt_route` remains the owner of route-to-physical-device migration.
Include `disabled_keys` in its existing optional-field fold: unset canonical
values inherit the legacy value, including an explicit empty set; an explicit
canonical value wins disagreements. Repeat adoption is idempotent. Serialization
and agent-side policy lookup must retain the resulting three states.
On reload, resolve policy keys against the new configuration through
`Config::resolve_device_key`; the inventory's cached key may still precede
route adoption. A migrated change or opt-out must take effect without waiting
for another inventory pass or reconnect.

## Writes

Generalize the existing Fn-lock ordering implementation into a shared write-order
owner, with independent instances for Fn-lock and Disable Keys and independent
queues per keyboard route. Allocate intent tickets before dispatch/lease waits;
serialize actual writes and reject obsolete queued work before it reaches HID.
Retries reuse the configured policy's ticket: dispatch is not a new intent.
An interactive request temporarily supersedes that policy; confirmed success
retains its priority until config acknowledges it. Failure/cancellation removes
only that request, falling back to the last committed intent, not another dead
request. Late cleanup or completion cannot replace a newer committed intent.
Unchanged config acknowledges a matching confirmed value without making an old,
different policy newer. Synchronize policy for offline routes too; invalidate
departed routes before forgetting them. A superseded RPC returns an explicit
append-only wire error rather than success that the GUI might persist.
Foreground, reconnect, and changed-config paths must use the same setting owner.
Unmanaged/disabled policy changes must invalidate queued reapplication without
inventing a new hardware value. Preserve existing Fn-lock behavior.
Route retirement is unconditional, including when an unmanaged route has a
pending manual write. A different physical identity at the same route retires
old requests too. Resolve channels after receiver access and write-turn waits;
retain the turn through readback. Already-issued HID writes are not preempted.

Do not copy the atomic/queue protocol into a second module. Add an architectural
guard preventing agent callers from bypassing the ordered setting writers.

## Proofs and limits

Reproduce migration loss on the public adoption path. Reproduce a reconnect write
queued behind device access and superseded by a newer interactive request through
a scripted HID channel. Use barriers/notifications and worker completion, not
sleeps. Test ongoing writes, separate keyboards/settings, config change/removal,
and unchanged config. Existing stale-GUI-result and retry-cadence tests remain.
Include retry after manual success but before reload, overlapping failed/cancelled
RPCs, and offline opt-out while queued behind receiver access.

Run the full host gate, wasm, architectural guards, and relevant wire tests.
An independent review checks lifecycle coverage before push. Scripted evidence is
not physical MX Keys validation. Public replies remain drafts until authorized.
