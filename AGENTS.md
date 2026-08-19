# OpenLogi — Agent Guide

OpenLogi is a native, local-first alternative to Logitech Options+ written in Rust:
button remapping, DPI, SmartShift, and per-app profiles for Logitech HID++ devices
(Bolt/Unifying receiver, Bluetooth-direct, wired) — no account, no telemetry, plain-TOML
config. macOS and Linux are first-class; Windows is a young but shipping port.
Dual-licensed MIT/Apache-2.0; the `design/` brand assets are proprietary.

The developer handbook (toolchain, packaging, release pipeline) is
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). This file is the agent-facing contract;
subsystem deep-rules are indexed at the bottom.

## Architecture

Three tiers ship in one install: the **GUI** is a pure IPC client, the **agent** is a
background server owning the input hook and ALL device I/O, and shared orchestration
sits beneath both.

| Crate | Role |
|---|---|
| `crates/openlogi` | The CLI binary — thin wrapper over `openlogi-cli` |
| `crates/openlogi-core` | Pure types: TOML config, device model, action catalog. No I/O, no async |
| `crates/openlogi-hidpp` | Vendored fork of the `hidpp` protocol crate (**lib name `hidpp`**, 0BSD) |
| `crates/openlogi-hid` | Device discovery + HID++ writes over `async-hid` |
| `crates/openlogi-assets` | Device-render registry + cached fetch from OpenLogi asset mirrors |
| `crates/openlogi-cli` | `clap` command tree: `list`, `assets`, `diag` |
| `crates/openlogi-hook` | OS input capture: CGEventTap / evdev+uinput / WH_MOUSE_LL |
| `crates/openlogi-inject` | OS input synthesis: CGEvent / uinput+MPRIS / SendInput |
| `crates/openlogi-agent-core` | Shared agent orchestration: hook runtime, HID++ writes, DPI cycle, Actions Ring session state |
| `crates/openlogi-ipc` | The tarpc IPC contract (`src/ipc.rs`) + its local-socket transport, shared by agent and GUI |
| `crates/openlogi-agent` | The `openlogi-agent` binary — hook + device I/O server |
| `crates/openlogi-ui` | Presentation shared by the two GPUI processes: ring geometry/icons, the GPUI asset source, locale negotiation. Depends on `gpui` but **not** `gpui-component` |
| `crates/openlogi-desktop` | GPUI + gpui-component desktop app — polls the agent, no device I/O |
| `crates/openlogi-overlay` | The `openlogi-overlay` binary — cursor-centred Actions Ring, a pure IPC client |
| `xtask` | `cargo xtask` maintenance: bundling, packaging, release manifest |

- GUI ↔ agent speak tarpc/bincode over an `interprocess` local socket. The wire format
  is versioned and **append-only** — read `.claude/rules/ipc-protocol.md` before touching it.
- Three processes ship in the bundle — GUI, agent, overlay — and the overlay is a
  *sibling* of the GUI, not a part of it: it links `openlogi-ui`, never `openlogi-desktop`.
  Anything both need goes in `openlogi-ui`; adding a dependency there puts it in the
  overlay too, so keep the widget kit (`gpui-component`) on the app's side.
- Platform code is cfg-gated per crate (`[target.'cfg(target_os = …)'.dependencies]`).
  `crates/openlogi-desktop/src/platform/AGENTS.md` is the contract for the workspace's ObjC
  FFI and indexes every file that carries any — read it before editing one, including
  `crates/openlogi-overlay/src/platform.rs`, which lives outside that directory.

## Build, run, verify

Nix/devenv is optional — rustup + `rust-toolchain.toml` is enough. If devenv is
installed, direnv loads it; otherwise `.envrc` prints a notice and leaves PATH
alone so system `cargo` works. With devenv active, cargo may only be on PATH
inside the shell — run from the repo root (or `direnv exec . …`), including
git (the hooks need cargo):

```sh
cargo clippy --workspace --all-targets -- -D warnings
# when cargo is only inside devenv:
direnv exec . cargo clippy --workspace --all-targets -- -D warnings
direnv exec . git commit …
```

### Local gate (hard stop — do this before every push)

**Never `git push` until the final tree has passed the full local gate.**
`cargo check` alone is not enough. Conflict resolution + "it compiles on my
Mac" is not enough. Run **all four** on the commit you are about to push:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc -p openlogi-hid -p openlogi-hidpp \
  -p openlogi-hidpp-derive --no-deps --document-private-items
