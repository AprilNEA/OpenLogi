# OpenLogi — Agent Guide

OpenLogi is a native, local-first alternative to Logitech Options+ written in Rust:
button remapping, DPI, SmartShift, and per-app profiles for Logitech HID++ devices
(Bolt/Unifying receiver, Bluetooth-direct, wired) — no account, no telemetry, plain-TOML
config. macOS and Linux are first-class; Windows is a young but shipping port.
Dual-licensed MIT/Apache-2.0; the `design/` brand assets are proprietary.

The developer handbook (toolchain, packaging, release pipeline) is
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). This file is the agent-facing contract:
the architecture map plus the global workflow. Everything subsystem-specific lives in
the path-scoped rule files indexed at the bottom — read the matching one before
touching an area.

## Architecture

Three tiers ship in one install: the **GUI** is a pure IPC client, the **agent** is a
background server owning the input hook and ALL device I/O, and shared orchestration
sits beneath both.

| Crate | Role |
|---|---|
| `crates/openlogi` | The CLI binary — thin wrapper over `openlogi-cli` |
| `crates/openlogi-core` | Pure types: TOML config, device model, action catalog. No I/O, no async |
| `crates/openlogi-device-registry` | Pure hardware identity registry: receiver protocols and standalone-device driver metadata |
| `crates/openlogi-hidpp` | Vendored fork of the `hidpp` protocol crate (**lib name `hidpp`**, 0BSD) |
| `crates/openlogi-device` | The HID++ device layer: enumeration, probing, writes, sessions, pairing. Knows no host — expressed against `HidBackend` |
| `crates/openlogi-hid` | That layer wired to this host: `async-hid` transport, macOS Input Monitoring, the on-disk probe cache |
| `crates/openlogi-assets` | Device-render registry + cached fetch from OpenLogi asset mirrors |
| `crates/openlogi-cli` | `clap` command tree: `list`, `assets`, `diag` |
| `crates/openlogi-hook` | OS input capture: CGEventTap / evdev+uinput / WH_MOUSE_LL |
| `crates/openlogi-inject` | OS input synthesis: CGEvent / uinput+MPRIS / SendInput |
| `crates/openlogi-agent-core` | Shared agent orchestration: hook runtime, HID++ writes, DPI cycle, Actions Ring session state |
| `crates/openlogi-ipc` | The tarpc IPC contract (`src/ipc.rs`) + its local-socket transport, shared by agent and GUI |
| `crates/openlogi-agent` | The `openlogi-agent` binary — hook + device I/O server |
| `crates/openlogi-permissions` | Privacy-permission status + System-Settings deep links: macOS TCC reads, Linux device-file probes. Reads only — never prompts |
| `crates/openlogi-ui` | Presentation shared by the two GPUI processes: ring geometry/icons, the GPUI asset source, locale negotiation. Depends on `gpui` but **not** `gpui-component` |
| `crates/openlogi-desktop` | GPUI + gpui-component desktop app — polls the agent, no device I/O |
| `crates/openlogi-overlay` | The `openlogi-overlay` binary — cursor-centred Actions Ring, a pure IPC client |
| `xtask` | `cargo xtask` maintenance: bundling, packaging, release manifest |

- GUI ↔ agent speak tarpc/bincode over an `interprocess` local socket. The wire format
  is versioned and **append-only** — read `crates/openlogi-ipc/AGENTS.md` before touching
  it.
- Three processes ship in the bundle — GUI, agent, overlay — and the overlay is a
  *sibling* of the GUI, not a part of it: it links `openlogi-ui`, never
  `openlogi-desktop`. Anything both need goes in `openlogi-ui`, and every dependency
  added there lands in the overlay too (`.claude/rules/gui.md` has the rule).
- Platform code is cfg-gated per crate (`[target.'cfg(target_os = …)'.dependencies]`).
  `.claude/rules/objc-ffi.md` is the contract for the workspace's macOS native FFI and
  indexes every file that carries any — read it before editing one. That surface spans
  seven crates: the agent's tray, the camera backends, the hook, the injector, the
  overlay, `openlogi-permissions`, and one file in the GUI.

## Build, run, verify

