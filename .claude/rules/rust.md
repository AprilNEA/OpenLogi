---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
---

# Rust standards

Edition 2024, MSRV = current stable (1.98). OpenLogi ships as an app and no crate
here has an external reverse dependency, so the floor exists only to give
`cargo install` users a clear error — it tracks stable rather than trailing it.
Reaching for a newly stabilized API is fine: raise `rust-version` and the `msrv`
CI matrix together, and run `devenv update rust-overlay` so the local toolchain
matches CI. There is exactly **one** lint table, in the root `Cargo.toml`,
and every crate inherits it with `[lints] workspace = true` — never a private copy, or
the next lint added to the workspace silently skips that crate. A crate needing a
different level opts out **in source** (the `openlogi-hook` platform modules carry
`#![allow(unsafe_code, reason = "…")]`), because Cargo rejects mixing `workspace = true`
with local overrides. `openlogi-hidpp` inherits the table like everything else, hard
fork or not.

Workspace-wide clippy *configuration* lives in `clippy.toml` at the root. It currently
carries `allow-unwrap-in-tests` / `allow-expect-in-tests`, so test code never needs to
restate that exemption by hand.

The table: `unsafe_code = "deny"` (opt out per item with `#[expect(unsafe_code,
reason = "…")]` plus a `// SAFETY:` comment), `clippy::pedantic` at warn,
`unwrap_used`/`expect_used` at warn, plus the shared lint set —
`assertions_on_result_states`, `cast_possible_truncation`, `cast_possible_wrap`,
`cast_sign_loss`, `error_impl_error`, `exit`, `or_fun_call`, `ptr_as_ptr`,
`tests_outside_test_module`, `undocumented_unsafe_blocks`. `allow_attributes` and
`allow_attributes_without_reason` machine-check the suppression rules below. What that
changes day to day:

- Every `unsafe` block needs a `// SAFETY:` comment saying why it is sound.
- `assert!(r.is_ok())` / `assert!(r.is_err())` are rejected — unwrap the `Result` (in a
  test module that already allows it) or give the assertion a message.
- Tests use `expect`/`unwrap` freely: `clippy.toml` exempts `#[cfg(test)]` modules and
  `#[test]` functions, so **do not** add a suppression for it. The one shape clippy
  cannot see is a free helper in a `tests/` integration file, outside any `#[test]` fn —
  that file needs a `#![expect(clippy::expect_used, reason = "…")]`. Never route around
  the lint with `unwrap_or_else(|e| panic!("…: {e}"))` — that is the same panic with the
  check switched off. The one honest use of that form is a *dynamic* panic message,
  where `expect` would need a `format!` that allocates on the happy path
  (`expect_fun_call`).
- A test module gated on more than `test` needs stacked attributes (`#[cfg(test)]` then
  `#[cfg(unix)]`), not `#[cfg(all(test, unix))]`, which clippy reads as a test outside a
  test module. Integration tests under `tests/` carry a file-level
  `#![expect(clippy::tests_outside_test_module, reason = "…")]`.
- `std::process::exit` needs `#[expect(clippy::exit, reason = "…")]` naming why that call
  site cannot hand an `ExitCode` back to `main` instead.

### `expect` by default, `allow` only when `expect` would break

`#[allow]` goes quiet the day it stops suppressing anything, so suppressions rot in
place — an audit in 2026-08 found 20 dead ones, including three module-wide `dead_code`
blankets that had been inert since their modules went `pub`. `#[expect]` reports itself
unfulfilled instead, which `-D warnings` turns into a failure. `allow_attributes`
enforces this — but only for outer `#[allow]`; a module-wide `#![allow(…)]`, the shape
that rots worst, is invisible to it and is on you.

Three cases where `expect` is wrong and `allow` is correct. Each keeps its `allow` plus
an `#[expect(clippy::allow_attributes, reason = "see above")]` and a comment saying which
case applies — riding the same `cfg_attr` predicate when there is one:

- **The lint fires only under some `cfg`.** `platform::os_version` returns `Some(…)` on
  macOS (so `unnecessary_wraps` fires) and `None` elsewhere (so it does not). An
  `expect` there is green on macOS and red on the other two lanes.
- **Fulfilment differs between a crate's targets.** A `dead_code` suppression on a
  helper that only the tests call is fulfilled in the `--lib` build and unfulfilled in
  the `--test` build; `--all-targets` compiles both, so one of them always warns. Being
  `cfg_attr`-wrapped is *not* itself a reason to reach for `allow` — check.
- **The lint is raised inside a macro expansion.** rustc does not credit an expectation
  with such a lint: it suppresses the warning *and* reports itself unfulfilled. A
  `float_cmp` on floats compared inside `assert_eq!` is the case in this tree.

Scope a suppression to the item that needs it. A file-level `#![expect(cast_…)]` also
covers every cast added to that file later, which is how a bounded-by-construction
argument silently becomes a blanket one. Prefer removing the need instead: `cast_signed`
/ `cast_unsigned` for bit-reinterpreting casts, `&raw const` / `&raw mut` for raw-pointer
coercions, `to_le_bytes` for byte splitting, or one shared conversion helper carrying the
single suppression when a file converts the same pair of types over and over.

