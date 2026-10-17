# openlogi-hidpp — HID++ protocol, hard fork

This crate started as a vendored copy of the 0BSD `hidpp` crate
(<https://github.com/lus/logy>) but **is a hard fork, not a tracked vendor copy**.
Upstream is provenance, not a merge target: no change here needs to preserve
upstream diffability, and nothing has to be re-derivable from a future upstream
release. Restructure freely, add dependencies, add derive macros, rename types,
resplit modules across files. "Upstream does it this way" is **not** an argument
in review — judge changes against this crate's own contract and the workspace
house style in the root `AGENTS.md`, not against `lus/logy`'s source.

## What the fork status does NOT license

Being a hard fork changes how freely the *code* can diverge. It changes nothing
about licensing or attribution:

- The `LICENSE` file (0BSD), the `license = "0BSD"` field in `Cargo.toml`, and the
  upstream-provenance comment at the top of `Cargo.toml` (commit hash, upstream
  author) stay. This is a legal fact about the code's origin, not a style
  preference — never remove or reword it away.
- The crate-doc attribution in `src/lib.rs` (the Logitech HID++ Google Drive
  folder link, the Solaar-project credit) stays. Keep crediting the sources this
  crate's protocol knowledge came from even as the code itself moves away from
  upstream's structure.
- `[lib] name = "hidpp"` in `Cargo.toml`. Every consumer imports it as
  `hidpp`, not by the package name. Before changing the lib target or its public
  surface, derive the current consumer set with
  `rg -l '\bhidpp::|use hidpp::' crates --glob '*.rs'` and
  `rg -n 'hidpp\s*=' --glob 'Cargo.toml' .`. Renaming the target is not a
  documentation-only change: update every current consumer in the same commit.
  Do not do it as a drive-by.

## Rules that hold regardless of fork/vendor status

These never had anything to do with tracking upstream — they're this crate's own
protocol-correctness contract:

- Protocol facts (byte layouts, feature IDs, function semantics) come from the
  official Logitech HID++ feature specs, never from guessing. Where an offset or
  field was reverse-engineered instead of read from a spec, the comment says so
  — keep those marks honest when you touch nearby code.
- Everything is typed end to end: the `registry.rs` data-macro
  (`known_features!`) + `FeatureEndpoint` pattern for feature wiring, `num_enum`
  for wire discriminants, `bitflags` with `from_bits_retain` where unknown bits
  are legal. An unknown wire value surfaces as an **error**
  (`UnsupportedResponse`-style) — never falls back to a silent default.
- Feature `0x0005` (`device_type_and_name`) is one of four incompatible "device
  kind" vocabularies used across the workspace; the cross-crate rule about never
  mixing them by raw value lives in `crates/openlogi-device/AGENTS.md` (the
  `openlogi-hid` side), not duplicated here.

## Settled: this crate answers to the workspace like any other

`Cargo.toml` used to opt out of the workspace `[lints]` table and to pin its own
`rust-version`, both justified as "it's third-party code" / "so future syncs can
see the fork's own lower MSRV." The hard-fork ruling retired both rationales, and
both opt-outs are gone: the crate now inherits `[lints] workspace = true` and
`rust-version.workspace = true`.

So `clippy::pedantic`, `unwrap_used`, and `expect_used` apply here exactly as
they do elsewhere. There is no "it's vendored" excuse for a bare `unwrap`, and a
suppression needs the same `reason = "…"` any other crate would need.

## Optional serde surface

The optional `serde` dependency gates widespread
`#[cfg_attr(feature = "serde", derive(serde::Serialize))]` library surface. Before
changing it, derive the current feature consumers from the manifests and source.
Types crossing OpenLogi's IPC boundary are converted to `openlogi-core` structs first.
Decide deliberately whether this crate remains general-purpose or strictly internal;
do not remove the feature as a drive-by.
