---
paths:
  - "xtask/**"
  - "packaging/**"
  - "scripts/**"
---

# xtask & packaging tooling

- `xtask/README.md` is the contract for this crate — module layout mirrors the CLI
  hierarchy, `xshell` for short-lived external tools (`cargo`, `create-dmg`, `codesign`,
  `nfpm`), `std::process::Command` only for real process control, crates (not shell-outs)
  for structured data, no thin wrappers around tools that already own a task. Read it
  before adding a command.
- xtask is linted like product code: the workspace `clippy::pedantic` +
  `unwrap_used`/`expect_used` warns run with `-D warnings` — use `?` and combinators,
  not `unwrap`/`expect`, even in "script" code.
- App icon: the master is the **committed** `design/icon/openlogi.png` (1024²);
  `cargo xtask macos icns` downscales it via `sips` + `iconutil`. The build never
  fetches the icon from the CDN — a build-time fetch was tried and deliberately
  reverted; don't reintroduce it. After changing the icon, macOS caches by bundle
  path: `touch target/dev/OpenLogi.app && killall Dock` to see it.
- Package contents are declarative, not coded: Linux `.deb`/`.rpm` in
  `packaging/linux/nfpm.yaml` (plus udev rules, systemd unit, desktop entry beside it),
  Windows MSI in `packaging/windows/OpenLogi.wxs`. Packaging env overrides
  (`OPENLOGI_SIGN_IDENTITY`, `OPENLOGI_BUNDLE_ASSETS`, `PKG_ARCH`, …) are documented in
  `docs/DEVELOPMENT.md`.
- `scripts/cargo-run-macos.sh` (the dev-run bundle wrapper) stays outside xtask on
  purpose — cargo must exec it while running arbitrary binaries, including xtask itself.
  `scripts/release-notes/` is a dedicated Node/Octokit tool; don't wrap it in xtask.