Sweeping for rot is mechanical: rewrite every non-`cfg_attr` `allow(` to `expect(`, run
clippy, and each "this lint expectation is unfulfilled" is a suppression to delete.
Do it on all three lanes — `cargo clippy`, `--target x86_64-pc-windows-gnu`, and
`--target aarch64-unknown-linux-musl` — because CI has no macOS clippy job and a
platform-gated suppression is only ever evaluated on its own platform.

Encode invariants in the type system instead of checking them at runtime:

- Wire/firmware values get typed wrappers: `num_enum` for discriminants, `bitflags`
  (`from_bits_retain` when unknown bits are legal) for flag sets. Unknown wire values
  surface as **errors** (`UnsupportedResponse`-style), never as silent fallbacks.
- Replace long parameter lists with Change/Params structs; make illegal combinations
  unrepresentable rather than validated.
- A `bool` parameter is boolean-blind at its call sites. When only a couple of
  combinations are ever used, split into intent-named methods
  (`divert_cid`/`undivert_cid`, not `set_cid_reporting(cid, bool, bool)`).
  Otherwise name the facts: a struct with named fields when they are independent
  (`ScanPass { complete, healthy }`), a sum type only when the erased
  combinations are truly meaningless — checked against persisted state, not
  just the current UI branches: `HiresWheel { Here, Elsewhere, Nowhere }`
  collapses a display precedence, while collapsing
  `(inversion_supported, inverted)` erased the configured-but-unsupported
  state a disabled toggle must still show. An `Option<bool>` encoding a genuine
  three-state is the same defect (`HidrawProbe { Accessible, Denied,
  NonePresent }`). `struct_excessive_bools` firing is the signal to re-type,
  not to `expect`.
- When a loop scatters mutable locals that feed one free decision function, fold
  the state and the rule into a sans-I/O object: events become named methods,
  the decision method is pure and takes an explicit `now`, and all I/O stays in
  the loop (`SpawnReflex`, `OneShotScan`; older precedents `RearmBudget`, the
  haptics `Budget`, `QueryState`). Tests then drive real transitions and cannot
  construct unreachable states; a total decision function earns one exhaustive
  truth-table test rather than scattered single-case asserts.
- Lifecycles are typestate: stages are types, transitions consume `self`
  (`Booted::arm(self) -> Armed`), and a resource legal in only some stages
  travels inside the stage that may hold it — a third consumer then cannot
  exist by construction.
- Ownership models resources (`Retained<T>` in the ObjC FFI) and thread affinity is
  proven by types (`MainThreadMarker`, `!Send` handles), not by runtime checks.
- Libraries return `thiserror` types; binaries may use `anyhow`.

House style:

- **Root-cause fixes only.** Never layer compatibility shims over a broken abstraction —
  refactor it. Never change product code to work around a dev-environment quirk; debug
  the environment (or a release build) instead.
- **Prefer mature crates over hand-rolled logic** (retry/backoff, hashing, paths, …).
  Check `cargo tree | grep <candidate>` before adding a dependency and use `cargo add`
  so versions come from the registry. After ANY dependency change, verify the
  `gpui`/`gpui-component` git pins in `Cargo.lock` didn't move (they are held only by
  the lock; restore with `cargo update -p gpui --precise <rev>`).
- Module layout: a module with its own semantics is `foo.rs` (children in a sibling
  `foo/`); `foo/mod.rs` is only for pure namespace shells. Never both for one module.
- Platform-divergent code: once more than one function diverges, use one module per
  OS selected by a single `cfg` at the module declaration, with a thin facade owning
  the shared types and dispatch — `inject.rs` → `inject/{macos,linux,windows}.rs`,
  `autostart.rs` likewise — not a file interleaved with repeated
  `#[cfg(target_os = …)]` arms. Each platform file implements the same function
  names; a missing one fails that platform's compile, which is the same guarantee a
  trait would give here. Reach for a trait only when implementations genuinely
  coexist — runtime backend selection, test doubles, or a cross-crate seam
  (`HidBackend`) — never as ceremony around a compile-time-exclusive choice. A
  single small divergent function (`platform/os.rs`) stays inline. Splitting does
  not lift the cross-platform rule: the non-host files are only ever compiled by
  that platform's CI or a cross-lint, so `.claude/rules/cross-platform.md` applies
  with full force.
- Keep files reasonably sized (split around ~500 lines) into real modules — never
  simulate structure with `// ---- section ----` banner comments. But don't
  over-extract either: inline single-use helpers.
- rustdoc every public item. Comments state non-obvious constraints only.
- Tests cover failure and edge paths, not just the happy path (state machines
  especially). No tautological tests that mirror the implementation; never weaken an
  assertion or special-case an input to make a test pass.

## rustdoc: intra-doc links break silently when impls move

rustdoc resolves a `Type::trait_method` link only while that trait is **in scope**, so
handing a hand-written trait impl over to a derive macro deletes the now-unused `use`
and silently breaks every such link — neither a compile error nor a clippy lint, only
the pre-push rustdoc gate catches it. Re-adding the import does not fix it either: a
doc link does not count as a use, so that just trades the broken link for an
`unused_imports` failure — write the trait method's full path instead. After any
refactor that moves impls between hand-written and generated, grep for doc links
naming that trait's methods.

## Reproducing CI

`openlogi:check` is the host-OS gate, not the pipeline. To run a `ci.yml` job
locally: `cargo xtask ci --list` and `.claude/rules/ci.md`. Host
clippy on macOS does not compile linux cfg; MSRV needs `RUSTUP_TOOLCHAIN`;
cargo-deny is its own job.
