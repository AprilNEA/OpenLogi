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
  `use hidpp::...` — as of this writing that's 30+ files, all in
  `crates/openlogi-hid/src/**`, plus a doctest in this crate's own `src/lib.rs`.
  Renaming the lib target is not a documentation-only change: it means touching
  every one of those call sites in the same commit. Don't do it as a drive-by.

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
  mixing them by raw value lives in `.claude/rules/hidpp.md` (the
  `openlogi-hid` side), not duplicated here.

## Open question — not decided by this file

`Cargo.toml` currently opts this crate out of the workspace's `[lints]` table and
pins its own `rust-version = "1.96"` separately from the workspace MSRV, both
justified in `Cargo.toml`'s comments as "it's third-party code" / "so future
syncs can see the fork's own lower MSRV." With the hard-fork ruling, that stated
justification no longer holds — but this file does not resolve it, and this
commit does not touch `Cargo.toml`.

Adopting `[lints] workspace = true` here means fixing every `clippy::pedantic` /
`unwrap_used` / `expect_used` violation the workspace table would newly flag
across this crate — a real, separately-scoped piece of work, not a toggle. Until
someone does that work and updates `Cargo.toml`'s comments to match, treat the
opt-out and the separate MSRV pin as unresolved, not as license to keep writing
non-pedantic code here on the "it's vendored" excuse. This should be filed as a
tracked issue rather than actioned ad hoc or left as a silent TODO.
