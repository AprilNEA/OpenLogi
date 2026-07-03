# OpenLogi xtask

`xtask` is the repository-level entry point for development tasks that need Rust
or cross-language orchestration. Run it from the repository root:

```sh
devenv shell -- cargo xtask <command>
# or, without the cargo alias:
devenv shell -- cargo run -p xtask -- <command>
```

## Commands

- `macos icns` — generate `crates/openlogi-gui/icon/AppIcon.icns` from the master PNG.
- `macos bundle` — build the release `OpenLogi.app` and embed the agent helper.
- `macos dmg` — package an existing app bundle into the branded DMG.
- `macos package` — build the app bundle, optionally sign it, then create the branded DMG.
- `linux package` — build release binaries and package `.deb` / `.rpm` artifacts with nfpm.
- `release latest-json` — generate the static updater manifest for the stable channel.

The Cargo runner in `../scripts/cargo-run-macos.sh` stays outside xtask because
Cargo must execute it while running arbitrary binaries, including this crate.
The release-notes generator stays in `../scripts/release-notes` because it is a
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
        latest_json.rs
    support/
      mod.rs
      fs.rs                  # shared filesystem/process guards only
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
