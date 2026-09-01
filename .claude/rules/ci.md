---
paths:
  - ".github/workflows/**"
  - "xtask/src/commands/ci.rs"
  - "xtask/src/commands/ci/**"
  - ".cargo/deny.toml"
  - ".config/typos.toml"
  - ".editorconfig"
  - "rust-toolchain.toml"
  - "prek.toml"
---

# Reproduce CI locally

`.github/workflows/ci.yml` is the source of truth for the PR test pipeline.
This file is the agent-facing map of every job in that workflow to a local
command. Keep them in lockstep: changing a `run:` in `ci.yml` without updating
this file and `cargo xtask ci` is a bug. Two xtask tests catch part of that
drift — `ci_yml_runs_what_this_runner_runs` compares the commands of the jobs
whose invocation does not depend on the host, and `every_ci_yml_job_name_resolves`
fails on a job name the runner cannot even name — but neither can check this
file, so it is on you.

`devenv tasks run openlogi:check` is the **full tier** of the host-OS pre-push
gate (fmt, clippy, tests, rustdoc). `AGENTS.md` defines when a package-local diff
may check its complete reverse-dependency closure instead. Neither tier is the
pipeline. macOS-green clippy does not compile linux cfg; it does not run typos,
MSRV, cargo-deny, Windows clippy, or the shell lint.

Do not claim a skipped job passed. Name it as not run in the PR Testing section.

## How to run it

```sh
cargo xtask ci                    # every job this host can reproduce
cargo xtask ci --list             # job → command table
cargo xtask ci rustfmt docs       # named jobs (CI `name:` or job id)
cargo xtask ci --dry-run          # print each job's commands, run nothing
direnv exec . cargo xtask ci      # when cargo is only inside devenv
devenv tasks run openlogi:ci      # same as the command
```

The runner sets CI's semantic compiler env (`RUSTFLAGS=-D warnings`). CI also
sets `CARGO_INCREMENTAL=0` and wraps rustc with sccache; those only change how
compiler outputs are produced and reused, not what the jobs validate. A rustc
warning that host clippy `-D warnings` does not surface still fails CI.

## Job map (`ci.yml`)

| CI job | Local command | Who can run it |
|---|---|---|
| `rustfmt` | `cargo fmt --all -- --check` | any |
| `typos` | `typos --config .config/typos.toml .` | any (needs `typos`; included in the devenv shell) |
| `publish closure` | `cargo xtask release check-publish` | any |
| `shell` | `git ls-files -z \| xargs -0 shfmt -f` piped into `xargs shellcheck` and `xargs shfmt -d` | any (needs `shellcheck` + `shfmt`; both are in the devenv shell) |
| `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` | **Linux** is the CI job. Host clippy on macOS/Windows compiles a different `cfg` |
| `MSRV (cargo check, <os>)` | `RUSTUP_TOOLCHAIN=<rust-version> cargo check --workspace --all-targets` | macOS and Linux. `<rust-version>` is `rust-version` in the root `Cargo.toml` |
| `rustdoc (non-GUI crates)` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items --exclude openlogi-ui --exclude openlogi-desktop --exclude openlogi-overlay --exclude openlogi-agent` | any |
| `tests (linux)` | `cargo test --workspace --exclude openlogi-desktop` | Linux |
| `tests (macos, <arch>)` | `cargo test --workspace --all-targets` | macOS. CI matrix is arm64 (`macos-latest`) and x86_64 (`macos-15-intel`) |
| `cargo-deny` | `cargo deny --config .cargo/deny.toml --all-features --manifest-path crates/openlogi/Cargo.toml check` | any (needs `cargo-deny`; `nix run nixpkgs#cargo-deny -- …` also works) |
| `clippy (windows)` | `cargo clippy --workspace --all-targets -- -D warnings` | **Windows**. Elsewhere: `devenv tasks run openlogi:check-windows` (ring-free subset, not the full workspace) |
| `wasm (portable crates)` | `cargo check -p openlogi-hidpp -p openlogi-device --target wasm32-unknown-unknown` then `cargo check -p openlogi-core --no-default-features --target wasm32-unknown-unknown` | any (needs the `wasm32-unknown-unknown` std; devenv installs it) |