# or: devenv tasks run openlogi:check
```

Exit non-zero on any of those → fix, re-run the **whole** set, then push.
Do not push "to see if CI likes it." CI is confirmation, not the first compile.

The rustdoc step mirrors CI's `rustdoc (hid crates)` job and catches what the
other three cannot: a broken intra-doc link is neither a compile error nor a
clippy lint. How it bites is non-obvious — rustdoc resolves a
`Type::trait_method` link only while that trait is **in scope**, so handing a
hand-written trait impl over to a derive macro deletes the now-unused `use` and
silently breaks every such link. Re-adding the import does not fix it either: a
doc link does not count as a use, so that just trades the broken link for an
`unused_imports` failure — write the trait method's full path instead. After any
refactor that moves impls between hand-written and generated, grep for doc links
naming that trait's methods.

prek hooks (`prek.toml`): `cargo fmt` at commit; full-workspace clippy at push
(rust-scoped, so non-Rust pushes skip it). Hooks are a backstop, not a substitute
for running the gate yourself after a rebase.

### Platform / cfg-gated code (macOS-green is a trap)

macOS-green proves **nothing** about `#[cfg(target_os = "linux")]` /
`windows` code. Recent agent failures that only showed up on CI Linux:

- Shadowing a crate-level constant with a local `const` of a different type
  (e.g. `LOGITECH_VENDOR_ID: u16` next to `use crate::LOGITECH_VENDOR_ID`
  which is `u32`) — E0255 / E0308, **only compiles on Linux**.
- Importing a name that only exists on another OS, or redefining one that
  master already exports from `lib.rs`.

When the diff touches any of:

- `crates/openlogi-hook/src/linux.rs` / `windows.rs`
- `crates/openlogi-inject/src/inject/linux.rs` / `windows.rs`
- `crates/openlogi-hid/src/transport.rs` (has `#[cfg]` branches)
- any `#[cfg(target_os = …)]` block

you MUST either:

1. Cross-check with devenv when available:
   `devenv tasks run openlogi:check-windows` (and any linux check the repo has), or
2. Manually re-read every changed cfg-gated file against **current master** for:
   - name collisions with existing `pub use` / `pub const` items
   - type mismatches (`u16` vs `u32`, `Option` arity, new enum fields)
   - call sites that gained args on master (e.g. `with_runtime`, `build_device_list`,
     `dispatch_action`) but the PR still uses the old signature

Do not claim "cross-platform green" without CI (or a local cross-lint) having
actually run those targets. `RUSTFLAGS=-D warnings` is global in CI — plain
warnings fail there too.

### Wire format / IPC (another silent CI red)

If the change touches anything that crosses the agent↔GUI boundary
(`crates/openlogi-ipc/src/ipc.rs`, serde enums in hid write errors, `DeviceKind`, …):

- Enums are **append-only** (serde index = wire). New variants go at the end.
- Bump `PROTOCOL_VERSION` and regenerate
  `crates/openlogi-ipc/tests/wire_format.rs` goldens from the failure
  message (`left` is the new encoding).
- Run `cargo test -p openlogi-ipc --test wire_format` before push.

### i18n

New GUI strings: insert the same key in the **same position** in every
`crates/openlogi-ui/locales/*.yml` (parity is required). Run
`cargo test -p openlogi-desktop i18n`.

### App / agent runtime notes

- The macOS GUI build needs full Xcode for GPUI's Metal shaders. devenv sets
  `DEVELOPER_DIR`/`SDKROOT` when present; without it, use system Xcode. If the
  shader compile fails under devenv, `direnv reload` first.
- Dev-run the app with `cargo run -p openlogi-desktop` — a cargo runner wraps it
  into `target/dev/OpenLogi.app`. `cargo build` does NOT refresh that bundle,
  and a second instance exits on the singleton lock: quit the old instance and
  re-`run` before judging a UI change "not applied".
- No hardware attached? `cargo run -p openlogi-agent --bin openlogi-agent-mock`
  serves a scripted inventory (both route kinds, every capability-gated panel, a
  pairing flow) over the IPC socket, so the GUI runs unmodified. It defaults to
  the `openlogi-dev` profile — same socket the dev bundle uses, production app
  untouched. Details in `docs/DEVELOPMENT.md`.

## Rust standards

Edition 2024, MSRV 1.96. There is exactly **one** lint table, in the root `Cargo.toml`,
and every crate inherits it with `[lints] workspace = true` — never a private copy, or
the next lint added to the workspace silently skips that crate. A crate needing a
different level opts out **in source** (the `openlogi-hook` platform modules carry
`#![allow(unsafe_code, reason = "…")]`), because Cargo rejects mixing `workspace = true`
with local overrides. `openlogi-hidpp` currently stays out of the table — it is a **hard
fork**, so the "third-party code" rationale for that opt-out no longer holds; whether it
should now inherit is an open question, costed in `crates/openlogi-hidpp/AGENTS.md`.