Nix/devenv is optional — rustup + `rust-toolchain.toml` is enough. If devenv is
installed, direnv loads it; otherwise `.envrc` prints a notice and leaves PATH
alone so system `cargo` works. With devenv active, cargo may only be on PATH
inside the shell — run from the repo root (or `direnv exec . …`), including
git (the hooks need cargo):

```sh
cargo check -p openlogi-core
# when cargo is only inside devenv:
direnv exec . cargo check -p openlogi-core
direnv exec . git commit …
```

### Verification while iterating (fast path)

Use the narrowest command that can disprove the change while code is still moving.
Do **not** run full-workspace Clippy, tests, or rustdoc after every edit; broad checks
are a final gate, not an inner development loop.

1. **Define one proof first.** Pick the focused test, compile target, or runtime
   behavior that demonstrates the requested outcome.
2. **Inner loop:** run that proof only. For Rust, prefer
   `cargo test -p <crate> <test-filter>` for behavior and `cargo check -p <crate>`
   for API/type feedback. A one-line or docs-only edit does not justify Clippy.
3. **Once the code is stable:** run formatter check, the relevant tests, and Clippy
   once for each crate actually changed (`cargo clippy -p <crate> --all-targets --
   -D warnings`). For a shared public API, `cargo check` its affected consumers;
   do not Clippy every consumer unless their source changed or `cargo check` exposes
   a problem there.
4. **Before push only:** choose the affected-package or full local gate below from
   the final diff. If a gate command fails, fix the cause with a focused command,
   then rerun that tier once after the tree is final again.

Do not rerun an identical broad command merely because a later edit touched an
unrelated file. Do rerun the focused check whose inputs changed. If no commit or push
was requested, the task does not need the push gate solely because this file documents
one; report the targeted verification that was actually relevant.

### Local gate (hard stop before push — scale it to the affected graph)

The local gate keeps predictable failures off a PR update; CI then sweeps the
whole workspace on its other hosts. CI does not run for an ordinary branch push
that has no open PR, so never use it as a substitute for the local tier.

A **Rust-bearing diff** changes Rust source or an input that controls how the Rust
workspace builds or is validated. A truly non-Rust diff does not need Rust commands
merely because it is being pushed. Run the applicable checks from
`.claude/rules/ci.md` instead (shell, Nix, packaging, and so on).

For a Rust-bearing diff, derive the **affected package set** from the final tree:
every changed workspace package plus every workspace package that depends on one of
them, transitively. Run `cargo tree --workspace --target all --invert <changed>` for
each changed package and take the union of workspace packages in the output. Count
that set, not edited crate directories — changing only `openlogi-core` still affects
much of the application. When the set is uncertain, use the full tier.

**Affected-package tier** — allowed only when all of these are true:

- no Rust-bearing commit was rebased and no conflict was resolved since the last
  full gate;
