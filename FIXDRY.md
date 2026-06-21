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
- [ ] Consider `xshell` / `duct` for `xtask` command orchestration (`run`, `command_stdout`, display/quoting).

## Needs behavior tests before replacing

- [ ] Evaluate `etcetera` for XDG-style config/data/runtime paths. Do not switch to platform-native macOS paths; OpenLogi intentionally uses XDG on every platform.
- [ ] Evaluate `single-instance` as a replacement for the small `fs4`-based single-instance wrapper only if it preserves per-role lock names and error semantics.
- [ ] Evaluate `unic-langid` + `fluent-langneg` for locale matching, while keeping OpenLogi's shipped-locale policy for Chinese, Portuguese, and Norwegian variants.

## Keep custom for now

- `openlogi-hook`: event suppression/rewriting and foreground-app lookup are OpenLogi-specific and not covered cleanly by generic input crates.
- `openlogi-inject`: platform-specific action synthesis may overlap with `enigo`, but current semantics are narrower and more controlled.
- `openlogi-hid` / vendored `openlogi-hidpp`: the right path is upstreaming OpenLogi-specific fixes, not replacing the fork blindly.
