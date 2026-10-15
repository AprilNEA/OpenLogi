# openlogi-ipc — the wire format is append-only

The GUI and agent speak tarpc over bincode on an `interprocess` local socket
(`openlogi-ipc/src/ipc.rs`). bincode encodes the enum **variant index** and
tarpc encodes the **method order**, so the wire format is positional:

- Service methods are append-only; never reorder or remove. `protocol_version` must
  remain method 0 forever — the takeover handshake probes it across versions.
- serde enums that cross the IPC boundary are append-only too. serde encodes the
  declaration index, NOT a `#[repr(u8)]` discriminant — the two can disagree. The wire
  surface is wider than this crate: serde types from `openlogi-core` (device model,
  `DeviceKind`, actions, config) and `openlogi-hid` (write errors) ride inside RPC
  payloads, so the same rule binds them.
- Any wire change regenerates the golden tests in
  `crates/openlogi-ipc/tests/wire_format.rs` — the failure message prints the bytes
  (`left` is the new encoding) — and needs `PROTOCOL_VERSION` (checked strict-equal
  at connect) above the latest release's. Run
  `cargo test -p openlogi-ipc --test wire_format` before any push that touched wire
  types.
- `PROTOCOL_VERSION` moves once per release, not once per change. Both binaries ship
  and update together, so the check only has to tell released builds apart. Read the
  released value with
  `git show "$(git describe --tags --abbrev=0 --match 'v*' origin/master)":crates/openlogi-ipc/src/ipc.rs | grep 'pub const PROTOCOL_VERSION'`:
  - master still at the released value: increment it, add a `/// vN:` history line,
    and update the pinned-version assertion.
  - master already above it: the change joins that unreleased version. Extend its
    `/// vN:` line and leave the constant and the pin alone. When a rebase brings in
    an unreleased bump, drop yours and fold its note into that line.

  Two dev builds of one unreleased version can then disagree on the wire without a
  version error; rebuild both.
- The goldens use tokio-serde's `Bincode::default()` = bincode `DefaultOptions`
  (varint, little-endian); the free `bincode::serialize` functions are fixint and do
  NOT produce matching bytes.
- Debug-build agents never take over a running release agent — that is by design (a
  dev agent must not displace the user's production agent), not a bug.

## The client policy lives here too

`src/client.rs` owns everything a client must do identically: `connect_as(kind)` is
the handshake (connect, judge the version in both directions, declare — all within
`HANDSHAKE_TIMEOUT`, so no caller adds a timeout of its own),
`probe_version` the agent's takeover probe, and `Observer` the observe loop's
connection state: the client, its generation ledger, and the one observe call in
flight under a deadline that outlasts the hold (`Observer::state` /
`Observer::action_ring`, then `next()`; the thread a client loop runs on is
`openlogi_core::worker`'s). Consumers never compare `PROTOCOL_VERSION`, call
`declare_client`, `observe` or `observe_action_ring`, compare generations, or open
the transport themselves — this crate's `.ast-grep/rules/ipc-*.yml` guards fail the
`ast-grep` CI job on any of that outside this crate. A new decision every client
must share goes here, with its guard, not into the first client that needs it.
`testing::in_memory_agent` (feature `test-support`) is the scripted agent for
client tests.
