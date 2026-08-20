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
with local overrides. `openlogi-hidpp` currently stays out of the table — it is a **hard
fork**, so the "third-party code" rationale for that opt-out no longer holds; whether it
should now inherit is an open question, costed in `crates/openlogi-hidpp/AGENTS.md`.

The table: `unsafe_code = "deny"` (opt out per item with `#[expect(unsafe_code,
reason = "…")]` plus a `// SAFETY:` comment), `clippy::pedantic` at warn,
`unwrap_used`/`expect_used` at warn, plus the shared lint set —
`assertions_on_result_states`, `cast_possible_truncation`, `cast_possible_wrap`,
`cast_sign_loss`, `error_impl_error`, `exit`, `or_fun_call`, `ptr_as_ptr`,
`tests_outside_test_module`, `undocumented_unsafe_blocks`. Any lint suppression carries
a `reason`. What that changes day to day:

- Every `unsafe` block needs a `// SAFETY:` comment saying why it is sound.
- `assert!(r.is_ok())` / `assert!(r.is_err())` are rejected — unwrap the `Result` (in a
  test module that already allows it) or give the assertion a message.
- A test module that wants `expect`/`unwrap` says so:
  `#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]` on the
  module (or on its `mod tests;` declaration). Never route around the lint with
  `unwrap_or_else(|e| panic!("…: {e}"))` — that is the same panic with the check switched
  off. The one honest use of that form is a *dynamic* panic message, where `expect` would
  need a `format!` that allocates on the happy path (`expect_fun_call`).
- A test module gated on more than `test` needs stacked attributes (`#[cfg(test)]` then
  `#[cfg(unix)]`), not `#[cfg(all(test, unix))]`, which clippy reads as a test outside a
  test module. Integration tests under `tests/` carry a file-level
  `#![expect(clippy::tests_outside_test_module, reason = "…")]`.
- `std::process::exit` needs `#[expect(clippy::exit, reason = "…")]` naming why that call
  site cannot hand an `ExitCode` back to `main` instead.

Encode invariants in the type system instead of checking them at runtime:

- Wire/firmware values get typed wrappers: `num_enum` for discriminants, `bitflags`
  (`from_bits_retain` when unknown bits are legal) for flag sets. Unknown wire values
  surface as **errors** (`UnsupportedResponse`-style), never as silent fallbacks.
- Replace long parameter lists with Change/Params structs; make illegal combinations
  unrepresentable rather than validated.
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
locally: `.github/scripts/ci-local.sh --list` and `.claude/rules/ci.md`. Host
clippy on macOS does not compile linux cfg; MSRV needs `RUSTUP_TOOLCHAIN`;
cargo-deny is its own job.
