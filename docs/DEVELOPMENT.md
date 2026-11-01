# Developing OpenLogi

This document covers the local development workflow for OpenLogi. For end-user
build instructions, see the [README](../README.md).

## Toolchain

- Stable Rust (Edition 2024, MSRV 1.99 — the floor tracks current stable)
- macOS: Xcode 26+ with the optional **Metal Toolchain** component. The Metal
  Toolchain is what GPUI's `gpui_macos` build script compiles shaders with; the
  version floor is `actool`, which packaging uses to compile the app icon from
  its Icon Composer document. `OPENLOGI_DEVELOPER_DIR` overrides which Xcode is
  used when several are installed.
- Linux: system libraries — on Debian/Ubuntu:
  `sudo apt-get install libudev-dev gcc g++ clang libfontconfig-dev libwayland-dev libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev pkg-config`
- `create-dmg` for packaging (`brew install create-dmg`); `cargo-bundle` is
  installed automatically by `cargo run -p xtask -- macos bundle`

## Building from source

Nix/devenv is optional. A normal Rust toolchain is enough.

### Without Nix

```sh
# rustup installs the stable toolchain pinned in rust-toolchain.toml
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# macOS: full Xcode 26+ with the Metal Toolchain (not only Command Line Tools)
# Linux: see system libraries under Toolchain above
# optional helpers: brew install cmake create-dmg sccache
git clone https://github.com/AprilNEA/OpenLogi
cd OpenLogi
cargo run -p openlogi --release -- list
cargo run -p openlogi-desktop --release
```

