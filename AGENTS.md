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

For runtime HID and input state, the long-running **GUI** and **overlay** are pure IPC
clients; the **agent** owns the input hook and HID I/O. The CLI is a diagnostic
exception: `openlogi list` prefers a compatible agent snapshot and falls back to
direct enumeration when none is available, while hardware-diagnostic subcommands
access devices directly.

| Crate | Role |
|---|---|
| `crates/openlogi` | The CLI binary — thin wrapper over `openlogi-cli` |
| `crates/openlogi-core` | Pure types: TOML config, device model, action catalog, locale negotiation. No I/O, no async (feature-gated host plumbing: `fs`, `locale`, and the `worker` thread) |
| `crates/openlogi-device-registry` | Pure hardware identity registry: receiver protocols and standalone-device driver metadata |
| `crates/openlogi-hidpp` | Hard fork of the `hidpp` protocol crate (**lib name `hidpp`**, 0BSD) |
| `crates/openlogi-hidpp-derive` | Private derive macro for `openlogi-hidpp` feature boilerplate |
| `crates/openlogi-fixture` | Host-free fixture schemas, synthetic identity policy, canonical semantic data, and privacy/relationship verification |
| `crates/openlogi-device` | The HID++ device layer: enumeration, probing, writes, sessions, pairing. Knows no host — expressed against `HidBackend` |
| `crates/openlogi-hid` | That layer wired to this host: `async-hid` transport, macOS Input Monitoring, the on-disk probe cache |
| `crates/openlogi-camera` | Cross-platform Logitech UVC enumeration, capture, controls, and Camera-permission APIs |
| `crates/openlogi-assets` | Device-render registry + cached fetch from OpenLogi asset mirrors |
| `crates/openlogi-cli` | CLI dispatch: agent-backed inventory when available, plus direct hardware diagnostics |
| `crates/openlogi-hook` | OS input capture: CGEventTap / evdev+uinput / WH_MOUSE_LL |
| `crates/openlogi-inject` | OS input synthesis: CGEvent / uinput+MPRIS / SendInput |
| `crates/openlogi-agent-core` | Shared agent orchestration: hook runtime, HID++ writes, DPI cycle, Actions Ring session state |
| `crates/openlogi-ipc` | The tarpc IPC contract (`src/ipc.rs`) + its local-socket transport, shared by the agent and its clients |
| `crates/openlogi-agent` | The `openlogi-agent` binary — runtime HID/input server |
| `crates/openlogi-permissions` | Privacy-permission status + System-Settings deep links: macOS TCC reads, Linux device-file probes. Reads only — never prompts |
| `crates/openlogi-ui` | Presentation shared by the two GPUI processes: action icons, colors, the GPUI asset source, and locale catalogs. Currently depends on `gpui`, not `gpui-component` |
| `crates/openlogi-desktop` | GPUI + gpui-component desktop app — polls the agent, no HID/input I/O |
| `crates/openlogi-overlay` | The `openlogi-overlay` binary — cursor-centred Actions Ring, a pure IPC client |
| `xtask` | `cargo xtask` maintenance: bundling, packaging, release manifest |

- IPC clients ↔ agent speak tarpc/bincode over an `interprocess` local socket. The wire
  format is versioned and **append-only** — read `crates/openlogi-ipc/AGENTS.md` before
  touching it.
- Three processes ship in the bundle — GUI, agent, overlay — and the overlay is a
  *sibling* of the GUI, not a part of it: it links `openlogi-ui`, never
  `openlogi-desktop`. Anything both need goes in `openlogi-ui`, and every dependency
  added there lands in the overlay too (`.agents/rules/gui.md` has the rule).
- Platform code is cfg-gated per crate (`[target.'cfg(target_os = …)'.dependencies]`).
  `.agents/rules/objc-ffi.md` is the contract for the workspace's macOS native FFI and
  maintains the canonical file-by-file inventory — read it before editing that surface.

## Evidence and root-cause discipline

- Treat every issue, user report, and review finding as a claim. Verify it against the
  current head and the most direct available evidence before accepting its diagnosis.
- Fix the verified root cause at its owning module and lifecycle boundary. Do not hide
  a broken owner or lifecycle behind a shim, fallback, or one-use abstraction.

## Single source of truth