CI always sets `CARGO_TERM_COLOR=always`, `CARGO_INCREMENTAL=0`, and
`RUSTFLAGS=-D warnings`. Compilation jobs also set `RUSTC_WRAPPER=sccache`;
`cargo-deny` clears it because metadata probes rustc without compiling and the
job deliberately skips sccache setup. `rust-cache` stores only Cargo registry/git
inputs (`cache-targets: false`); sccache owns compiler outputs. PRs read the
default branch's sccache objects but do not write their isolated merge-ref cache.
There is no Windows test job — only `clippy (windows)`.

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

Linux CI tests **exclude** `openlogi-desktop`, but still run `openlogi-ui`'s
portable locale-parity test. The desktop end-to-end key-resolution tests run
only on macOS CI (`cargo test -p openlogi-desktop i18n`).

## If you changed X, run Y

| Diff | Run |
|---|---|
| anything Rust | the local-gate tier selected in `AGENTS.md`; the pre-push hook always runs full-workspace Clippy and non-GUI rustdoc |
| crate publish flags, workspace path dependencies, `release-plz.toml` | `publish-closure` |
| any `*.sh`, any file with a shell shebang, `.editorconfig` | `shell` (the prek hooks run the same two tools at commit) |
| `#[cfg(target_os = …)]`, hook/inject/hid/camera platform files | `clippy-windows` proxy + the linux-musl recipe; say so if you cannot |
| `crates/openlogi-hidpp/**`, `crates/openlogi-device/**`, `crates/openlogi-core/**`, or any dependency they gain | `wasm` — those crates must keep building with no OS under them |
| `Cargo.lock` / `.cargo/deny.toml` / new deps | `cargo-deny` |
| `rust-version` or a newly stabilized API | `MSRV` |
| rustdoc / moved trait impls / hidpp derive | `rustdoc` |
| `crates/openlogi-ipc/**` or wire types | `cargo test -p openlogi-ipc --test wire_format` |
| `crates/openlogi-ui/locales/**` | `cargo test -p openlogi-ui locale`; also `cargo test -p openlogi-desktop i18n` when binary wiring or desktop resolution changed |
| `devenv.nix` / `.envrc` / `devenv.lock` | devenv CI: `nix fmt -- --check devenv.nix` and `devenv --no-tui shell -- true` |
| `flake.nix` / `flake.lock` / `packaging/linux/**` | Nix CI: `nix fmt -- --check flake.nix devenv.nix packaging/linux/package.nix packaging/linux/nixos-module.nix` and `nix flake check --all-systems --no-build --show-trace` |
| `xtask/**` / `packaging/**` | unsigned `cargo xtask` package for that platform; the Build workflow is not part of `cargo xtask ci` |

## Other PR workflows

Not part of `ci.yml`, not in the default run:

- **Nix CI** (path-filtered): evaluate + format, then `nix build` the package on
  x86_64-linux and aarch64-linux. Local: the `nix fmt` / `nix flake check`
  commands above; a full `nix build` matches the build job on Linux.
- **devenv CI** (path-filtered): format `devenv.nix` and `devenv --no-tui shell -- true`.
- **Build**: unsigned installers on every PR. Run the matching `cargo xtask`
  package command only when the diff touches packaging.
- **Build / Linux Flatpak**: gated behind the `needs: build` label like the rest
  of that matrix, so a PR touching `packaging/linux/flatpak/**` gets no feedback
  until someone labels it. Reproduce locally with
  `flatpak-builder --user --disable-rofiles-fuse --force-clean --repo=repo
  --default-branch=stable build packaging/linux/flatpak/org.openlogi.OpenLogi.yml`,
  and keep the build tree off `/tmp` if it is a tmpfs: the workspace needs well
  over 10 GB to compile inside the sandbox.

## When you add a CI job

1. Add a `Job` variant plus its `Spec` row (name, aliases, hosts, caveat) in
   `xtask/src/commands/ci/jobs.rs`, its steps in `ci/jobs/steps.rs`, and a row
   in the table above. `--list` renders itself from the `Spec` rows, so it
   needs no edit; `every_ci_yml_job_name_resolves` fails until the `Spec` row
   answers to the workflow's `name:`. The `Spec` row is also what decides the
   host skip — do not reach for `cfg!(target_os = …)` in a job's steps.
   If the new job's command is the same everywhere, add it to
   `ci_yml_runs_what_this_runner_runs` so a typo in either copy fails a test.
2. If it belongs in the host-OS pre-push gate, also update `openlogi:check` in
   `devenv.nix` and the Local gate in `AGENTS.md`.