If you use [direnv](https://direnv.net) without devenv installed, `.envrc`
prints a notice and leaves your shell alone. Install rustup/cargo yourself
and keep working.

### With devenv (optional)

`devenv.nix` provisions sccache, the stable Rust toolchain, platform libraries,
nfpm on Linux, and the macOS packaging/env helpers GPUI needs
(`create-dmg`, `DEVELOPER_DIR`, and `SDKROOT`). Tasks:

```sh
devenv tasks run openlogi:gui      # run the desktop app
devenv tasks run openlogi:check    # host-OS gate: fmt + clippy + tests + rustdoc
devenv tasks run openlogi:ci       # every GitHub Actions CI job this host can reproduce
devenv tasks run openlogi:dmg      # build the macOS DMG
devenv tasks run openlogi:i18n-upload    # upload English source strings to Crowdin
devenv tasks run openlogi:i18n-download  # download translations and run i18n tests
```

After a `devenv.nix` change, reload direnv so the new env takes effect:

```sh
direnv reload    # or: exit your shell and `cd` back in
```

Without that, GPUI's `gpui_macos` build script can't find Apple's `metal`
shader compiler, and link errors about missing `_write` / `_sysconf` /
`_waitpid` symbols show up because the Nix `apple-sdk-14.4` stub doesn't
expose `libSystem` the way Apple's real linker wants.

### Nix package

The Flake in `packaging/` exposes native `x86_64-linux` and `aarch64-linux`
packages plus the NixOS module. It is separate from the devenv shell. Nix only
looks for a Flake in the working directory and its parents, so name it from the
repository root:

```sh
nix flake check ./packaging --all-systems --no-build  # evaluate every output
nix build ./packaging#openlogi                        # build + test this host's package
nix run ./packaging#openlogi -- list                  # run the packaged CLI
```

The package expression and NixOS module live beside the other Linux packaging
inputs in `packaging/linux/`. `nix fmt` formats all Nix expressions through the
Flake's pinned formatter. It takes no Flake argument, so run it from
`packaging/`.

The root `flake.nix` is a deprecated alias for existing
`github:AprilNEA/OpenLogi` inputs. It has no committed lock file, prints a
warning on every evaluation, and will be removed in a later release.

### Dev app bundle (macOS)

On macOS the desktop binary is launched from inside a throwaway
`target/dev/OpenLogi.app` — a Cargo `runner` wired in `.cargo/config.toml`
(`.cargo/run-macos.sh`) that hands the build to `xtask macos dev-bundle`. This
makes the dev build show as **OpenLogi Dev** in the menu bar and Dock, with the
real app icon; a bare `cargo run` binary has no bundle, so macOS would otherwise
fall back to the `openlogi-desktop` executable name and a generic icon. The
binary is hardlinked in (no copy) unless the bundle is being signed, and the
icon is generated on demand. The runner is a transparent passthrough for
everything else (the CLI, tests); set `OPENLOGI_DEV_BUNDLE=0` to launch the raw
`openlogi-desktop` binary instead.

Each run also stops the dev agent and overlay left behind by the previous one,
then starts the freshly built agent and waits for its IPC socket before the
GUI launches — so the window connects immediately instead of sitting on its
connecting frame while the GUI's production fallback re-spawns the agent.
The helpers are launched through LaunchServices so they get their own TCC
identity, which also means they are not children of the GUI: closing its
window or pressing Ctrl-C ends only the GUI, and a surviving dev agent
relaunches itself ~20 s later once its watcher notices the rewritten binary.
Set `OPENLOGI_DEV_AGENT=0` to run against an agent you started yourself —
nothing is stopped, built, embedded, or started then.

Packaged local dev bundles (`cargo run` and
`cargo run -p xtask -- macos bundle`) use `-dev` bundle identifiers and the
`openlogi-dev` XDG profile (`~/.config/openlogi-dev`,
`~/.local/share/openlogi-dev`, and its own `agent.sock`). That keeps the dev
GUI and agent from sharing the installed production app's Accessibility grant,
single-instance lock, config, or IPC socket.

Those identifiers are a channel, not a guess from the build type:
`macos bundle` takes `--channel dev|production` (dev by default) and verifies
what it stamped, and `macos dmg` refuses a non-production bundle once it is
given a signing identity. Reproduce the shipped layout locally with
`--channel production`, but don't sign and run it — it would take over the
installed app's grants and config, which is exactly what releases
0.6.24–0.6.26 did in reverse.

To install the CLI binary on `PATH`:

```sh
cargo install --path crates/openlogi
```

### macOS input-hook safety

The agent's input-hook watchdog gives CoreGraphics permission probes a 10-second budget. After a probe returns, tap servicing and teardown use a fresh 1.5-second budget. Teardown keeps that short budget when Accessibility is revoked or the re-arm limit is reached. The watchdog exits the agent if cleanup stalls, so macOS releases the process-owned tap.

## Developing the GUI without hardware

`openlogi-agent-mock` serves the real agent IPC contract from a scripted
in-memory inventory, so the desktop app can be developed with no Logitech
device (or receiver) attached:

```sh
cargo run -p openlogi-agent --bin openlogi-agent-mock   # then, in another terminal:
OPENLOGI_DEV_AGENT=0 cargo run -p openlogi-desktop
```

The mock defaults itself to the `openlogi-dev` profile (as if `OPENLOGI_PROFILE=dev`
were set), which is the profile the dev app bundle already uses — so it meets the
dev GUI on the dev socket, and an installed *release* build, which is on the
production profile, keeps running untouched. (A locally built bundle installed
into `/Applications` carries `-dev` identifiers and therefore shares the dev
profile: it and the mock contend for the same lock, and whichever starts second
exits.) `OPENLOGI_DEV_AGENT=0` keeps the runner from building and embedding
the real agent for the GUI to auto-spawn; add `OPENLOGI_ALLOW_EXTERNAL_AGENT=1`
if your installed production agent is running, since the runner's guard against
it predates the profile split and cannot know the dev GUI is on a separate
socket. Pass `OPENLOGI_PROFILE=prod` to serve the production socket instead; the
mock then contends for the production agent's single-instance lock and refuses
to start while it is running.

The script covers an online mouse (DPI and SmartShift writes persist and read
back, battery drains so poll-driven repaints are visible), an offline mouse, a
lighting-capable keyboard, a directly-attached device, and a full Bolt pairing
flow (discovery → passkey → paired). Its agent version carries a `-mock` suffix,
so a mock session is identifiable in the UI. It is a dev tool only and is never
bundled.

While the mock runs, type a command in its terminal and press Enter to change
what it reports. `help` lists the commands, and `devices` numbers the devices
that the other commands address:

```text
devices              # 1 is the online Bolt mouse in the built-in script
battery 1 5          # it reads 5% and the overlay shows a low-battery notice
offline 1            # it goes offline; reads and writes fail
fail 3 error         # the keyboard stays online, but every request fails
lock caps on         # the overlay shows the Caps Lock indicator
backlight 2 4        # the overlay shows backlight level 2 of 4
disconnect           # every client loses its connection and reconnects
```

The low-battery notice and the lock-key indicator follow the notification
switches in Settings, as they do with the real agent. `advance <seconds>` moves
time only in a `--fixture` run.

The proposed architecture for recorded device profiles, raw HID++ replay, and
deterministic hardware scenarios is documented in
[Mock device and hardware record/replay architecture](MOCK_DEVICE_TESTING.md).

### Component gallery

Use the debug-only component gallery to review shared controls across light and
dark themes and every supported interface scale without config, IPC, or hardware:

```sh
OPENLOGI_COMPONENT_GALLERY=1 cargo run -p openlogi-desktop
```

Gallery mode opens one isolated window and bypasses the normal single-instance,
config, agent, asset-sync, and updater startup paths. The environment variable is
ignored by release builds.

## Project layout

```
crates/
  openlogi/         the `openlogi` binary — a thin wrapper over openlogi-cli
  openlogi-core/    types, config (TOML), paths, button + action catalog — no HID, no async
  openlogi-inject/  OS input synthesis: CGEvent, uinput/MPRIS, and SendInput
  openlogi-hidpp/   vendored HID++ protocol crate (lib name `hidpp`)
  openlogi-hid/     device discovery, HID++ reads/writes, and control capture over async-hid
  openlogi-assets/  device-render registry schema + cached HTTP fetch from OpenLogi asset mirrors
  openlogi-cli/     CLI implementation: command tree + `run()`, called by the `openlogi` binary
  openlogi-flow/    Flow protobuf protocol, framing, pairing, identity, and QUIC transport
  openlogi-agent-core/  shared orchestration + the agent/GUI IPC contract
  openlogi-agent/   the `openlogi-agent` binary — background agent owning device I/O and the hook
  openlogi-hook/    OS mouse hook: macOS CGEventTap, Linux evdev/uinput, Windows WH_MOUSE_LL
  openlogi-ui/      presentation shared by the two GPUI processes: ring geometry/icons,
                    the GPUI asset source, locale negotiation — gpui, no gpui-component
  openlogi-desktop/     the `openlogi-desktop` binary — GPUI + gpui-component IPC client
  openlogi-overlay/ the `openlogi-overlay` binary — the cursor-centred Actions Ring
```

## Agent guidance

Shared rules have one tracked source: [`.agents/rules/`](../.agents/rules/).
Edit and link those `.md` files, not a client-specific copy. The tracked
`.claude/rules` symlink points to `../.agents/rules` inside the checkout.
Existing references to `.claude/rules/<name>.md` still resolve through that alias.
Crate-specific contracts stay in each crate's `AGENTS.md`; task workflows stay
in `.agents/skills/`. No rule generator or install step is required.

The root [AGENTS.md](../AGENTS.md) holds global instructions and the rule index.
`.claude/CLAUDE.md` imports only that entrypoint. Claude Code discovers `.md`
rules through `.claude/rules` and uses their `paths` metadata for conditional
loading.
Other clients must follow the index; `.agents/rules/` is not a universal
automatic discovery path. Keep the index as ordinary Markdown links: importing
every rule from the root would load unrelated guidance into Claude's context.

### Check the checkout and client loading

From the repository root, in a POSIX shell or Git Bash:

```sh
git ls-files --stage -- .claude/rules .agents/rules
test -L .claude/rules && test -f .claude/rules/rust.md
readlink .claude/rules
```

Expect regular rule files under `.agents/rules/` and one `120000` entry
for `.claude/rules`, whose link target is `../.agents/rules`. The file checks
must succeed too: the index mode alone does not prove a working symlink.

On Windows, enable Developer Mode or obtain symlink creation permission before
cloning with `git clone -c core.symlinks=true <repository-url> <new-directory>`.
With `core.symlinks=false`, Git writes a text file containing the target instead
of a directory link; Claude cannot discover the rules through it. Changing the
config alone does not repair an existing checkout. Preserve local changes and
use a fresh symlink-enabled checkout. Until then, read the canonical rules via
the root index; do not replace the alias with independently maintained copies.

Verify loading in the client, not only the filesystem. In a fresh Claude Code
session, use `/context` or an `InstructionsLoaded` hook to inspect loaded files:

- Read `README.md`: Rust and GUI path rules should not load solely from that read.
- Read `crates/openlogi-core/src/lib.rs`: the Rust rule should load, but not GUI.
- Read `crates/openlogi-desktop/src/app.rs`: the GUI rule should now load too.

Follow [Claude's loading diagnostics](https://code.claude.com/docs/en/memory#troubleshoot-memory-issues)
if those results differ. Filesystem checks and unchanged `paths` metadata are
not evidence that a particular client version loaded the rules correctly.

## Local CI

The PR test pipeline is `.github/workflows/ci.yml`. To run every job this
machine can reproduce — including typos, the ast-grep guards, MSRV, cargo-deny,
and the Windows cross-lint the host-OS gate does not run:

```sh
cargo xtask ci
cargo xtask ci --list                        # job → command table
devenv tasks run openlogi:ci                 # same, from devenv
```

The runner sets `RUSTFLAGS=-D warnings` the way CI does. Jobs that need another
OS are reported as skipped; a skip is not a pass. The full job map (and which
diff requires which job) is [`.agents/rules/ci.md`](../.agents/rules/ci.md).

For structural searches and guard changes, use the [ast-grep skill](../.agents/skills/ast-grep/SKILL.md). The [guard workflow](../.agents/rules/ci.md#ast-grep-rules) defines placement beside the owner, root discovery, and verification.

### Pre-push gate

Before pushing, read [the local gate and push checklist](../.agents/rules/ci.md#local-gate-hard-stop-before-push--scale-it-to-the-affected-graph).
That file owns tier selection, exact commands, and additional checks required by
the diff. `devenv tasks run openlogi:check` runs the full host-OS tier, not the
whole CI pipeline. A Rust-bearing rebase or conflict resolution requires the
full tier. Non-Rust changes use the applicable non-Rust checks.

## GitHub workflow

Read this section before preparing, adopting, reviewing, or merging a PR.
These procedures do not authorize remote writes: obtain approval before pushing,
opening or merging PRs, publishing, or approving/rerunning workflows.

### Preparing and merging PRs

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
- Issues use the bug/feature/device forms and the `type:`/`area:`/`platform:`/`needs:`/
  `status:` label families. Deferred or out-of-scope work becomes a linked issue, not a
  TODO comment.
- Discussions route by category: Q&A for usage questions (mark the reply that solves
  it), Ideas for feature wishes, General for the rest. The forms in
  `.github/DISCUSSION_TEMPLATE/` carry this routing; device requests go to the device
  issue form. When an issue tracks a discussion, link it and close the discussion as a
  duplicate. Close a discussion whose request has shipped as resolved.

### Adopting contributor PRs

Contributor PRs are adopted, not rejected: check `maintainerCanModify`, rebase onto
**fresh** master in a worktree, fix review findings, run the applicable local gate
on the rebased tip (a Rust-bearing rebase takes the full tier), **then** push to the
fork branch; preserve authorship (`Co-authored-by` when re-homing work).
Squash-then-rebase is fine when the PR is far behind and commit-by-commit conflicts
thrash.

### CI / Actions when adopting PRs

- CI concurrency is **per branch** (`ci-${{ workflow }}-${{ ref }}` with
  `cancel-in-progress: true`). Approving or re-running an **old SHA** on the same
  branch cancels the current-head run. Only approve / re-run workflows whose
  `head_sha` equals the PR's current head.
- After a force-push, wait for the new runs; do not re-approve stale
  `action_required` jobs from earlier commits on that branch.
- First-time-fork PRs may sit in `action_required` until a maintainer approves the
  workflow run — that is fine; still do not push until the local gate is green.

## Packaging the macOS DMG

```sh
cargo run -p xtask -- macos package    # → target/release/OpenLogi.dmg
# Cross-compile a distribution DMG (aarch64 or x86_64):
cargo run -p xtask -- macos package --target x86_64-apple-darwin
```

Environment overrides:

- `OPENLOGI_BUNDLE_ASSETS=1` — bundle every device render into the `.app` for a
  fully offline build (default: fetched on demand at first launch).
- `OPENLOGI_SIGN_IDENTITY=<identity>` — codesign the `.app` and `.dmg` with the
  given Developer ID.
- `OPENLOGI_SPARKLE_PUBLIC_ED_KEY=<base64>` — embed Sparkle.framework and update
  through it (see Release updater publishing). Without it the bundle updates
  through the built-in engine.
- `OPENLOGI_UPDATE_BASE_URL=<url>` — update host of the embedded Sparkle feed;
  required with `OPENLOGI_SPARKLE_PUBLIC_ED_KEY`.
- `OPENLOGI_DMG_BACKGROUND_URL=<url>` — override the branded DMG background
  TIFF URL (default: `https://assets.openlogi.org/dmg/dmg-background.tiff`).

The local packaging command and release workflow both use the same branded DMG
layout: a 760×480 background image in a 760×512 Finder window, with 128px icons
positioned at `(212, 250)` for `OpenLogi.app` and `(548, 250)` for
`Applications`.

## Packaging Linux `.deb` / `.rpm` / `.pkg.tar.zst`

Requires [nfpm](https://nfpm.goreleaser.com/) on `PATH`; the package arch is
derived from the host (override with `PKG_ARCH`):

```sh
cargo run -p xtask -- linux package
# → target/release/*.deb / *.rpm / *.pkg.tar.zst
```

The package contents (binaries, udev rules, systemd user unit, desktop entry,
icon) are declared in `packaging/linux/nfpm.yaml`.

After nfpm, `linux package` re-encodes two of the packages: the `.deb` data
member becomes `data.tar.xz` (xz level 9), and the `.pkg.tar.zst` becomes a
level-19 zstd frame with a 128 MiB window. The four binaries share most of
their code, and only a window that spans the whole tar finds it; this halves
both packages. Leave `deb.compression` unset in `nfpm.yaml`, because the
rewrite expects nfpm's gzip data member. The `.rpm` keeps nfpm's compression.
The two encodes add about three minutes per architecture (164 s on a 6-core
x86_64 machine).

`packaging/linux/install.sh` is a POSIX `/bin/sh` frontend for those release
packages, not a second package lifecycle. Its online mode maps the host and
package manager to the release naming contract
`openlogi-v<version>-linux-<amd64|arm64>.<format>`, authenticates that file's
detached signature against its embedded minisign public key, verifies only that
file's `SHA256SUMS` entry, and then delegates installation to apt, dnf, yum,
zypper, rpm, or pacman. The embedded public key must stay in sync with
`OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY`. The nFPM post-install script remains the
owner of udev and desktop/icon cache reloads. The explicit `--from-source` mode
preserves the checkout installer for the four local `target/release` binaries
and shared resources.

Run the mocked online/source smoke suite directly with dash; it exercises
latest and pinned versions, both architectures, every package-manager mapping,
signature and checksum rejection before sudo, dry-run, and the source resource
set without network or system writes:

```sh
dash -n packaging/linux/install.sh
dash packaging/linux/tests/install-smoke.sh
```

The Nix package uses the same shared resources and is declared in
`packaging/linux/package.nix`; see the Nix package section above for its build
commands.

## Installation-source detection

The desktop app probes once in the background and publishes the typed
`platform::installation::Installation` global: `Detecting`, then
`Detected(InstallationSource)`. It also logs `detected installation source`.
Settings → Updates displays the result as **Installation source**, separately
from the update download source. An open window refreshes when detection
completes or the interface language changes.
This is an ownership snapshot, not download provenance or an update policy;
the updater does not yet change behavior based on it.

- **Homebrew:** matches the installed receipt and Caskroom app back-link to
  the running bundle, distinguishing `openlogi` from `openlogi@latest`. It
  checks both standard prefixes, `HOMEBREW_PREFIX`, and prefixes discoverable
  from `PATH`, without executing brew. An undiscoverable custom prefix cannot
  be recognized. Other macOS bundles report `MacAppBundle`, not "DMG".
- **Linux:** recognizes a resolved `/nix/store/` executable, or queries dpkg,
  rpm, and pacman for ownership of the exact executable by `openlogi`.
  Package queries are read-only, with a two-second timeout per command.
- **Windows:** the MSI writes its `InstallLocation` under
  `HKCU\Software\OpenLogi`; only a matching executable is `WindowsMsi`.
  The ZIP carries `openlogi-installation.json` next to `OpenLogi.exe` and is
  `WindowsPortable`. A matching MSI registration takes precedence.
- **Unknown:** unmarked Windows releases predating these markers, bare
  source/manual installs, or otherwise inconclusive ownership. Missing
  metadata never implies a portable ZIP or a DMG.

## Release updater publishing

Tagged releases attach artifacts and `SHA256SUMS` to GitHub Releases for manual
downloads. When both Linux build legs succeed, the release workflow copies
`packaging/linux/install.sh` to `dist/install.sh`; that exact file is listed in
`SHA256SUMS`, signed as `install.sh.minisig`, and attached as a release asset.
Partial releases without a complete Linux package set omit the installer. The
workflow also publishes the artifacts to Cloudflare R2 and writes a static
updater manifest at:

```text
${OPENLOGI_UPDATE_BASE_URL}/channels/stable/latest.json
```

The app embeds the update host at build time via `OPENLOGI_UPDATE_BASE_URL`
(unset or empty means `https://updates.openlogi.org`) and derives the manifest
URL from it with `openlogi_core::update_feed`, the module that owns the
channel layout on the host. Release builds also embed `OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY`
and run with `Verification::Strict`: an update is installed only if the manifest
asset carries a minisign signature that verifies against that key, plus a
matching SHA-256. A build without the key embedded (local/dev) fails closed —
the update check errors rather than installing an unverified artifact.

The app follows one update channel per launch, which
`UpdateChannel::followed` decides. A channel picked in Settings → Updates
(`update_channel` in `config.toml`) wins. Without one, a pre-release such as
`0.8.17-beta.12` follows `beta` and a final release follows `stable`. The
engine reads `${OPENLOGI_UPDATE_BASE_URL}/channels/<channel>/latest.json`.
Sparkle keeps reading the stable appcasts below and, on `beta`, also offers
their items tagged `<sparkle:channel>beta</sparkle:channel>`. The beta
channel's `latest.json` offers the newest beta when it is newer than the
newest stable release, and that stable release otherwise. So an app on `beta`
gets every stable release too, with one exception. The engine compares
release versions and never offers a lower one, so an app on a beta numbered
past a release cut after it stays on that beta until a release numbered past
it. This happens when the release PR's version goes down after the beta,
whether the later release is stable or another beta. Sparkle compares build
versions and moves the app to the later release. `release channels` warns
whenever a beta is numbered past what the beta channel offers.

### macOS: Sparkle

The signed macOS release bundle embeds
[Sparkle](https://sparkle-project.org) and updates through it instead
(`platform::updater` in `openlogi-desktop`). Each architecture ships its own
DMG, so each reads its own feed:

```text
${OPENLOGI_UPDATE_BASE_URL}/channels/stable/appcast-arm64.xml
${OPENLOGI_UPDATE_BASE_URL}/channels/stable/appcast-x86_64.xml
```

Each feed carries the newest stable release's item. When a beta is newer, the
beta's item comes before it, tagged `<sparkle:channel>beta</sparkle:channel>`.
An item is that architecture's notarized, stapled DMG with an EdDSA
(`sparkle:edSignature`) signature over the exact file, and the release notes
as Markdown. The bundle trusts only the key in its `SUPublicEDKey`, and
`SUVerifyUpdateBeforeExtraction` makes Sparkle check that signature before it
unpacks anything.

Updates stay opt-in. The bundle ships `SUEnableAutomaticChecks = NO`, so
Sparkle never schedules a check or asks to; the app runs the one launch check
itself when **Check for updates** is on, without a window. With **Automatic
updates** also on, a found update downloads in the background and installs on
quit or through **Restart to Update**. "Check for Updates…" opens Sparkle's
update window. A bundle without Sparkle (Windows, Linux, local builds) and a
Sparkle that fails to load fall back to the engine and `latest.json`.

`latest.json` and the DMG minisigs keep being published: bundles from before
Sparkle read only that manifest, and they update into a Sparkle bundle through
it.

### Release secrets: 1Password Environments and the `release` environment

Every workflow reads its credentials from 1Password through the shared
[`AprilNEA/actions/op-environment`](https://github.com/AprilNEA/actions)
action, which runs `op run --environment` with a service-account token and
exports the Environment's variables, masked, to the job's later steps
(`GITHUB_ENV`, so a step reads `$APPLE_ID` or `${{ env.APPLE_ID }}`). GitHub
App tokens are then minted by its sibling `github-app-token` from
`env.GITHUB_APP_ID` / `env.GITHUB_APP_PRIVATE_KEY`. Both are pinned by commit
SHA with the version as a comment, like every action that handles a secret.
Two [1Password Environments](https://developer.1password.com/environments)
hold them, each read by a service account scoped to that Environment alone, so
the vaults the items used to live in no longer matter:

| 1Password Environment | Variables | Service account token lives in | Read by |
|---|---|---|---|
| `OpenLogi Automation` | `GITHUB_APP_ID`, `GITHUB_APP_PRIVATE_KEY`, `CARGO_REGISTRY_TOKEN`, `CROWDIN_PROJECT_ID`, `CROWDIN_PERSONAL_TOKEN` | repository secret `OP_SERVICE_ACCOUNT_TOKEN` | release-plz, the beta cut, Crowdin, the flake update, and the release workflow's installer-site / homebrew-tap dispatches, from any branch |
| `OpenLogi Release` | the `APPLE_*`, `AZURE_*`, `OPENLOGI_UPDATE_*`, `OPENLOGI_SPARKLE_*` and `CLOUDFLARE_R2_*` variables listed below | **`release` GitHub environment** secret `OP_SERVICE_ACCOUNT_TOKEN` | the signing jobs in `build.yml` (`macos`, `windows`, `windows-msi`) when `sign` is set, and `release.yml`'s `publish` job |

The Environment IDs are not secrets; they are the repository variables
`OP_AUTOMATION_ENVIRONMENT_ID` and `OP_RELEASE_ENVIRONMENT_ID`. A GitHub
environment secret shadows the repository secret of the same name, so a job
sees the release token only inside the `release` GitHub environment, and a job
outside it that asks for the release Environment fails in `op run` with the
automation token. The `release` GitHub environment admits only `v*` tag refs
and requires a review, so a branch dispatch with `sign` fails before any
secret is read, and each tag run pauses twice for approval: once when the
build legs reach the environment, once when `publish` does. The Azure
federated credential is bound to the environment
(`repo:AprilNEA/OpenLogi:environment:release`), which is why the Windows jobs
were in it first.

`op-environment` exports whatever the Environment holds, so a new variable
needs no workflow change beyond its consumer; a variable the Environment
lacks is simply unset, and each job's validation step names the missing one.
Everything exported is visible to every later step of that job, third-party
actions included, which is one more reason the two Environments stay apart.

The `OpenLogi Release` Environment must contain the Apple signing values
`xtask/README.md` describes (`APPLE_SIGNING_IDENTITY`, `APPLE_CERTIFICATE`,
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`,
`APPLE_AGENT_PROVISIONING_PROFILE`), the Azure Artifact Signing values
(`AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`,
`AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_CERT_PROFILE`), and
the R2/update settings:

- `OPENLOGI_UPDATE_BASE_URL` — public HTTPS base URL, for example
  `https://updates.openlogi.org`.
- `OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY` — base64 minisign public key embedded in
  the app and used to verify updater artifacts.
- `OPENLOGI_UPDATE_MINISIGN_SECRET_KEY` — the passwordless minisign secret key
  file, **base64-encoded** (`base64 < minisign.key`), used only in the release
  publish job to sign release artifacts before `latest.json` is generated. It
  is stored base64 (not raw) so its two lines survive 1Password's paste
  handling; the workflow decodes it, mirroring the GitHub App key.
- `OPENLOGI_SPARKLE_PUBLIC_ED_KEY` — Sparkle's base64 EdDSA public key, stamped
  into the macOS bundle as `SUPublicEDKey`.
- `OPENLOGI_SPARKLE_PRIVATE_ED_KEY` — the matching base64 private key (the
  32-byte seed), used only in the publish job to sign the DMGs into the
  appcasts. `release appcast` refuses a private key that does not belong to the
  public key above.
- `CLOUDFLARE_R2_ACCOUNT_ID` — Cloudflare account ID used for the S3 endpoint.
- `CLOUDFLARE_R2_BUCKET` — bucket name.
- `CLOUDFLARE_R2_ACCESS_KEY_ID` — R2 S3 access key.
- `CLOUDFLARE_R2_SECRET_ACCESS_KEY` — R2 S3 secret key.

The workflow uploads immutable files under `/releases/<tag>/`: the artifacts
first, then the release's feed record in `/releases/<tag>/feed/`. The record is
the release's own `latest.json` and one single-item appcast per architecture.
Only the channel files are mutable: `latest.json` and the two appcasts under
`/channels/stable/`, and `latest.json` under `/channels/beta/`. After the
record is up, `cargo xtask release channels` composes them from every record
on R2, and they go up last. Each channel offers its newest release in
Sparkle's order of build versions, whatever order the tags published in.
Items are copied from the records byte for byte, so their signatures are the
ones `release appcast` made. A record's appcasts go up before its
`latest.json`, and a record counts only once its `latest.json` is up, so an
upload that stops part way is skipped rather than blocking later releases.
Records began after `v0.8.17`; until a stable release has one, the current
stable channel files stand in for it, and after that they are not read.
Publish jobs run one at a time (the `release-publish`
concurrency group), so two tags never interleave these writes. Up to 100
waiting publish jobs queue (`queue: max`) instead of replacing each other, and
start in the order they began to wait. The queue only keeps writes apart: it
does not order releases by version, and it does not make a re-run safe. Never
re-run publish for a tag whose release has published, even partly. The release
is immutable, so the re-run fails, after it has already overwritten that tag's
R2 files and composed the channel files again. A missing artifact ships with
the next tag.

Generate the Sparkle key pair once, with the `generate_keys` tool from the
[Sparkle release](https://github.com/sparkle-project/Sparkle/releases) xtask
embeds:

```sh
./bin/generate_keys --account openlogi         # prints the public key
./bin/generate_keys --account openlogi -x sparkle-private.key
# store the file's contents as OPENLOGI_SPARKLE_PRIVATE_ED_KEY, then delete it
```

The key cannot be replaced casually: every installed bundle trusts only the one
it shipped with. Rotating it means shipping a bundle with the new key that is
still signed with the old one, or relying on Sparkle's fallback to the DMG's
Developer ID signature.

After a complete Linux stable release is published, the workflow dispatches
`publish-openlogi-installer` with the tag to `openlogi-org/get`. That repository
downloads `install.sh`, `install.sh.minisig`, and `SHA256SUMS` from the published
GitHub Release, verifies both the exact checksum and minisign signature, and
commits immutable versioned files plus the latest stable aliases served by
Cloudflare Pages at `get.openlogi.org`. It mirrors no native package binaries.
The dispatch uses the same 1Password-backed GitHub App as the Homebrew update;
the App must be installed for the `openlogi-org/get` repository.

The workspace `xtask` helper writes the record into a stage laid out as on
R2. First the manifest:

```sh
cargo run -p xtask -- release latest-json \
  --dist dist \
  --tag v0.2.0 \
  --base-url https://updates.openlogi.org \
  --stage .release/stage
```

then the appcasts, after the DMGs are stapled (stapling rewrites them). The
private key is read only from the environment:

```sh
OPENLOGI_SPARKLE_PRIVATE_ED_KEY=… cargo run -p xtask -- release appcast \
  --dist dist \
  --tag v0.2.0 \
  --build-version 0.2.0 \
  --base-url https://updates.openlogi.org \
  --public-key "$OPENLOGI_SPARKLE_PUBLIC_ED_KEY" \
  --release-notes .release/RELEASE_NOTES.md \
  --stage .release/stage
```

An item's `sparkle:version` is the release's build version (see
[Release versions](#release-versions)), and its `sparkle:shortVersionString`
and title are the tag's version. Sparkle's version comparator stops at the
first `-`, so it would read `1.2.0-beta.3` as `1.2.0`.

The channel files come from a copy of R2's records and channel files, with
this release's record in it:

```sh
cargo run -p xtask -- release channels \
  --tag v0.2.0 \
  --mirror .release/bucket \
  --output-dir .release/channels \
  --base-url https://updates.openlogi.org
```

A stable release's item also offers Sparkle deltas from the three newest
earlier stable releases that embed Sparkle (0.8.14 and later). A beta's item
offers none, so a beta update downloads the full DMG. The macOS build leg
makes the deltas right after it staples the DMG, because Sparkle's
`BinaryDelta` runs only on macOS:

```sh
GH_TOKEN=… cargo run -p xtask -- release sparkle-deltas \
  --dmg dist/OpenLogi-v0.2.0-macos-arm64.dmg \
  --tag v0.2.0
```

It downloads each earlier DMG from the GitHub Release, applies the new delta
to that app, and keeps the delta only when the result matches the new app file
for file. It writes `<dmg>.from-<version>.sparkle.delta` and a `.json` next to
it; `release appcast` reads that metadata and signs the delta. A delta that
cannot be made is a warning, and the feed still offers the full DMG. The deltas
go to `/releases/<tag>/` with the DMGs; their metadata stays out of R2.

Sparkle applies a download as a delta only when its file name ends in
`.delta`. With any other name it cannot unarchive the file and downloads the
full DMG instead. 0.8.16 and 0.8.17 shipped `.sparkle-delta` names, so no
client used those deltas.

### Release versions

Every tag run starts with `cargo xtask release version`. It reads the tag and
checks the workspace version of the tagged commit. A stable tag is on
release-plz's version bump, so the workspace version must be the tag's
version. A beta tags `master` between releases without a version bump, so the
workspace version must be the stable release the beta follows. The command
prints these values for the build:

| Output | Stable `v0.8.17` | Beta `v0.8.17-beta.12` |
| --- | --- | --- |
| `channel` | `stable` | `beta` |
| `version` | `0.8.17` | `0.8.17-beta.12` |
| `build_version` | `0.8.17` | `0.8.16.12` |
| `stamp_version` | empty | `0.8.17-beta.12` |

A beta builds at its own version. Each build leg that compiles OpenLogi
first runs `cargo xtask release stamp-version --version <stamp_version>`
(`windows-msi` compiles nothing and has no such step). It writes
the version into `[workspace.package]`, into the `version` of every path
dependency and `[patch]` entry that points at a workspace crate, and into the
workspace crates in `Cargo.lock`, and commits nothing. A dependency belongs
to the workspace by its `path`, so a registry or git crate with a workspace
crate's name keeps its requirement. The command fails on a requirement it
would not write itself, such as `0.8`, before it writes anything. After
writing, the command requires full `cargo metadata --locked` resolution and
the requested version on every workspace crate. On failure, it tries to
restore every file it may have written and names each file it could not
restore. That run compiles xtask at the old version, but
everything the leg builds after it, xtask included, sees the beta's
`CARGO_PKG_VERSION`: the update engine's current version, the About page,
diagnostics, `CFBundleShortVersionString`, the Linux packages and the
AppStream release. nfpm writes a pre-release as `0.8.17~beta.12` in the deb
and rpm. For the Arch package, `linux package` spells it `0.8.17beta.12`,
because nfpm 2.46.3 leaves a semver pre-release out of the Arch `pkgver`;
it reads `.PKGINFO` back to check. Each sorts below `0.8.17`. The Windows
executables' `FILEVERSION` keeps only the first three fields (`0.8.17.0`). A
stable tag's `stamp_version` is empty, and its legs skip the step.

To try a beta's build before its tag exists, dispatch the Build workflow on
the branch with `stamp_version` set. It builds unsigned and publishes
nothing. It is not quite a tag's build: without a `build_version`, the macOS
bundles' `CFBundleVersion` is the stamped version and the MSI's
`ProductVersion` is `0.0.0`. Sparkle would read `0.8.18-beta.1` as `0.8.18`,
so a signed stamp without a build version fails before anything builds.

Sparkle and Windows Installer order releases by `build_version`. The macOS
bundles carry it as `CFBundleVersion`, and the MSI as its `ProductVersion`. A
beta's build version is the stable release before it plus a fourth field, the
beta number. Sparkle ranks it above the stable release before it and below
the next stable release. Windows Installer ignores the fourth field, so it
treats the beta and that stable release as the same version, and the MSI
allows same-version upgrades.

The stable release before a beta is the newest stable tag reachable from the
beta tag, and the beta's version must be above it. The beta number is the
count of commits from that stable release to the beta
(`git rev-list --count`), so it is at least 1 and grows with `master`.
`release version` fails if the tag's number is a different count.

Only `vX.Y.Z` and `vX.Y.Z-beta.N` are release tags.
`xtask/src/support/release_tag.rs` owns the syntax and the encoding.

The publish side follows the channel. A beta becomes a GitHub prerelease and
never the latest release. It leaves the installer site and `openlogi@latest`
as they are. Publishing recomposes the channel files in build-version order.
The beta channel offers the beta with the highest build version if it is
above the newest stable release's; otherwise it offers that stable release.
The appcasts contain the stable item and any beta item the beta channel
offers, and only apps on `beta` receive the beta item. The stable channel's
`latest.json` keeps its bytes.

The Homebrew tap gets one event per channel. A stable release sends
`update-openlogi` for `openlogi@latest`. A publish that changes what the beta
channel offers sends `update-openlogi-beta` for `openlogi@beta`, with the tag
the beta channel now serves: the new beta, or a stable release newer than
every beta. A tap workflow that listens only for `update-openlogi` ignores
`update-openlogi-beta`, so a beta event cannot change `openlogi@latest`
through it.

A stable release's notes compare with the previous stable tag, so they cover
its betas too. A beta's notes compare with the highest-version release tag
below it that its commit can reach, betas included.
`cargo xtask release version` picks that tag and prints it as
`previous_tag`; publish passes it to GitHub's release-notes API.

### Cutting a beta

Run the Prerelease workflow (`.github/workflows/prerelease.yml`) from
`master` in the Actions tab. It tags the `master` commit the run was
dispatched on, and does not move to a newer commit when `master` advances.
Nobody tags a beta by hand. It finds the open release-plz PR whose head
contains that commit, the way `release-plz.yml` finds it, and stops if there
is none. Then it runs
`cargo xtask release next-beta --release-ref <the PR's head>`. The beta's
version is that PR's workspace version, the next stable release. Its number
is the count of commits since the stable release before it. With the release
PR at 0.8.18 and `master` three commits after `v0.8.17`, the tag is
`v0.8.18-beta.3`, and its build version is `0.8.17.3`.

`release next-beta` refuses to name a beta when:

- the release PR's version is not above the stable release before the beta;
- `master` carries a version that has no stable tag yet;
- the commit is that stable release, or already has a beta tag;
- the tag exists, is not above the newest beta, or the newest beta is not on
  the commit's history;
- the build version does not fit an MSI `ProductVersion`.

`release version` checks the first, second and last of these again on the
pushed tag. They check the tag's version only, so a build, signature or
publish can still fail. The others keep betas rising, one per commit.

`release next-beta` reads the `HEAD` commit and the tags, not the working
tree, so an uncommitted edit cannot change its answer. It needs the whole
history and every `v*` tag, and refuses a shallow repository, because a
shallow history can hide commits from the count. To run it outside the
workflow, run `git fetch --tags` in a full clone first.

The workflow creates the tag with the GitHub App's token, because a tag that
`GITHUB_TOKEN` pushes starts no workflow. The tag push runs `release.yml`. A
dry run prints the tag and creates nothing. Only one cut runs at a time, and
a newer pending run replaces an older pending one.

## Crowdin translation sync

`.github/workflows/crowdin.yml` syncs GUI locales with
[Crowdin](https://crowdin.com/project/openlogi) and opens a `crowdin/i18n` PR
when a **real** translation value improved — nightly, and on master pushes that
touch English sources (`en.toml`), `.config/crowdin.yml`, the Crowdin workflow,
the merge script under `.github/scripts/i18n/`, or the shared GitHub App token
action.

**How it helps translation**

| | Role |
|--|--|
| `en.toml` (git) | English source of truth; stable semantic keys grouped by product-domain tables |
| All `locales/*.toml` in git | Same keys as `en.toml` (parity test); seed Crowdin per language |
| Crowdin project | Where people improve non-English **values** |
| Merge script | Applies only values ≠ English; restores keys sparse exports omit |
| Bot PR (`crowdin/i18n`) | Only when a non-English value actually changed |

Call sites use stable keys such as `device.connected`. Feature PRs add
new keys to **every** locale file in the same change. English wording can change
without renaming the key or updating call sites. Crowdin does not invent
translations; it only stores and syncs them. A raw Crowdin download is unsafe:
untranslated strings come back as English (#549), and
`skip_untranslated_strings` overwrites catalogs with sparse files that delete
keys (#552). The workflow always **snapshots → download → merge** via
`.github/scripts/i18n/merge_crowdin_download.py` so catalogs stay complete and only real
translations land in git.

Each run:

1. Snapshots every `locales/*.toml`.
2. Uploads `en.toml` **sources**.
3. Uploads **per-language translations** already in git (`import_eq_suggestions`
   off so `value == English` is not stored as a finished translation).
4. Downloads Crowdin’s export (`skip_untranslated_strings`; sparse is fine).
5. Merges the export into the snapshot (English fill-in ignored; omitted keys
   kept; headers / `_version` preserved).
6. Opens/updates `crowdin/i18n` only when the working tree still differs.

Like the release workflow, the job reads its credentials through
`AprilNEA/actions/op-environment`, from the `OpenLogi Automation` 1Password
Environment (see [Release secrets](#release-secrets-1password-environments-and-the-release-environment)),
which must contain:

- `CROWDIN_PROJECT_ID` — the numeric Crowdin project id.
- `CROWDIN_PERSONAL_TOKEN` — a Crowdin API token with access to the project.

Grant the token only these scopes and restrict its granular access to the
OpenLogi project:

- Projects (List, Get, Create, Edit) — Read.
- Translation Status — Read Only.
- Source files & strings — Read and Write.
- Translations — Read and Write.

Missing or invalid credentials fail the workflow. Translation PRs run the
normal CI checks, including the locale key parity test (every catalog must match
`en.toml` key-for-key). The workflow uses the GitHub App from the same Environment to
mint a short-lived token for pushing its translation branch and opening the PR;
the default `GITHUB_TOKEN` remains read-only. Checkout runs with
`persist-credentials: false` and the origin remote is rewritten to the app token
so git push does not inherit the read-only Actions credential.

Local helpers (with Crowdin credentials configured):

```sh
devenv tasks run openlogi:i18n-upload    # en.toml sources + per-language translations
devenv tasks run openlogi:i18n-download  # download + merge + i18n tests
python3 .github/scripts/i18n/merge_crowdin_download.py --self-test
```
