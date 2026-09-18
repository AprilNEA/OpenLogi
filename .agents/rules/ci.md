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
  - ".ast-grep/**"
  - "sgconfig.yml"
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
gate (fmt, clippy, tests, rustdoc). The local gate below defines when a package-local
diff may check its complete reverse-dependency closure instead. Neither tier is the
pipeline. macOS-green clippy does not compile linux cfg; it does not run typos,
ast-grep, MSRV, cargo-deny, Windows clippy, or the shell lint.

Do not claim a skipped job passed. Name it as not run in the PR Testing section.

## Local gate (hard stop before push — scale it to the affected graph)

The local gate keeps predictable failures off a PR update; CI then sweeps the
whole workspace on its other hosts. CI does not run for an ordinary branch push
that has no open PR, so never use it as a substitute for the local tier.

A **Rust-bearing diff** changes Rust source or an input that controls how the Rust
workspace builds or is validated. A truly non-Rust diff does not need Rust commands
merely because it is being pushed. Run the applicable checks from
[If you changed X, run Y](#if-you-changed-x-run-y) instead (shell, Nix, packaging, and so on).

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
`.agents/rules/rust.md`.

prek hooks (`prek.toml`): typos, `cargo fmt`, and the ast-grep guards at commit;
full-workspace clippy
**and rustdoc** at push (rust-scoped, so non-Rust pushes skip it). Hooks are a
backstop, not a substitute for running the gate yourself after a rebase.

### Push checklist (agents)

1. Rebase/merge conflicts fully resolved — no `<<<<<<<` left, no half-ported APIs.
2. Applicable local gate green on the **final** tree: non-Rust checks for a
   non-Rust diff; affected-package tier when none of the full-tier triggers above
   applies; full otherwise.
3. Additional pipeline jobs required by the diff run by name with
   `cargo xtask ci <job>…`. Skipped jobs stay named as not run — never claimed
   green. Mapping: [If you changed X, run Y](#if-you-changed-x-run-y).
4. If cfg-gated files changed (any `#[cfg(target_os = …)]` block, in any crate):
   cross-lint or hand-audit against master — macOS-green proves nothing there; see
   `.agents/rules/cross-platform.md`.
5. If wire types changed: `PROTOCOL_VERSION` bumped and
   `cargo test -p openlogi-ipc --test wire_format` green — see
   `crates/openlogi-ipc/AGENTS.md`.
6. If locales changed: every `crates/openlogi-ui/locales/*.toml` carries the same keys
   as `en.toml` (new keys at the same position); run `cargo test -p openlogi-ui locale`
   for catalog parity and `cargo test -p openlogi-desktop i18n` for catalog wiring and
   desktop resolution — see `.agents/rules/i18n.md`.
7. Only then `git push` / force-push to the PR branch.

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
| `ast-grep` | `ast-grep scan` | any (needs `ast-grep`; included in the devenv shell). The single-source-of-truth guards in `.ast-grep/rules/`, configured by `sgconfig.yml` |
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
cross-compile from macOS. Details: `.agents/rules/cross-platform.md`.

Linux CI tests **exclude** `openlogi-desktop`, but still run `openlogi-ui`'s
portable locale-parity test. The desktop end-to-end key-resolution tests run
only on macOS CI (`cargo test -p openlogi-desktop i18n`).

## If you changed X, run Y

| Diff | Run |
|---|---|
| anything Rust | the local-gate tier above; the pre-push hook always runs full-workspace Clippy and non-GUI rustdoc; `ast-grep` (the prek hook runs it over the staged files at commit) |
| `.ast-grep/**`, `sgconfig.yml` | `ast-grep` — a new rule must flag nothing on the current tree and must flag the copy it was written against (check out the commit before the consolidation) |
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
   `devenv.nix` and the Local gate above.
