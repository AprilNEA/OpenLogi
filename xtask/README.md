# OpenLogi xtask

`xtask` is the repository-level entry point for development tasks that need Rust
or cross-language orchestration. Run it from the repository root:

```sh
devenv shell -- cargo xtask <command>
# or, without the cargo alias:
devenv shell -- cargo run -p xtask -- <command>
```

## Commands

- `ci [--list] [--dry-run] [JOB…]` — reproduce the `ci.yml` jobs this host can
  run; a job it cannot is skipped with a reason, never passed.
- `macos icon` — compile `design/icon/openlogi.icon` into the `AppIcon.icns` and
  `Assets.car` under `crates/openlogi-desktop/icon/`.
- `macos bundle [--channel dev|production]` — build `OpenLogi.app` and embed the
  agent and overlay helpers.
- `macos dev-bundle --binary <path>` — wrap a freshly built desktop binary in
  `target/dev/OpenLogi.app`. Driven by the Cargo runner, not run by hand.
- `macos dmg` — package an existing app bundle into the branded DMG.
- `macos package` — build the app bundle, embed Sparkle when given its public key,
  optionally sign it, then create the branded DMG.
- `linux package` — build release binaries, package `.deb`, `.rpm`, and
  `.pkg.tar.zst` artifacts with nfpm, then re-encode the `.deb` data member as
  xz and the Arch package as zstd with a long window.
- `lint sweep-allows [--out <path>]` — remove each `#[allow]` in turn, run
  clippy, and record which of its lints fire on this host.
- `lint merge-allows <reports…>` — join the per-host reports and fail on a lint
  that fires on none of them. `stale-allows.yml` runs both weekly.
- `release changelog` — write the next workspace version's section into
  `CHANGELOG.md` with git-cliff.
- `release check-publish` — verify that every crates.io package has a publishable,
  versioned workspace dependency closure.
- `release checkout-version-bump` — pin a release job to the commit that
  introduced the current workspace version.
- `release latest-json` — write a release's static updater manifest into its
  feed record.
- `release appcast` — sign the macOS DMGs and their Sparkle deltas with
  Sparkle's EdDSA key and write one single-item appcast per architecture into
  the release's feed record.
- `release channels` — compose every channel's `latest.json` and appcasts
  from the release records on the update host, and print what each channel
  offers as GitHub Actions outputs.
- `release next-beta --release-ref <commit>` — name the beta tag to cut on
  `HEAD`: the version that `<commit>`, the open release PR, prepares, and the
  count of commits since the stable release before it, checked as
  `release version` checks the pushed tag. It reads committed files, not the
  working tree, and needs the whole history and every `v*` tag; it refuses a
  shallow repository. The prerelease workflow runs it and creates the tag.
- `release sparkle-deltas` — make verified Sparkle deltas from the newest
  earlier stable releases to a stable release's stapled DMG, with the metadata
  `release appcast` reads. A beta gets none.
- `release stamp-version` — write a version into the workspace manifests and
  the workspace crates in `Cargo.lock` before a build, uncommitted, so a beta
  builds at its own version.
- `release version` — print a release tag's channel, version, build version,
  the version its build stamps (empty for a stable tag) and the previous tag
  its release notes compare with as GitHub Actions outputs, after checking the
  tagged commit's workspace version: the tag's for a stable tag, and the
  stable release it follows for a beta.

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

`macos package --agent-provisioning-profile` (`OPENLOGI_AGENT_PROVISIONING_PROFILE`)
takes the agent's Developer ID provisioning profile, which grants
`com.apple.developer.hid.virtual.device` to `org.openlogi.agent`. It needs
`--sign-identity`. `bundle/provisioning.rs` checks the profile before the
build, embeds it as the agent's `Contents/embedded.provisionprofile`, signs the
agent with exactly the entitlements the virtual HID keyboard needs, and then
checks that the profile includes the signing certificate. Without a profile,
the agent claims no entitlements: AMFI kills a process that claims a
restricted entitlement without its profile. The release workflow reads the
profile from 1Password (`APPLE_AGENT_PROVISIONING_PROFILE`, base64).

The profile lists the Developer ID Application certificate that signs the
release. `provisioning.rs` refuses a profile that has expired or that does not
list the signing certificate, so a stale profile fails the build. The current
profile expires on 2044-10-15, and its certificate expires on 2031-01-20. When
either is renewed:

1. In the Apple Developer portal, create a Developer ID profile for the
   `org.openlogi.agent` App ID with the current Developer ID Application
   certificate. The App ID must keep the HID Virtual Device capability.
2. Put the profile's base64 in the `APPLE_AGENT_PROVISIONING_PROFILE` variable
   of the `OpenLogi Release` 1Password Environment (`docs/DEVELOPMENT.md`,
   "Release secrets").
