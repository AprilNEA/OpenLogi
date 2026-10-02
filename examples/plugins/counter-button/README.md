# Counter button plugin

This component decodes reports from a synthetic HID device, counts rising edges, and emits a short-press trigger after `minimum_count` presses. Every attachment has its own counter. The example does not match a real product.

Build from the repository root with the installed `wasm32-unknown-unknown` Rust target:

```sh
cargo build --manifest-path examples/plugins/counter-button/Cargo.toml --target wasm32-unknown-unknown --release
cargo run -p openlogi-plugin --example componentize -- examples/plugins/counter-button/target/wasm32-unknown-unknown/release/openlogi_counter_button.wasm examples/plugins/counter-button/package/driver.wasm
```

Install the `package` directory using [the peripheral workflow](../../../docs/PERIPHERALS.md#wasm-plugins). Edit the descriptor's synthetic VID/PID and collection only for a device whose reports you have verified. Grant only the required report IDs and operations.

The `alternate` feature changes the counting algorithm, so tests can prove that replacing guest bytes changes behavior without rebuilding the host. `conformance-faults` adds deliberate quota and permission violations for tests. Build the checked conformance artifact with both features:

```sh
cargo build --manifest-path examples/plugins/counter-button/Cargo.toml --target wasm32-unknown-unknown --release --features alternate,conformance-faults
cargo run -p openlogi-plugin --example componentize -- examples/plugins/counter-button/target/wasm32-unknown-unknown/release/openlogi_counter_button.wasm crates/openlogi-plugin/tests/fixtures/counter-boundaries.wasm
cargo test -p openlogi-plugin --all-features
```

Do not install the conformance component on a real device. The package directory must contain only the manifest and its referenced files; keep this README outside it.