The table: `unsafe_code = "deny"` (opt out per item with `#[expect(unsafe_code,
reason = "…")]` plus a `// SAFETY:` comment), `clippy::pedantic` at warn,
`unwrap_used`/`expect_used` at warn, plus the shared lint set —
`assertions_on_result_states`, `cast_possible_truncation`, `cast_possible_wrap`,
`cast_sign_loss`, `error_impl_error`, `exit`, `or_fun_call`, `ptr_as_ptr`,
`tests_outside_test_module`, `undocumented_unsafe_blocks`. Any lint suppression carries
a `reason`. What that changes day to day:

- Every `unsafe` block needs a `// SAFETY:` comment saying why it is sound.
- `assert!(r.is_ok())` / `assert!(r.is_err())` are rejected — unwrap the `Result` (in a
  test module that already allows it) or give the assertion a message.
- A test module that wants `expect`/`unwrap` says so:
  `#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]` on the
  module (or on its `mod tests;` declaration). Never route around the lint with
  `unwrap_or_else(|e| panic!("…: {e}"))` — that is the same panic with the check switched
  off. The one honest use of that form is a *dynamic* panic message, where `expect` would
  need a `format!` that allocates on the happy path (`expect_fun_call`).
- A test module gated on more than `test` needs stacked attributes (`#[cfg(test)]` then
  `#[cfg(unix)]`), not `#[cfg(all(test, unix))]`, which clippy reads as a test outside a
  test module. Integration tests under `tests/` carry a file-level
  `#![expect(clippy::tests_outside_test_module, reason = "…")]`.
- `std::process::exit` needs `#[expect(clippy::exit, reason = "…")]` naming why that call
  site cannot hand an `ExitCode` back to `main` instead.

Encode invariants in the type system instead of checking them at runtime:

- Wire/firmware values get typed wrappers: `num_enum` for discriminants, `bitflags`
  (`from_bits_retain` when unknown bits are legal) for flag sets. Unknown wire values
  surface as **errors** (`UnsupportedResponse`-style), never as silent fallbacks.
- Replace long parameter lists with Change/Params structs; make illegal combinations
  unrepresentable rather than validated.
- Ownership models resources (`Retained<T>` in the ObjC FFI) and thread affinity is
  proven by types (`MainThreadMarker`, `!Send` handles), not by runtime checks.
- Libraries return `thiserror` types; binaries may use `anyhow`.

House style:

- **Root-cause fixes only.** Never layer compatibility shims over a broken abstraction —
  refactor it. Never change product code to work around a dev-environment quirk; debug
  the environment (or a release build) instead.
- **Prefer mature crates over hand-rolled logic** (retry/backoff, hashing, paths, …).
  Check `cargo tree | grep <candidate>` before adding a dependency and use `cargo add`
  so versions come from the registry. After ANY dependency change, verify the
  `gpui`/`gpui-component` git pins in `Cargo.lock` didn't move (they are held only by
  the lock; restore with `cargo update -p gpui --precise <rev>`).
- Module layout: a module with its own semantics is `foo.rs` (children in a sibling
  `foo/`); `foo/mod.rs` is only for pure namespace shells. Never both for one module.
- Keep files reasonably sized (split around ~500 lines) into real modules — never
  simulate structure with `// ---- section ----` banner comments. But don't
  over-extract either: inline single-use helpers.
- rustdoc every public item. Comments state non-obvious constraints only.
- Tests cover failure and edge paths, not just the happy path (state machines
  especially). No tautological tests that mirror the implementation; never weaken an
  assertion or special-case an input to make a test pass.

## Git & GitHub

- Conventional commits: `type(scope): imperative lowercase description`. Types in use:
  `feat fix refactor chore docs ci perf style build test`. Scopes are crate short names
  (`gui agent hidpp hid core hook ipc cli assets xtask`) or cross-cutting concerns
  (`release ci i18n windows linux macos tray infra`). `i18n` is a scope, not a type.
- Branches: `type/kebab-description` off `master`. Substantial or risky work goes in a
  worktree so parallel work doesn't collide; trivial fixes may go straight to master.
- Commits are small and focused — split unrelated concerns into separate commits; never
  one giant unreviewable diff.
- **Always `git fetch upstream master` (or origin) immediately before a rebase.** Rebase
  onto the refreshed tip, not a stale local `master`.
- Merging PRs: **squash by default** with a hand-written subject
  `type(scope): description (#N)` (release-plz parses it; merge commits are disabled).
  Rebase-merge only when every commit on the branch is already release-quality
  conventional. Wait for the Greptile review check and CI before merging — findings get
  fixed, replied to, and resolved, not ignored.
