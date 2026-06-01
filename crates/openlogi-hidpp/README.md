# openlogi-hidpp

OpenLogi's vendored fork of the [`hidpp`](https://crates.io/crates/hidpp) crate —
an implementation of the Logitech HID++ protocol.

- **Upstream:** <https://github.com/lus/logy> (crate `hidpp`)
- **Forked at:** commit `135c5600807845c269b5d5bfa1f33032281fbd86` (upstream v0.3.0, 2025-12-26)
- **License:** 0BSD © Lukas Schulte Pelkum (see [`LICENSE`](./LICENSE))

The library target is named `hidpp`, so dependents `use hidpp::…` unchanged.
OpenLogi-specific changes live here; the source is otherwise kept close to
upstream to ease future syncs.

The crate is versioned with the OpenLogi workspace (unified versioning), not
upstream's `0.3.0` — that number is provenance, recorded above.
