# Developing OpenLogi

This document covers the local development workflow for OpenLogi. For end-user
build instructions, see the [README](../README.md).

## Toolchain

- Stable Rust (Edition 2024, MSRV 1.96)
- macOS: Xcode 16+ with the optional **Metal Toolchain** component (required by
  GPUI's `gpui_macos` build script to compile shaders)
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
# macOS: full Xcode 16+ with the Metal Toolchain (not only Command Line Tools)
# Linux: see system libraries under Toolchain above
# optional helpers: brew install cmake create-dmg sccache
git clone https://github.com/AprilNEA/OpenLogi
cd OpenLogi
cargo run -p openlogi --release -- list
cargo run -p openlogi-gui --release
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
devenv tasks run openlogi:check    # fmt + clippy + tests (run before committing)
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

The root Flake exposes native `x86_64-linux` and `aarch64-linux` packages plus
the NixOS module. It is separate from the devenv shell:

```sh
nix flake check --all-systems --no-build  # evaluate every output
nix build .#openlogi                      # build + test this host's package
nix run .#openlogi -- list                # run the packaged CLI
```

The package expression and NixOS module live beside the other Linux packaging
inputs in `packaging/linux/`. `nix fmt` formats all Nix expressions through the
Flake's pinned formatter.

### Dev app bundle (macOS)

On macOS the desktop binary is launched from inside a throwaway
`target/dev/OpenLogi.app` — a Cargo `runner` wired in `.cargo/config.toml`
(`scripts/cargo-run-macos.sh`). This makes the dev build show as
**OpenLogi Dev** in the menu bar and Dock, with the real app icon; a bare
`cargo run` binary has no bundle, so macOS would otherwise fall back to the
`openlogi-gui` executable name and a generic icon. The binary is hardlinked in
(no copy) and the icon is generated on demand by
`cargo run -p xtask -- macos icns`. The runner is a transparent passthrough for
everything else (the CLI, tests); set
`OPENLOGI_DEV_BUNDLE=0` to launch the raw `openlogi-gui` binary instead.

Packaged local dev bundles (`cargo run` and
`cargo run -p xtask -- macos bundle`) use `.dev` bundle identifiers and the
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
cargo install --path .
```

## Developing the GUI without hardware

`openlogi-agent-mock` serves the real agent IPC contract from a scripted
in-memory inventory, so the desktop app can be developed with no Logitech
device (or receiver) attached:

```sh
cargo run -p openlogi-agent --bin openlogi-agent-mock   # then, in another terminal:
OPENLOGI_DEV_AGENT=0 cargo run -p openlogi-gui
```

The mock defaults itself to the `openlogi-dev` profile (as if `OPENLOGI_PROFILE=dev`
were set), which is the profile the dev app bundle already uses — so it meets the
dev GUI on the dev socket, and an installed *release* build, which is on the
production profile, keeps running untouched. (A locally built bundle installed
into `/Applications` carries `.dev` identifiers and therefore shares the dev
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

## Project layout

```
src/                the `openlogi` binary (workspace root package) — a thin wrapper over openlogi-cli
crates/
  openlogi-core/    types, config (TOML), paths, button + action catalog — no HID, no async
  openlogi-inject/  OS input synthesis: CGEvent, uinput/MPRIS, and SendInput
  openlogi-hidpp/   vendored HID++ protocol crate (lib name `hidpp`)
  openlogi-hid/     device discovery, HID++ reads/writes, and control capture over async-hid
  openlogi-assets/  device-render registry schema + cached HTTP fetch from OpenLogi asset mirrors
  openlogi-cli/     CLI implementation: command tree + `run()`, called by the `openlogi` binary
  openlogi-agent-core/  shared orchestration + the agent/GUI IPC contract
  openlogi-agent/   the `openlogi-agent` binary — background agent owning device I/O and the hook
  openlogi-hook/    OS mouse hook: macOS CGEventTap, Linux evdev/uinput, Windows WH_MOUSE_LL
  openlogi-gui/     the `openlogi-gui` binary — GPUI + gpui-component IPC client
```

## Pre-commit checklist

Before committing, the following must pass:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Equivalent to `devenv tasks run openlogi:check`.

## Packaging the macOS DMG

```sh
cargo run -p xtask -- macos package    # → target/release/OpenLogi.dmg
```

Environment overrides:

- `OPENLOGI_BUNDLE_ASSETS=1` — bundle every device render into the `.app` for a
  fully offline build (default: fetched on demand at first launch).
- `OPENLOGI_SIGN_IDENTITY=<identity>` — codesign the `.app` and `.dmg` with the
  given Developer ID.
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
# → target/release/openlogi_*.deb / .rpm / .pkg.tar.zst
```

The package contents (binaries, udev rules, systemd user unit, desktop entry,
icon) are declared in `packaging/linux/nfpm.yaml`.

The Nix package uses the same shared resources and is declared in
`packaging/linux/package.nix`; see the Nix package section above for its build
commands.

## Release updater publishing

Tagged releases still attach DMGs and `SHA256SUMS` to GitHub Releases for manual
downloads and the Homebrew cask. The release workflow also publishes the same
DMGs to Cloudflare R2 and writes a static updater manifest at:

```text
${OPENLOGI_UPDATE_BASE_URL}/channels/stable/latest.json
```

The app embeds that manifest URL at build time via
`OPENLOGI_UPDATE_MANIFEST_URL`, derived from `OPENLOGI_UPDATE_BASE_URL` in the
release workflow. Release builds also embed `OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY`
and run with `Verification::Strict`: an update is installed only if the manifest
asset carries a minisign signature that verifies against that key, plus a
matching SHA-256. A build without the key embedded (local/dev) fails closed —
the update check errors rather than installing an unverified artifact.

Configure the R2/update settings in one 1Password item referenced by the GitHub
secret `OP_R2_SECRET_ITEM`. The item must contain:

- `OPENLOGI_UPDATE_BASE_URL` — public HTTPS base URL, for example
  `https://updates.openlogi.org`.
- `OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY` — base64 minisign public key embedded in
  the app and used to verify updater artifacts.
- `OPENLOGI_UPDATE_MINISIGN_SECRET_KEY` — the passwordless minisign secret key
  file, **base64-encoded** (`base64 < minisign.key`), used only in the release
  publish job to sign DMGs before `latest.json` is generated. It is stored
  base64 (not raw) so its two lines survive 1Password's paste handling; the
  workflow decodes it, mirroring the GitHub App key.
- `CLOUDFLARE_R2_ACCOUNT_ID` — Cloudflare account ID used for the S3 endpoint.
- `CLOUDFLARE_R2_BUCKET` — bucket name.
- `CLOUDFLARE_R2_ACCESS_KEY_ID` — R2 S3 access key.
- `CLOUDFLARE_R2_SECRET_ACCESS_KEY` — R2 S3 secret key.

The workflow uploads immutable artifacts under `/releases/<tag>/` and only the
channel manifest under `/channels/stable/latest.json` is mutable.

The manifest is generated by the workspace `xtask` helper:

```sh
cargo run -p xtask -- release latest-json \
  --dist dist \
  --tag v0.2.0 \
  --base-url https://updates.openlogi.org \
  --output dist/latest.json
```

## Crowdin translation sync

`.github/workflows/crowdin.yml` syncs GUI locales with
[Crowdin](https://crowdin.com/project/openlogi) and opens a `crowdin/i18n` PR
when a **real** translation value improved — nightly, and on master pushes that
touch English sources (`en.yml`), `crowdin.yml`, the Crowdin workflow, the merge
script under `scripts/i18n/`, or the shared GitHub App token action.

**How it helps translation**

| | Role |
|--|--|
| `en.yml` (git) | English source of truth — English text is the key |
| All `locales/*.yml` in git | Same keys as `en.yml` (parity test); seed Crowdin per language |
| Crowdin project | Where people improve non-English **values** |
| Merge script | Applies only values ≠ English; restores keys sparse exports omit |
| Bot PR (`crowdin/i18n`) | Only when a non-English value actually changed |

Feature PRs add new keys to **every** locale file in the same change. Crowdin
does not invent translations; it only stores and syncs them. A raw Crowdin
download is unsafe: untranslated strings come back as English (#549), and
`skip_untranslated_strings` overwrites catalogs with sparse files that delete
keys (#552). The workflow always **snapshots → download → merge** via
`scripts/i18n/merge_crowdin_download.py` so catalogs stay complete and only real
translations land in git.

Each run:

1. Snapshots every `locales/*.yml`.
2. Uploads `en.yml` **sources**.
3. Uploads **per-language translations** already in git (`import_eq_suggestions`
   off so `value == English` is not stored as a finished translation).
4. Downloads Crowdin’s export (`skip_untranslated_strings`; sparse is fine).
5. Merges the export into the snapshot (English fill-in ignored; omitted keys
   kept; headers / `_version` preserved).
6. Opens/updates `crowdin/i18n` only when the working tree still differs.

Like the release workflow, the job reads its credentials from one 1Password
item referenced by the GitHub secret `OP_CROWDIN_SECRET_ITEM`. The item must
contain:

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
`en.yml` key-for-key). The workflow uses the existing `OP_GITHUB_APP_ITEM` to
mint a short-lived token for pushing its translation branch and opening the PR;
the default `GITHUB_TOKEN` remains read-only. Checkout runs with
`persist-credentials: false` and the origin remote is rewritten to the app token
so git push does not inherit the read-only Actions credential.

Local helpers (with Crowdin credentials configured):

```sh
devenv tasks run openlogi:i18n-upload    # en.yml sources + per-language translations
devenv tasks run openlogi:i18n-download  # download + merge + i18n tests
python3 scripts/i18n/merge_crowdin_download.py --self-test
```
