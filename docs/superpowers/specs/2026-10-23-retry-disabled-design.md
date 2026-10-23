# Shared retry disabled appearance

Approved scope: fix P2 in the existing retry owner, with disabled-style,
Enter/Space, and Save Retry interaction coverage. Configuration schema, IPC,
dependencies, retry policy, and hardware behavior remain unchanged.

Remove the unconditional foreground override from `ui::status::retry_line`.
The pinned Button owns enabled/disabled/hover colors and keyboard activation.
Remove the now-unused palette parameter and update existing callers. Do not
duplicate disabled colors in this helper or introduce a panel-specific renderer.

Prove the regression before the fix by observing inherited text style inside
the rendered button in light/dark themes, comparing it with the stock text
Button and its enabled counterpart. Exercise Enter and Space on the same
shared component and verify that disabling it suppresses callbacks.

Drive Save Retry through the production panel after a real temporary-file
revision conflict: confirm a write, preserve an external binding edit, activate
retry by pointer/keyboard, then inspect persisted keys, copied bindings and
the sole reload command. No second hardware write is allowed.

Add enabled/disabled retry examples to the component gallery. Run focused
tests, independent review, and the full host pre-push gate using the existing
temporary target. Report pixel/hardware limitations honestly.