- PR bodies: `## Summary`, `## Changes` (per-crate bullets), `## Testing` listing the
  exact commands run plus hardware-verification status (say "not runtime-tested on
  hardware" when true), and a closing `Fixes #N` line. Screenshots for UI changes.
- **All GitHub artifacts — PR titles/bodies, commits, issues, reviews, comments — are
  written in English.**
- **Never add AI attribution** ("Generated with …", AI co-author trailers) to commits,
  PRs, or issues — including when adopting contributors' work.
- Never post to external repos or reply publicly on the maintainer's behalf — draft the
  text for approval. Keep public drafts short, casual, and problem-focused.
- Contributor PRs are adopted, not rejected: check `maintainerCanModify`, rebase onto
  **fresh** master in a worktree, fix review findings, run the **full local gate** on
  the rebased tip, **then** push to the fork branch; preserve authorship
  (`Co-authored-by` when re-homing work). Squash-then-rebase is fine when the PR is
  far behind and commit-by-commit conflicts thrash.
- Issues use the bug/feature/device forms and the `type:`/`area:`/`platform:`/`needs:`/
  `status:` label families. Deferred or out-of-scope work becomes a linked issue, not a
  TODO comment.

### CI / Actions when adopting PRs

- CI concurrency is **per branch** (`ci-${{ workflow }}-${{ ref }}` with
  `cancel-in-progress: true`). Approving or re-running an **old SHA** on the same
  branch cancels the current-head run. Only approve / re-run workflows whose
  `head_sha` equals the PR's current head.
- After a force-push, wait for the new runs; do not re-approve stale
  `action_required` jobs from earlier commits on that branch.
- First-time-fork PRs may sit in `action_required` until a maintainer approves the
  workflow run — that is fine; still do not push until the local gate is green.

## Releases

release-plz drives releases: one unified workspace version, ONE root `CHANGELOG.md`
(never per-crate changelogs), and a single `v{version}` tag that only release-plz
creates — **never hand-create the tag**. Published GitHub releases are immutable:
never re-run a failed release job or re-dispatch on an existing tag.
`release-plz.toml` is the versioning contract — don't trim it.

## Verification

Define the concrete check that proves a change works before writing it — a failing test
that should pass, a command whose output should change, a behavior in the running app —
and loop on that check. Real-hardware verification (physical mice, receivers) is the
maintainer's job: every fix PR states how to test it. Report outcomes honestly,
including what was NOT verified.

**Push checklist (agents):**

1. Rebase/merge conflicts fully resolved — no `<<<<<<<` left, no half-ported APIs.
2. Full local gate green on the **final** tree (fmt + clippy `-D warnings` + test).
3. If cfg-gated files changed: cross-lint or hand-audit against master (see above).
4. If wire types changed: `wire_format` tests green + `PROTOCOL_VERSION` bumped.
5. If locales changed: every `locales/*.yml` must have the same keys as
   `en.yml`; run `cargo test -p openlogi-desktop i18n`.
6. Only then `git push` / force-push to the PR branch.

## i18n (all locale files, then Crowdin)

- Add or change UI strings in **every** `crates/openlogi-ui/locales/*.yml` in
  the same PR. `en.yml` is the English source of truth (the English text IS the
  key); other files must not lag — the parity test fails the build.
- Crowdin improves non-English **values** over time. The sync job **merges**
  downloads into complete catalogs (`.github/scripts/i18n/merge_crowdin_download.py`):
  only real translations apply; English fill-in and sparse exports never wipe
  keys or open noise PRs.
- Details: [`.claude/rules/i18n.md`](.claude/rules/i18n.md).

## Subsystem rules — read before touching

Claude Code loads these automatically per path; other agents: read the listed file
before editing that area.

| Area | Rule file |
|---|---|
| `crates/openlogi-desktop/**`, `crates/openlogi-ui/**`, `crates/openlogi-overlay/**` (GPUI) | `.claude/rules/gui.md` |
| `crates/openlogi-ui/locales/**`, `openlogi-ui/src/locale.rs`, `openlogi-desktop/src/services/i18n.rs` | `.claude/rules/i18n.md` |
| `crates/openlogi-agent-core/**`, `crates/openlogi-agent/**`, `crates/openlogi-ipc/**` (IPC wire) | `.claude/rules/ipc-protocol.md` |
| `crates/openlogi-hidpp/**` (hard fork of `hidpp`) | `crates/openlogi-hidpp/AGENTS.md` |
| `crates/openlogi-hid/**` | `.claude/rules/hidpp.md` |
| `crates/openlogi-hook/**` (event taps) | `.claude/rules/hook.md` |
| `xtask/**`, `packaging/**`, `.github/scripts/**` | `.claude/rules/xtask.md` (+ `xtask/README.md`) |
| `crates/openlogi-desktop/src/platform/**`, `crates/openlogi-overlay/src/platform.rs` (ObjC FFI) | `crates/openlogi-desktop/src/platform/AGENTS.md` |
