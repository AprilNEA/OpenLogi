# Vendored tarpc (temporary security pin)

This is google/tarpc **0.37.0** with a minimal dependency bump so OpenLogi no
longer pulls the vulnerable `opentelemetry_sdk` 0.30 line
([GHSA-w9wp-h8wv-79jx](https://github.com/advisories/GHSA-w9wp-h8wv-79jx) /
CVE-2026-48504).

## Why

`tarpc` 0.37 on crates.io hard-depends on:

- `opentelemetry ^0.30`
- `tracing-opentelemetry ^0.31`

`tracing-opentelemetry` 0.31 has a normal dependency on `opentelemetry_sdk
^0.30`. The patched SDK is `0.32.1`, which is semver-incompatible with that
range, so Dependabot reports `security_update_not_possible`.

Upstream has not shipped a release that allows the 0.32 line yet (crates.io is
still 0.37.0 / otel 0.30; `main` is only on otel 0.31). See
[google/tarpc#564](https://github.com/google/tarpc/issues/564) and the open
Dependabot bumps on that repo.

## Delta from crates.io 0.37.0

1. `Cargo.toml`: `opentelemetry` / `opentelemetry-semantic-conventions` → 0.32,
   `tracing-opentelemetry` → 0.33 (0.33 no longer depends on
   `opentelemetry_sdk` at runtime — it is dev-only).
2. `src/context.rs`: ignore the `Result` from `OpenTelemetrySpanExt::set_parent`
   (return type changed in `tracing-opentelemetry` 0.32+).

No tarpc API used by OpenLogi was changed.

## Remove this vendor when

A crates.io `tarpc` release depends on `tracing-opentelemetry` ≥ 0.32.1 (or
otherwise no longer pulls `opentelemetry_sdk` < 0.32.1). Then drop
`[patch.crates-io].tarpc` from the workspace `Cargo.toml` and delete this
directory.
