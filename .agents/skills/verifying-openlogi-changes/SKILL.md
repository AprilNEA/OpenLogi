---
name: verifying-openlogi-changes
description: "Plans regression proofs and runs OpenLogi verification based on the diff, affected packages, host OS, and work stage. Use when planning a bug fix, after repository changes, when reviewing test coverage, or before an authorized commit or push."
---

# Verify OpenLogi Changes

Use the smallest check that can disprove the change, then apply the required final gate.

## Classify before running checks

1. Inspect `git status --short`, staged and unstaged diffs, and new files.
   For branch work, include all changes against the intended base, not only the last
   commit. Preserve unrelated work. Check whether Rust-bearing rebases or conflict
   resolution occurred since the last full gate.
2. Read the root [iteration policy](../../../AGENTS.md#verification-while-iterating-fast-path)
   and each changed area's scoped rules. For a push, also read the
   [local gate](../../../.agents/rules/ci.md#local-gate-hard-stop-before-push--scale-it-to-the-affected-graph).
   This skill applies those policies; it does not make the pre-push gate mandatory
   for every local edit.
3. Classify the diff by what it controls, not only by extension:
   - Prose, skill text, or images alone are non-Rust; do not compile Rust for them.
   - Rust source **or build/validation inputs** are Rust-bearing. A `Cargo.toml`,
     lockfile, toolchain, or CI change is not a docs-only change.
4. State the proof before editing: affected behavior, likely wrong implementation,
   and an input or interaction where the expected result differs from that mistake.

## Reproduce regressions before fixing them

- For a bug fix, run the smallest reproducer before changing production code.
  Drive normal actions or the existing injected backend, not corrupt private state.
  Confirm failure on the reported path, not a compile error or unrelated setup failure.
- Use the test layer's existing barriers, events, or logical time. Do not add sleeps
  or retries to hide an ordering bug.
- Record the violated invariant, its owner, reproduction command, and observed failure.
  If reproduction is unavailable, state the missing evidence and what a substitute
  test proves. Do not claim the original bug is verified fixed.
- After the fix, run the same reproducer and a nearby non-failing case that would
  catch an overbroad change. Report both before and after results.

## Iterate, then stabilize

- While code moves, run one focused test or package check. For Rust, use
  `cargo test -p <package> <test-filter>` or `cargo check -p <package>`.
  Inspect the test count; a filter that matches zero tests is not evidence.
- Once stable, run formatting, relevant tests, and Clippy for each changed Rust
  package as specified in the root policy. Check affected consumers for shared API
  changes. Do not run full-workspace checks after every edit.
- For non-Rust changes, check the actual files: spelling, whitespace, links,
  manifests, or executable behavior. Select shell/Nix/packaging checks from the
  [CI map](../../../.agents/rules/ci.md#if-you-changed-x-run-y), not Rust by habit.

## Select the pre-push gate only when pushing

1. Use the local gate's non-Rust checks or Rust-bearing tier selection.
2. For Rust-bearing work, apply the local gate's full-tier triggers first.
   Do not select the affected-package tier merely because few files changed.
3. Otherwise derive the affected set from the final dependency graph:

   ```sh
   cargo tree --workspace --target all --invert <changed-package>
   ```

   Repeat for every changed package. Take the union of workspace packages,
   including transitive consumers. Run the local gate's affected-package tier
   for that entire set, not just the edited crate directories.
4. Use the exact commands and compiler flags in the local gate. Keep Git hooks
   enabled. After a failure, fix the cause with a focused check, then rerun the
   applicable tier on the final tree. Do not push a known-red tree.

## Add checks for the affected boundary

Consult the [CI job map](../../../.agents/rules/ci.md) for exact commands:
`cargo xtask ci --list` lists jobs; `cargo xtask ci --dry-run` prints planned
commands but does not verify them. Run required named jobs rather than assuming
the host gate reproduces all CI.

| Changed boundary | Additional evidence |
| --- | --- |
| IPC or serialized wire types | [IPC rules](../../../crates/openlogi-ipc/AGENTS.md), version discipline, fixed-byte `wire_format` tests; roundtrips alone are insufficient |
| Platform `cfg` code | [Cross-platform rules](../../../.agents/rules/cross-platform.md), target checks or the required manual audit; host-green is not cross-platform-green |
| UI or localization | [UI workflow](../testing-openlogi-ui/SKILL.md), [i18n rules](../../../.agents/rules/i18n.md), rendered/interaction evidence and catalog checks |
| Fixtures | [Fixture workflow](../contributing-device-fixtures/SKILL.md), strict offline verification and independent semantic review |
| Dependencies, portable crates, MSRV, docs, packaging | The corresponding extra jobs in the CI map, including wasm/rustdoc when applicable |

Report each command's decisive result, the tested host, and the scope it proves.
List failed, skipped, and unavailable checks separately. State real-hardware
verification explicitly. Do not count a skip, dry run, or manual audit as an
executed test. Do not commit, push, create a PR, rerun remote workflows, or release
merely because this skill was loaded; follow the task's authorization.
