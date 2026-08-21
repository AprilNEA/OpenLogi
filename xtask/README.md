# OpenLogi xtask

`xtask` is the repository-level entry point for development tasks that need Rust
or cross-language orchestration. Run it from the repository root:

```sh
devenv shell -- cargo xtask <command>
# or, without the cargo alias:
devenv shell -- cargo run -p xtask -- <command>
```

## Commands

- `macos icns` — generate `crates/openlogi-desktop/icon/AppIcon.icns` from the master PNG.
- `macos bundle [--channel dev|production]` — build `OpenLogi.app` and embed the
  agent and overlay helpers.
- `macos dev-bundle --binary <path>` — wrap a freshly built desktop binary in
  `target/dev/OpenLogi.app`. Driven by the Cargo runner, not run by hand.
- `macos dmg` — package an existing app bundle into the branded DMG.
- `macos package` — build the app bundle, optionally sign it, then create the branded DMG.
- `linux package` — build release binaries and package `.deb`, `.rpm`, and
  `.pkg.tar.zst` artifacts with nfpm.
- `release changelog` — write the next workspace version's section into
  `CHANGELOG.md` with git-cliff.
- `release latest-json` — generate the static updater manifest for the stable channel.

### Bundle identity

macOS keys TCC grants to a bundle's code identity, and OpenLogi keys its config
profile to the identifier's suffix — so the identity decides whose permission
grants and whose settings a build inherits. Every bundle therefore gets it
written explicitly and read back:

| channel | app | agent helper | overlay helper |
|---|---|---|---|
| `production` | `org.openlogi.openlogi` / OpenLogi | `org.openlogi.agent` / OpenLogi Agent | `org.openlogi.overlay` / OpenLogi Overlay |
| `dev` | the same, suffixed `-dev` / ` Dev` | | |

`macos bundle` defaults to `--channel dev`, so a local build can never claim the
installed app's grants, and signs the result with `OPENLOGI_LOCAL_CODESIGN_IDENTITY`
or the first Apple Development identity it finds (`OPENLOGI_LOCAL_CODESIGN=0`
leaves it unsigned). `macos package` always builds the production channel and
signs with `OPENLOGI_SIGN_IDENTITY` / `--sign-identity`; `macos dmg` refuses to
package anything but a production bundle once it is given a signing identity, so
a dev build cannot reach users. That check is what releases 0.6.24–0.6.26 lacked
when the release workflow shipped `.dev` identifiers.

The raw DMG emitted by `cargo-bundle` is deleted during `macos bundle` because it
is created before xtask embeds and signs the helpers; use `macos package` when
you need a DMG.

### The dev bundle

`macos dev-bundle` assembles `target/dev/OpenLogi.app` around the binary Cargo
just built, so `cargo run -p openlogi-desktop` gets an app name, a Dock icon, an
`openlogi://` registration and a stable signed identity. It reuses everything
`macos bundle` uses — the identity table, the helper table, the `Info.plist`
templates — which is the point: the two were separate implementations until
they drifted far enough that the dev overlay shipped without an icon and the
`-dev` rename had to be made twice.

It also stops the dev agent and overlay an earlier run left behind. Those are
launched through LaunchServices for their own TCC identity, so they are not
children of the GUI and survive both closing its window and Ctrl-C.

`../.cargo/run-macos.sh` stays a shell script — Cargo execs it for every binary
of every `cargo run`/`test`/`bench`, so the passthrough must not pay for an
interpreter start — but it now does nothing except that passthrough and calling
this command. The release-notes generator stays in `../.github/scripts/release-notes` because it is a
dedicated Node tool with Octokit, changelog parsing, and OpenAI dependencies;
xtask should not add a one-line wrapper around a canonical specialized tool.

## Layout

```text
xtask/
  README.md
  src/
    main.rs                  # CLI shape and dispatch only
    commands/
      mod.rs
      macos.rs               # macOS domain entry
      macos/
        bundle.rs
        dmg.rs
      linux.rs               # Linux domain entry
      linux/
        package.rs
      release.rs             # release metadata entry
      release/
        changelog.rs
        latest_json.rs
    support/
      mod.rs
      fs.rs                  # shared filesystem/process guards only
      manifest.rs            # the root Cargo.toml's [workspace.package]
```

Keep command modules aligned with the CLI hierarchy. A platform action belongs
under its platform (`macos bundle`, `linux package`); release metadata belongs
under `release`; shared helpers belong in `support` only when they are reused by
multiple commands or handle real error/resource boundaries.

## Maintenance rules

- Use `xshell` for short-lived external tools such as `cargo`, `create-dmg`,
  `codesign`, and `nfpm`.
- Use `std::process::Command` only when a command needs explicit process
  lifetime, streaming, or stdout/stderr control.
- Use crates for structured data and platform-neutral formats: `serde_json` for
  JSON, `plist` for plist files, `time` for timestamps, hashing crates for
  digests, and `tempfile` for temporary directories.
- Do not shell out just to avoid a small, appropriate Rust dependency.
- Do not reintroduce thin wrappers around tasks already owned by a dedicated
  package script, Cargo subcommand, or external tool.
- Inline single-use helpers unless the name captures a durable domain concept,
  hides meaningful resource handling, or reduces repeated complexity.