3. If the certificate was renewed, update the same Environment too, because CI
   imports the certificate separately: `APPLE_CERTIFICATE` (the base64 of the
   new certificate and private key, exported as PKCS#12),
   `APPLE_CERTIFICATE_PASSWORD`, and `APPLE_SIGNING_IDENTITY` if the identity
   name changed.
4. Before the next release, run a signed build on a branch
   (`gh workflow run build.yml --ref <branch> -f sign=true`) and check that its
   macOS jobs pass.

`macos bundle` asks `cargo-bundle` for the base `.app` only, then embeds and signs
the helpers. Use `macos package` when you need the final DMG. The package command
also accepts `--target aarch64-apple-darwin` or `--target x86_64-apple-darwin` so
CI can cross-compile either distribution architecture.

### Sparkle

Given `OPENLOGI_SPARKLE_PUBLIC_ED_KEY` (or `--sparkle-public-key`), `macos package`
embeds the pinned Sparkle release (`bundle/sparkle.rs`: version and archive
SHA-256, cached under `target/sparkle/`) as `Contents/Frameworks/Sparkle.framework`,
drops its XPC services (OpenLogi is not sandboxed), and stamps the `SU*` keys
into `Info.plist`. The feed URL comes from `support/update_feed.rs`: one appcast
per architecture under `OPENLOGI_UPDATE_BASE_URL`, because each architecture
ships its own DMG. `release appcast` and `release channels` write those feeds
from the same module, so the URL a bundle reads and the file a release
uploads cannot drift apart. The module also owns where a release's files and
feed record live on the update host.
Sparkle orders releases by `CFBundleVersion`. When supplied, `--build-version`
(from `release version`) sets `CFBundleVersion`; otherwise it uses the crate
version. `CFBundleShortVersionString` always uses the crate version. A stable
release's build version is its version; `support/release_tag.rs` owns the
encoding.
Signing re-signs Sparkle's `Autoupdate`, `Updater.app` and the framework, inside
out, before the app seals them. Without the key the bundle has no Sparkle and
the app updates through its built-in engine; PR builds and unsigned
dispatches are built that way.

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
this command.

## Layout

```text
xtask/
  README.md
  src/
    main.rs                  # CLI shape and dispatch only
    commands/
      mod.rs
      ci.rs                  # CI job runner: CLI, host, step execution, summary
      ci/
        jobs.rs              # one row of facts per ci.yml job + host gating
        jobs/
          steps.rs           # what each job runs
          tests.rs
        list.rs              # renders --list from those rows (comfy-table)
        list/
          tests.rs
      macos.rs               # macOS domain entry
      macos/
        bundle.rs              # the assembly order, and what it means
        bundle/
          embed.rs             # login-item helpers, the CLI, required binaries
          embed/tests.rs
          identity.rs          # bundle ids, names, icons per channel
          identity/tests.rs
          provisioning.rs      # the agent's profile and the HID entitlement it backs
          provisioning/tests.rs
          signing.rs           # codesign, inside out
          signing/tests.rs
          sparkle.rs           # Sparkle.framework: fetch, embed, Info.plist keys
          sparkle/tests.rs
        dev_bundle.rs
        dev_bundle/
          processes.rs
          signing.rs
          tests.rs
        dmg.rs
      linux.rs               # Linux domain entry
      linux/
        package.rs
        package/
          payload.rs         # re-encoding the .deb and Arch payloads nfpm writes
          payload/tests.rs
          tests.rs
      lint.rs                # lint hygiene one clippy run cannot check
      lint/
        allow_sites.rs       # where each `#[allow]` is, and the edit removing it
        allow_sites/tests.rs
        report.rs            # per-host reports and the cross-host verdict
        report/tests.rs
        sweep.rs             # one clippy run per `allow`, counted by lint code
        sweep/tests.rs
      release.rs             # release metadata entry
      release/
        appcast.rs           # a release's per-architecture Sparkle items, EdDSA-signed
        appcast/tests.rs
        changelog.rs
        changelog/tests.rs
        channels.rs          # channel files composed from the release records
        channels/tests.rs
        checkout_version_bump.rs
        checkout_version_bump/tests.rs
        latest_json.rs
        latest_json/tests.rs
        next_beta.rs         # the beta tag the prerelease workflow cuts
        next_beta/tests.rs
        sparkle_deltas.rs
        sparkle_deltas/tests.rs
        stamp_version.rs     # a beta's version, written into the workspace
        stamp_version/tests.rs
        version.rs           # a release tag's channel, version and build version
    icon.rs                  # the icon set, and the pipeline a platform implements
    icon/
      macos.rs               # Icon Composer documents -> icns + asset catalog
      macos/tests.rs
    support/
      mod.rs
      fs.rs                  # shared filesystem/process guards only
      github_output.rs       # GitHub Actions step outputs
      info_plist.rs          # reading and stamping plist keys
      manifest.rs            # the root Cargo.toml's [workspace.package]
      release_tag.rs         # release tag syntax, channel and build version
      release_tag/tests.rs
      update_feed.rs         # update host layout and feed URLs packaging and release share
      update_feed/tests.rs
```

Unit tests are a sibling file throughout this crate: `foo.rs` declares
`#[cfg(test)] mod tests;` and the tests live in `foo/tests.rs`. That keeps a
module's source to what it does, and the `#[cfg(test)]` on the declaration is
what carries the Clippy unwrap/expect test exemption into the file — a test
helper outside any `#[test]` fn still gets it. Note that `include_str!` in such
a file resolves relative to `foo/`, one level deeper than the module it came
from.

A test that reads a file from the repository has one more constraint: the Nix
package builds from a source derivation that deliberately omits documentation
and CI metadata — editing a workflow must not rebuild the application — and it
runs `cargo test` inside that sandbox. Either add the file to the fileset in
`packaging/linux/package.nix`, the way `nfpm.yaml` is there for the packaged-bins
test, or let the test skip when the file's whole directory is absent, the way
the `ci.yml` drift tests do.

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
