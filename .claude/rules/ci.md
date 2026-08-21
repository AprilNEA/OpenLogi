---
paths:
  - ".github/workflows/**"
  - ".github/scripts/ci-local.sh"
  - ".editorconfig"
  - "deny.toml"
  - "rust-toolchain.toml"
  - "prek.toml"
---

# Reproduce CI locally

`.github/workflows/ci.yml` is the source of truth for the PR test pipeline.
This file is the agent-facing map of every job in that workflow to a local
command. Keep them in lockstep: changing a `run:` in `ci.yml` without updating
this file and `.github/scripts/ci-local.sh` is a bug.

`devenv tasks run openlogi:check` is the **host-OS pre-push gate** (fmt, clippy,
tests, rustdoc). It is **not** the pipeline. macOS-green clippy does not compile
linux cfg; it does not run MSRV, cargo-deny, Windows clippy, or the shell lint.

Do not claim a skipped job passed. Name it as not run in the PR Testing section.

## How to run it

```sh
.github/scripts/ci-local.sh                 # every job this host can reproduce
.github/scripts/ci-local.sh --list          # job → command table
.github/scripts/ci-local.sh rustfmt docs    # named jobs (CI `name:` or job id)
direnv exec . .github/scripts/ci-local.sh   # when cargo is only inside devenv
devenv tasks run openlogi:ci                # same as the script
```

The script sets the same env CI does (`RUSTFLAGS=-D warnings`). A rustc warning
that host clippy `-D warnings` does not surface still fails CI.

## Job map (`ci.yml`)

| CI job | Local command | Who can run it |
|---|---|---|
| `rustfmt` | `cargo fmt --all -- --check` | any |
| `shell` | `git ls-files -z \| xargs -0 shfmt -f` piped into `xargs shellcheck` and `xargs shfmt -d` | any (needs `shellcheck` + `shfmt`; both are in the devenv shell) |
| `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` | **Linux** is the CI job. Host clippy on macOS/Windows compiles a different `cfg` |
| `MSRV (cargo check, <os>)` | `RUSTUP_TOOLCHAIN=<rust-version> cargo check --workspace --all-targets` | macOS and Linux. `<rust-version>` is `rust-version` in the root `Cargo.toml` |
| `rustdoc (non-GUI crates)` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items --exclude openlogi-ui --exclude openlogi-desktop --exclude openlogi-overlay --exclude openlogi-agent` | any |
| `tests (linux)` | `cargo test --workspace --exclude openlogi-desktop` | Linux |
| `tests (macos, <arch>)` | `cargo test --workspace --all-targets` | macOS. CI matrix is arm64 (`macos-latest`) and x86_64 (`macos-15-intel`) |
| `cargo-deny` | `cargo deny --all-features --manifest-path crates/openlogi/Cargo.toml check` | any (needs `cargo-deny`; `nix run nixpkgs#cargo-deny -- …` also works) |
| `clippy (windows)` | `cargo clippy --workspace --all-targets -- -D warnings` | **Windows**. Elsewhere: `devenv tasks run openlogi:check-windows` (ring-free subset, not the full workspace) |

CI always sets `CARGO_TERM_COLOR=always`, `CARGO_INCREMENTAL=0`,
`RUSTFLAGS=-D warnings`. There is no Windows test job — only `clippy (windows)`.

### MSRV trap

`rust-toolchain.toml` pins `channel = "stable"`. rustup honours that file over a
toolchain the job installs, so the MSRV job **must** set `RUSTUP_TOOLCHAIN` to
the floor or it silently checks stable. Reproduce it the same way.

### Linux `clippy` / tests from macOS

Host clippy on macOS is not CI's `clippy` job. For linux cfg outside camera:

```sh
cargo clippy --target aarch64-unknown-linux-musl \
  -p openlogi-hook -p openlogi-inject -p openlogi-hid -p openlogi-hidpp \
  -p openlogi-core -p openlogi-agent -p openlogi-agent-core -p openlogi-ipc \
  -p openlogi-permissions --all-targets -- -D warnings
```

`openlogi-camera`'s Linux backend needs kernel headers and does not
cross-compile from macOS. Details: `.claude/rules/cross-platform.md`.

Linux CI tests **exclude** `openlogi-desktop`, so i18n locale-parity tests run
only on macOS CI (`cargo test -p openlogi-desktop i18n`).

## If you changed X, run Y

| Diff | Run |
|---|---|
| anything Rust | `rustfmt`; crate-scoped clippy + tests while iterating; host `clippy` / `tests` before push |
| any `*.sh`, any file with a shell shebang, `.editorconfig` | `shell` (the prek hooks run the same two tools at commit) |
| `#[cfg(target_os = …)]`, hook/inject/hid/camera platform files | `clippy-windows` proxy + the linux-musl recipe; say so if you cannot |
| `Cargo.lock` / `deny.toml` / new deps | `cargo-deny` |
| `rust-version` or a newly stabilized API | `MSRV` |
| rustdoc / moved trait impls / hidpp derive | `rustdoc` |
| `crates/openlogi-ipc/**` or wire types | `cargo test -p openlogi-ipc --test wire_format` |
| `crates/openlogi-ui/locales/**` | `cargo test -p openlogi-desktop i18n` (macOS; Linux CI does not run this) |
| `devenv.nix` / `.envrc` / `devenv.lock` | devenv CI: `nix fmt -- --check devenv.nix` and `devenv --no-tui shell -- true` |
| `flake.nix` / `flake.lock` / `packaging/linux/**` | Nix CI: `nix fmt -- --check flake.nix devenv.nix packaging/linux/package.nix packaging/linux/nixos-module.nix` and `nix flake check --all-systems --no-build --show-trace` |
| `xtask/**` / `packaging/**` | unsigned `cargo xtask` package for that platform; Build workflow is not in `ci-local.sh` |

## Other PR workflows

Not part of `ci.yml`, not in the script's default run:

- **Nix CI** (path-filtered): evaluate + format, then `nix build` the package on
  x86_64-linux and aarch64-linux. Local: the `nix fmt` / `nix flake check`
  commands above; a full `nix build` matches the build job on Linux.
- **devenv CI** (path-filtered): format `devenv.nix` and `devenv --no-tui shell -- true`.
- **Build**: unsigned installers on every PR. Run the matching `cargo xtask`
  package command only when the diff touches packaging.

## When you add a CI job

1. Add a function to `.github/scripts/ci-local.sh` and a row to the table above.
2. If it belongs in the host-OS pre-push gate, also update `openlogi:check` in
   `devenv.nix` and the Local gate in `AGENTS.md`.