- A decision every consumer must make the same way — a handshake step, a version
  policy, a deadline, a threshold, an encoding — has exactly one owner, and the owner
  exports the *decision*, not the ingredients. Clients call
  `openlogi_ipc::client::connect_as(kind)` and match `ConnectError::Skew`; they never
  see a raw version number to compare. Whatever consumers must not recombine stays
  private, or leaves the public surface with the consolidation.
- The second copy is the trigger, not the third. About to write a decision that already
  exists elsewhere — in another crate, in a test, in a different shape — stop, move the
  first copy to its owner, and consume it from both sites. Copies that differ are an
  investigation signal (`.agents/rules/rust.md`), never a licence to keep both.
- Every consolidation ships its guard: an ast-grep rule under `.ast-grep/rules/` that
  names the owner and fails on the ingredients anywhere else, so the next copy is a red
  `ast-grep` CI job (`cargo xtask ci ast-grep`; the prek hook runs it at commit), not a
  review comment. Token-level clone detectors were evaluated for this and rejected:
  they find copied text, and these copies were re-derivations that shared none.

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

Before any push, read and follow the [local gate and push checklist](.agents/rules/ci.md#local-gate-hard-stop-before-push--scale-it-to-the-affected-graph).
That file owns tier selection, commands, and the CI job map. Run the applicable
gate on the final tree; do not push a known-red tree or bypass the hooks.
A skipped job is **not** a pass. These procedures do not authorize a push.

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
refactoring, dependencies, and module layout — live in `.agents/rules/rust.md`,
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
- Before rebasing, managing issues, or preparing/adopting/reviewing/merging a PR, read the
  [GitHub workflow](docs/DEVELOPMENT.md#github-workflow). It owns fresh-base checks,
  PR format, contributor authorship, merge policy, and current-head CI handling.
- **All GitHub artifacts — PR titles/bodies, commits, issues, reviews, comments — are
  written in English.**
- **Never add AI attribution** ("Generated with …", AI co-author trailers) to commits,
  PRs, or issues — including when adopting contributors' work.
- Never post to external repos or reply publicly on the maintainer's behalf — draft the
  text for approval. Keep public drafts short, casual, and problem-focused.

## Releases

release-plz drives releases: one unified workspace version, ONE root `CHANGELOG.md`
(never per-crate changelogs), and a single `v{version}` tag that only release-plz
creates — **never hand-create the tag**. Published GitHub releases are immutable:
never re-run a failed release job or re-dispatch on an existing tag.
`release-plz.toml` is the versioning contract — don't trim it.

## Maintaining agent guidance

- Keep one source for each instruction. `AGENTS.md` owns global guidance;
  `CLAUDE.md` imports it. Shared rule files live in `.agents/rules/`;
  `.claude/rules` is only a relative symlink to that directory. Edit and link
  the canonical files. Keep crate-specific contracts in their own `AGENTS.md`
  and task procedures in skills. Link shared skills instead of copying them.
- Add a rule only for a **non-obvious, recurring, actionable** problem. Cite the
  repeated failure or review evidence. Put architecture explanations and long
  recipes in the developer docs; keep only essential boundaries and links here.
- During ordinary feature or bug work, propose a **Suggested guidance changes**
  section in the response or PR instead of editing guidance as a side effect.
  Apply it after maintainer review in a separate focused change. An explicit
  request to edit guidance authorizes that work directly.
- Prefer an existing lint, test, or ast-grep guard for a mechanically checkable
  invariant. Do not add prose as a substitute for enforcement or duplicate a rule
  that already has an owner. Keep imported skills and their source locks intact.

## Subsystem rules — read before touching

All agents must read the matching rules below and a crate's own `AGENTS.md`
before editing that area. `.agents/rules/` is the canonical store, not a
cross-client automatic loader. Claude Code discovers those files through the
`.claude/rules` symlink and applies their `paths` metadata. Other clients use
this index; do not assume they interpret Claude's `paths` field.
Keep this index as ordinary links, not unconditional imports of every rule.
See [agent guidance setup](docs/DEVELOPMENT.md#agent-guidance) for checkout and
client-loading checks, including Windows symlink requirements.

| Area | Rule file |
|---|---|
| reproducing CI jobs locally (every `ci.yml` job → command) | [.agents/rules/ci.md](.agents/rules/ci.md) |
| `.ast-grep/**`, `sgconfig.yml` (the single-source-of-truth guards) | [.agents/rules/ci.md](.agents/rules/ci.md) |
| any `*.rs` / `Cargo.toml` (workspace Rust standards) | [.agents/rules/rust.md](.agents/rules/rust.md) |
| `crates/openlogi-desktop/**`, `crates/openlogi-ui/**`, `crates/openlogi-overlay/**` (GPUI) | [.agents/rules/gui.md](.agents/rules/gui.md) |
| `crates/openlogi-desktop/**` (that crate's own contract and map) | `crates/openlogi-desktop/AGENTS.md` |
| locale catalogs/negotiation and each binary's `rust_i18n::i18n!` wiring | [.agents/rules/i18n.md](.agents/rules/i18n.md) |
| `crates/openlogi-ipc/**`, plus every crate whose serde types ride the wire (`openlogi-agent-core`, `openlogi-agent`, `openlogi-core`, `openlogi-hid`) | `crates/openlogi-ipc/AGENTS.md` |
| cfg-gated platform code, including hook/inject/hid, camera, and agent autostart/resume | [.agents/rules/cross-platform.md](.agents/rules/cross-platform.md) |
| `crates/openlogi-hidpp/**` (hard fork of `hidpp`) | `crates/openlogi-hidpp/AGENTS.md` |
| `crates/openlogi-device/**`, `crates/openlogi-hid/**` (the HID++ layer seam) | `crates/openlogi-device/AGENTS.md` |
| `crates/openlogi-hook/**` (event taps) | `crates/openlogi-hook/AGENTS.md` |
| `xtask/**`, `packaging/**`, `.github/scripts/**` | `xtask/AGENTS.md` (+ `xtask/README.md`) |
| macOS native FFI (the rule carries the canonical path inventory) | [.agents/rules/objc-ffi.md](.agents/rules/objc-ffi.md) |

## Task skills — invoke when the task matches

Load the skill when its task matches. If the client cannot invoke skills, read
the linked `SKILL.md` and the references it requires. For GPUI work, use the
upstream skills as the default design and coding practice; `.agents/rules/gui.md`
contains only OpenLogi integration constraints and verification entrypoints.

| Task | Skill |
|---|---|
| GPUI implementation, components, state, lifecycle, or testing | [gpui-kit](.agents/skills/gpui-kit/SKILL.md) |
| GUI layout, styling, interaction, copy, or design review | [gpui-kit-design-guides](.agents/skills/gpui-kit-design-guides/SKILL.md) |
| native UI verification, component gallery, mock-agent workflows, or visual/interaction regression tests | [testing-openlogi-ui](.agents/skills/testing-openlogi-ui/SKILL.md) |
| missing HID devices, failed opens or pairing, stale inventory, reconnect failures, unsupported features, or CLI/GUI disagreement | [diagnosing-openlogi-devices](.agents/skills/diagnosing-openlogi-devices/SKILL.md) |
| planning a regression test, selecting checks after changes, or verifying an authorized commit/push | [verifying-openlogi-changes](.agents/skills/verifying-openlogi-changes/SKILL.md) |
| recording, reviewing, or contributing device profiles and HID++ cassettes | [contributing-device-fixtures](.agents/skills/contributing-device-fixtures/SKILL.md) |
| a macOS report of no devices / "Failed to open device" / which permission to grant, and any change to the permission, helper-launch, or bundle-signing code | `.claude/skills/openlogi-macos-permissions/SKILL.md` |

The four OpenLogi workflow skills are maintained locally with the code. Keep
mandatory invariants in this file and the scoped rules; link to those rules from
skills rather than maintaining a second policy. Local skills have no upstream
entry in `skills-lock.json`.

The GPUI skills are imported from
[longbridge/gpui-kit](https://github.com/longbridge/gpui-kit/tree/959ccc5ea1ec23be8283c2c326467699a9b44729/skills),
with the upstream [Apache-2.0 license](.agents/skills/LICENSE-APACHE).
Marked reference files only normalize whitespace for repository hooks; keep the
upstream guidance intact. `skills-lock.json` records the upstream content hashes.
Track the files and lock together; review upstream changes before updating the
source revision above. Claude Code uses the tracked symlinks in `.claude/skills/`.
Other local skills remain ignored by Git.