- the diff changes no workspace-wide build or validation input: any `Cargo.toml`
  or `build.rs`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/**`, lint/format/hook
  configuration, devenv configuration, CI workflows, or the local CI runner.

Run fmt plus Clippy and tests for the whole affected set, not just the packages
whose files changed:

```sh
export RUSTFLAGS="-D warnings"
cargo fmt --all -- --check
cargo clippy -p <affected>… --all-targets -- -D warnings
cargo test -p <affected>…
```

Repeat `-p` for every package in the set. The mandatory pre-push hook still runs
full-workspace Clippy and non-GUI rustdoc before Git contacts the remote; this tier
does not authorize skipping that backstop.

**Full tier** — required after a Rust-bearing rebase or conflict resolution, for
any workspace-wide input above, when the affected set cannot be derived reliably,
or whenever a subsystem rule explicitly requires it:

```sh
export RUSTFLAGS="-D warnings"   # CI sets this globally; clippy `-D warnings` is not the same
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps \
  --document-private-items --exclude openlogi-ui --exclude openlogi-desktop \
  --exclude openlogi-overlay --exclude openlogi-agent
# or: devenv tasks run openlogi:check
# every CI job this host can reproduce: cargo xtask ci
```

Exit non-zero in either tier → fix, rerun that tier on the final tree, then push.
Do not push a known-red tree "to see if CI likes it." CI is confirmation, not
the first compile.

The rustdoc step mirrors CI's `rustdoc (non-GUI crates)` job and catches what the
other three cannot: a broken intra-doc link is neither a compile error nor a clippy
lint. The GPUI crates are excluded because documenting them drags in the whole
graphics toolchain; everything else is covered by exclusion rather than by a list, so
a new crate is documented by default. The classic silent breakage — handing a trait
impl to a derive macro kills every `Type::trait_method` doc link — is explained in
`.claude/rules/rust.md`.

### Reproduce every CI job locally

The local gate is the host-OS subset. The pipeline is `.github/workflows/ci.yml`
(Linux clippy, macOS+Linux MSRV, rustdoc, Linux tests excluding desktop, macOS
`--all-targets` tests, typos, cargo-deny, Windows clippy, wasm portability, shell
lint). macOS-green is not that matrix. To run every job this machine can reproduce:

```sh
cargo xtask ci
cargo xtask ci --list           # job → command table
cargo xtask ci rustfmt clippy   # one job, names match CI
# or: devenv tasks run openlogi:ci
```

The runner sets `RUSTFLAGS=-D warnings` the way CI does. A skipped job (wrong
OS, missing `cargo-deny`, no MSRV toolchain) is **not** a pass — name it as not
run in the PR Testing section. Full map, including "if you changed X, run Y":
[`.claude/rules/ci.md`](.claude/rules/ci.md).

prek hooks (`prek.toml`): typos and `cargo fmt` at commit; full-workspace clippy
**and rustdoc** at push (rust-scoped, so non-Rust pushes skip it). Hooks are a
backstop, not a substitute for running the gate yourself after a rebase.

**Push checklist (agents):**

1. Rebase/merge conflicts fully resolved — no `<<<<<<<` left, no half-ported APIs.
2. Applicable local gate green on the **final** tree: non-Rust checks for a
   non-Rust diff; affected-package tier when none of the full-tier triggers above
   applies; full otherwise.
3. Additional pipeline jobs required by the diff run by name with
   `cargo xtask ci <job>…`. Skipped jobs stay named as not run — never claimed
   green. Mapping: `.claude/rules/ci.md`.
4. If cfg-gated files changed (any `#[cfg(target_os = …)]` block, in any crate):
   cross-lint or hand-audit against master — macOS-green proves nothing there; see
   `.claude/rules/cross-platform.md`.
5. If wire types changed: `PROTOCOL_VERSION` bumped and
   `cargo test -p openlogi-ipc --test wire_format` green — see
   `crates/openlogi-ipc/AGENTS.md`.
6. If locales changed: every `crates/openlogi-ui/locales/*.yml` carries the same keys
   as `en.yml` (new keys at the same position); run
   `cargo test -p openlogi-desktop i18n` — see `.claude/rules/i18n.md`.
7. Only then `git push` / force-push to the PR branch.

### Running the app

- Dev-run with `cargo run -p openlogi-desktop` — a cargo runner wraps the build into
  `target/dev/OpenLogi.app` with the same identity, helper and plist tables packaging
  uses. The macOS GUI build needs full Xcode for GPUI's Metal shaders; devenv sets the
  env when present (`direnv reload` if the shader compile fails there).
- `cargo build` does NOT refresh that bundle, and a second instance exits on the
  singleton lock: quit the old instance and re-`run` before judging a UI change
  "not applied".
- Each dev run first stops the agent and overlay the previous one left behind — they
  are LaunchServices-launched (for their own TCC identity), not children of the GUI,
  and a surviving agent relaunches itself ~20 s later — then starts the freshly
  built agent and waits for its socket, so the GUI's first IPC connect succeeds
  instead of exercising the production spawn-on-unreachable fallback.
  `OPENLOGI_DEV_AGENT=0` opts out of all of it.
- No hardware attached? `cargo run -p openlogi-agent --bin openlogi-agent-mock` serves
  a scripted inventory over the dev IPC socket, so the GUI runs unmodified and the
  production app stays untouched.
- Mechanics, dev profiles, and the mock's scope: `docs/DEVELOPMENT.md`.

## Rust standards

Edition 2024, MSRV = current stable (1.98), one shared workspace lint table. The
floor tracks stable instead of trailing it — raise it the day a release ships
something worth using, and run `devenv update rust-overlay` with it so the local
toolchain stops being older than CI's. The full standards — the
lint table and what it changes day to day, typed-invariant style, house rules on
refactoring, dependencies, and module layout — live in `.claude/rules/rust.md`,
loaded for any Rust or `Cargo.toml` edit.

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
  hardware" when true — real-hardware verification is the maintainer's job, so every
  fix PR states how to test it), and a closing `Fixes #N` line. Screenshots for UI
  changes.
- **All GitHub artifacts — PR titles/bodies, commits, issues, reviews, comments — are
  written in English.**
- **Never add AI attribution** ("Generated with …", AI co-author trailers) to commits,
  PRs, or issues — including when adopting contributors' work.
- Never post to external repos or reply publicly on the maintainer's behalf — draft the
  text for approval. Keep public drafts short, casual, and problem-focused.
- Contributor PRs are adopted, not rejected: check `maintainerCanModify`, rebase onto
  **fresh** master in a worktree, fix review findings, run the applicable local gate
  on the rebased tip (a Rust-bearing rebase takes the full tier), **then** push to the
  fork branch; preserve authorship (`Co-authored-by` when re-homing work).
  Squash-then-rebase is fine when the PR is far behind and commit-by-commit conflicts
  thrash.
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

## Subsystem rules — read before touching

Claude Code loads the `.claude/rules/` files per matching path and a crate's
own `AGENTS.md` when working inside it; other agents: read the listed file
before editing that area.

| Area | Rule file |
|---|---|
| reproducing CI jobs locally (every `ci.yml` job → command) | `.claude/rules/ci.md` |
| any `*.rs` / `Cargo.toml` (workspace Rust standards) | `.claude/rules/rust.md` |
| `crates/openlogi-desktop/**`, `crates/openlogi-ui/**`, `crates/openlogi-overlay/**` (GPUI) | `.claude/rules/gui.md` |
| `crates/openlogi-desktop/**` (that crate's own contract and map) | `crates/openlogi-desktop/AGENTS.md` |
| `crates/openlogi-ui/locales/**`, `openlogi-ui/src/locale.rs`, `openlogi-desktop/src/services/i18n.rs` | `.claude/rules/i18n.md` |
| `crates/openlogi-ipc/**`, plus every crate whose serde types ride the wire (`openlogi-agent-core`, `openlogi-agent`, `openlogi-core`, `openlogi-hid`) | `crates/openlogi-ipc/AGENTS.md` |
| `crates/openlogi-hook/**`, `crates/openlogi-inject/**`, `crates/openlogi-hid/**` (cfg-gated platform code) | `.claude/rules/cross-platform.md` |
| `crates/openlogi-hidpp/**` (hard fork of `hidpp`) | `crates/openlogi-hidpp/AGENTS.md` |
| `crates/openlogi-device/**`, `crates/openlogi-hid/**` (the HID++ layer seam) | `crates/openlogi-device/AGENTS.md` |
| `crates/openlogi-hook/**` (event taps) | `crates/openlogi-hook/AGENTS.md` |
| `xtask/**`, `packaging/**`, `.github/scripts/**` | `xtask/AGENTS.md` (+ `xtask/README.md`) |
| macOS native FFI wherever it lives — `openlogi-{agent,camera,hook,inject,overlay,permissions}` + `openlogi-desktop/src/platform/**` | `.claude/rules/objc-ffi.md` |

## Task skills — invoke when the task matches, not when a path matches

The rules above load from the file you are editing. Some work has no file to key
on: triaging a user report, or deciding what a symptom means. That lives in
`.claude/skills/`, which Claude Code offers by task description; other agents
should read the `SKILL.md` when the task matches.

| Task | Skill |
|---|---|
| a macOS report of no devices / "Failed to open device" / which permission to grant, and any change to the permission, helper-launch, or bundle-signing code | `.claude/skills/openlogi-macos-permissions/SKILL.md` |

Everything else under `.claude/skills/` is a per-developer symlink into
`.agents/skills/` and is not part of the project — see `.gitignore`.
