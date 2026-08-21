---
paths:
  - "xtask/**"
  - "packaging/**"
  - ".github/scripts/**"
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
- `cargo xtask ci` is the local CI runner (`xtask/src/commands/ci/`). Facts about a
  job — CI name, the names it answers to, the hosts CI gives it, whether a bare run
  includes it — are one `Spec` row returned from one match in `ci/jobs.rs`; behaviour
  is `ci/jobs/steps.rs`. Host gating is a runtime `Host` value, not `cfg!`, so which
  job skips where is data a test reads. It is also the only place the Windows
  cross-lint crate list lives — `devenv.nix`'s `openlogi:check-windows` calls it
  rather than repeating the `-p` flags. Adding a job to `ci.yml` means a `Job`
  variant + `Spec` row, its steps, and a row in `.claude/rules/ci.md`; `--list`
  renders itself from the rows. It does not repeat the commands — `--dry-run`
  prints the real argv, and a hand-copied third version of what `ci.yml` says
  is a version that can be wrong.
- `.cargo/run-macos.sh` stays a shell script — cargo execs it for every binary of every
  `cargo run`/`test`/`bench`, including xtask's own, so the passthrough must stay cheap —
  but it holds no bundling logic: that is `xtask macos dev-bundle`, which shares the
  identity/helper/plist tables with `macos bundle`. Keep it that way; the two drifted
  badly while they were separate implementations.
  `.github/scripts/release-notes/` is a dedicated Node/Octokit tool; don't wrap it in xtask.
- Shell that is really program logic belongs here instead: `cargo xtask release changelog`
  replaced a script whose version parsing and changelog editing were two embedded Python
  heredocs. The release-plz workflow builds the binary on `master` before checking out
  the release branch, so the tool must keep reading the version from the tree at run
  time — `env!("CARGO_PKG_VERSION")` would bake in the pre-bump one.
