# DRY / low-code replacement audit

This file tracks places where OpenLogi carries general-purpose infrastructure code that can be replaced by mature crates. The goal is not to add dependencies for their own sake, but to delete code where an external crate is a better source of truth.

## Low-risk replacements to do first

- [x] Replace `xtask`'s custom temporary-directory guard with `tempfile`.
- [x] Replace `xtask`'s custom `PATH` scanning with `which`.
- [x] Replace macOS LaunchAgent plist string rendering / XML escaping with `plist`.
- [x] Replace hand-built GUI `file://` URLs with `url::Url::from_file_path`.

## Worth doing next

- [x] Replace `openlogi-assets::http::write_replace` with `atomic-write-file`, preserving atomic replacement and symlink safety.
- [x] Replace recursive asset-cache directory walking with `walkdir`.
- [x] Replace `xtask` command orchestration with `xshell`.
- [x] Replace stale-agent process discovery/signalling through `pgrep`/`kill` with `sysinfo`.
- [x] Replace `xtask` file/path helpers with `fs-err` and `path-absolutize`.
- [x] Replace remaining `xtask` production file operations with `fs-err`.
- [x] Replace `xtask`'s hand-written SHA-256 file loop with `sha2_hasher`.
- [x] Replace `xtask` manifest tests' custom temp directory with `tempfile`.
- [x] Replace macOS iconset shell-out (`sips` + `iconutil`) with `image` + `icns`.
- [x] Replace GUI asset-sync retry delay math with `backon`'s exponential backoff.
- [x] Replace generic GUI/agent `open` shell-outs with `opener`.

## Needs behavior tests before replacing

- [x] Evaluate `etcetera` for XDG-style config/data/runtime paths. Adopted `etcetera::base_strategy::Xdg`, not platform-native macOS paths.
- [x] Evaluate `fluent-langneg` for locale matching, while keeping OpenLogi's shipped-locale policy for Chinese, Portuguese, and Norwegian variants.

## Keep custom for now

- `openlogi-core::single_instance`: `single-instance` uses different backends (for example abstract Unix sockets on Linux) and does not preserve OpenLogi's data-dir lock-file path, per-role names, and error classification closely enough to be a safe deletion.
- Agent tray Quit's `openlogi://quit` dispatch keeps `std::process::Command::output()` intentionally: it blocks until LaunchServices accepts the Apple Event, while generic opener crates only guarantee process spawn.
- GUI helper launch keeps `/usr/bin/open -g -n` intentionally: it needs LaunchServices-specific flags to start the packaged agent under its own TCC identity, which generic opener crates do not expose.
- Agent autostart install keeps direct `systemctl` calls because it is managing systemd user units, not merely opening or spawning an arbitrary program.
- Self-restart and `disclaim` launches stay custom because they are process identity / update lifecycle boundaries, not generic command orchestration.
- `openlogi-hook`: event suppression/rewriting and foreground-app lookup are OpenLogi-specific and not covered cleanly by generic input crates.
- `openlogi-inject`: platform-specific action synthesis may overlap with `enigo`, but current semantics are narrower and more controlled.
- `openlogi-hid` / vendored `openlogi-hidpp`: the right path is upstreaming OpenLogi-specific fixes, not replacing the fork blindly.
