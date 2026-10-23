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

## Verification evidence

The rendered-style reproducer failed before the fix: the disabled retry inherited
an opaque foreground, while the stock disabled Button used a half-opacity muted
foreground. The same test passes after the shared override is removed. The
comparison keeps the pointer outside the controls to avoid comparing a hovered
button with a normal one; it covers enabled and disabled colors in both themes.

Focused checks passed: two shared status tests, 31 Disable Keys tests (including
pointer, Enter and Space Save Retry scenarios), and the gallery smoke test.
Independent source review found no actionable issues.

The final macOS arm64 gate passed with Rust 1.99.1 and warnings denied:

- workspace formatting and all-targets Clippy;
- workspace tests: 3403 passed, four ignored;
- all-targets workspace tests: 3402 passed, two ignored;
- non-GUI rustdoc, typos and both ast-grep scans.

GUI commands used `--features gpui_platform/runtime_shaders`. The ignored tests
are the existing interactive foreground-app/Space checks and, in the workspace
run, two ignored documentation examples. They were not counted as passes.

The isolated dev gallery was launched without agent/config/hardware startup.
The user's light-theme, 100% screenshot confirms the visible enabled/disabled
distinction. Automated native screenshot inspection was unavailable (app-control
timeout); dark-theme paint is covered in-process, not yet by an inspected native
screenshot. No new physical-keyboard or Windows/Linux runtime validation is
claimed for this UI-only correction. Schema, IPC and dependencies are unchanged.
