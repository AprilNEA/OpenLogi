# Flow — cross-machine device handoff (design)

Status: **proposal** — nothing here is implemented yet. This document records
the intended architecture, the protocol decisions and their rationale, and the
prior-art research they rest on, so implementation can start from settled
ground. Once milestones land, durable decisions graduate into
[DECISIONS.md](DECISIONS.md).

The peer protocol itself is specified normatively in
[FLOW-PROTOCOL.md](FLOW-PROTOCOL.md) (framing, stream binding, state
machines, evolution rules) with the message schema in
[flow.v1.proto](flow.v1.proto); both move into `crates/openlogi-flow` when it
exists.

## What Flow is

Logitech Flow (a Logi Options+ feature) lets one mouse and keyboard roam
between up to three computers: move the cursor to the edge of one screen and
the devices hop to the next machine, with clipboard contents following. The
mechanism is **not** input forwarding over the network (Synergy/Deskflow/
lan-mouse style). The device itself is paired to each machine on a separate
Easy-Switch channel; the software merely commands the device to switch
channels (HID++ `ChangeHost`, feature `0x1814`) and uses the network only for
coordination and clipboard transfer. That distinction is the product:

- input never crosses the network — no keystroke sniffing surface, no added
  input latency;
- devices keep working on whatever host they are switched to even when the
  other machines are off;
- the radio link (receiver or Bluetooth) does what it already does; software
  only chooses *which* link is live.

OpenLogi Flow is the same model, between machines running OpenLogi.

### Goals

- Edge-of-screen handoff of a mouse (and linked keyboard) between two or three
  machines running OpenLogi, across macOS / Windows / Linux.
- Peer discovery, pairing, and transport that are LAN-only, encrypted, and
  mutually authenticated — no account, no cloud, no telemetry, consistent with
  the rest of OpenLogi.
- Clipboard sync (text first, files later) over the same peer link.
- Keyboard-follows-mouse via the existing host-switch link machinery.

### Non-goals

- **Interoperability with Logi Options+ Flow.** See
  [Options+ protocol compatibility](#options-protocol-compatibility) — the
  protocol is undocumented, encrypted, partly cloud-dependent, and a moving
  target. Both machines must run OpenLogi.
- Input forwarding to machines that have no channel paired (that is a software
  KVM, a different product).
- Rendezvous or relay infrastructure of any kind, vendor or self-hosted.
  Options+ uploads node info to `datapipeline.logitech.io` ("NodeStore") as a
  discovery assist; OpenLogi will never phone a vendor endpoint, and it also
  does not ship its own server. Machines that cannot see each other's
  broadcasts are connected by the user's own network — see
  [Non-LAN connectivity](#non-lan-connectivity-bring-your-own-network).

## Prior art

Research summary; links are the sources this design leans on.

**Official Logitech material.** The
[Options+ Flow port documentation](https://support.logi.com/hc/articles/17498487100055-Flow-for-Logi-Options-Port-Information)
shows the network footprint: fixed UDP 59870 subnet broadcast for discovery, a
TCP presence port (default 59869) advertised inside discovery messages, UDP
59871 for the NodeStore cloud-assisted path (legacy Options used TCP 59866,
UDP 59867/59868). The
[official FAQ](https://support.logi.com/hc/en-us/articles/1500005634742)
confirms: same-subnet UDP broadcast reachability is a hard requirement, up to
three computers (= Easy-Switch channel count), and a "hold Ctrl to cross"
option guards against accidental edge crossings (worth copying as a setting).

**Solaar** ([issue #672](https://github.com/pwr-Solaar/Solaar/issues/672)):
the community has never reverse-engineered Flow's network protocol ("the
communications protocol that is used is unknown"). The issue also articulates
the channel-switch-vs-input-forwarding distinction above.

**[logitech-flow-kvm](https://github.com/coddingtonbear/logitech-flow-kvm)**
(Python, Linux) is the closest working third-party Flow. Its key insight is
the **positive-evidence model for "where did the device land"**: a device
*disconnect* only proves departure; the machine that receives the device's
*connect* notification (HID++ 1.0 connection notification, sub-ID `0x41`,
link-status bit) has positive evidence of arrival, and that machine reports
the authoritative new host. Followers are moved with `0x1814` fn `0x10`,
written without awaiting a reply (the device leaves before it could answer) —
matching our existing `ChangeHostFeature::set_current_host`. Its weaknesses
(central server topology, no discovery, non-cryptographic pairing-code RNG,
one-way trust, text-only clipboard) are things this design deliberately does
differently.

**[lan-mouse](https://github.com/feschber/lan-mouse)** (Rust) is an input
forwarder — a different model — but the best reference for the platform layer:

- macOS edge detection via `CGEventTap` + display bounds, clamping the cursor
  1 px inside the edge on crossing. Known flaw to avoid: it tests against the
  *union bounding rectangle* of all displays, so irregular monitor
  arrangements misdetect; per-display exposed-edge segments are the correct
  geometry from day one (its own Windows backend already does this).
- Wayland edge detection is possible and shipped, two ways: wlroots
  layer-shell 1 px edge surfaces + pointer constraints, or the freedesktop
  InputCapture portal + libei. Wayland is therefore a later milestone, not a
  permanent exclusion.
- Its DTLS trust is one-way (the outbound side sets `insecure_skip_verify`) —
  a concrete cautionary example; our transport must authenticate both
  directions.
- Its `Enter` message carries only a four-way side, no coordinate, so entry
  position is not preserved across machines. Our handoff message carries the
  normalized entry point.

**Local Easy-Switch synchronizers**
([LogiKMSwitch](https://github.com/Larry57/LogiKMSwitch),
[Lunaar](https://github.com/NSHenry/Lunaar)) prove the observe-one-device,
switch-the-others pattern that OpenLogi's existing host-switch link already
implements.

**[Options+ agent IPC reverse engineering](https://github.com/saimanish1/logitech-ipc-protocol/blob/master/logi-options-ipc-reverse-engineering.md)**:
Options+ internally drives `ChangeHost` through its agent over a protobuf IPC
(`logi.protocol.devices.ChangeHost`), and macOS kernel policy blocks raw HID
access to Bluetooth input devices for everyone — confirming that Bluetooth
device switching must be executed by the resident agent, which OpenLogi's
architecture already guarantees (the agent owns all device I/O).

## Building blocks already in the tree

| Piece | Where | State |
|---|---|---|
| `ChangeHost` (`0x1814`), fire-and-forget `set_current_host` | `crates/openlogi-hidpp/src/feature/change_host.rs` | done |
| `HostsInfo` (`0x1815`) host slots/names | `crates/openlogi-hidpp/src/feature/hosts_info.rs` | done |
| Keyboard→pointer host-switch links (keyboard-follow) | `crates/openlogi-device/src/session/host_switch.rs`, `crates/openlogi-agent-core/src/watchers/host_switch.rs` | done |
| Global cursor position + mouse event stream | `crates/openlogi-hook` | done |
| Cursor/synthesis injection (entry-point warp) | `crates/openlogi-inject` | done |
| Versioned local IPC with observation cells | `crates/openlogi-ipc`, `crates/openlogi-agent-core/src/observable.rs` | done |

What is missing is the network layer and the orchestration on top.

## Architecture

Two machines, each running the standard OpenLogi agent. The GUI stays a pure
IPC client; nothing about the three-process model changes.

```
┌────────────── machine A ──────────────┐      ┌────────────── machine B ─────────────┐
│ hook: cursor hits configured edge     │      │                                      │
│   ↓                                   │ LAN  │                                      │
│ flow orchestrator ──"handoff(entry)"──┼─────▶│ flow orchestrator                    │
│   ↓                                   │(QUIC)│   ↓ awaits 0x41 connect notification │
│ ChangeHost.set_current_host(B's slot) │      │   ↓ inject: warp cursor to entry pt  │
│ (device departs A; A is now blind)    │      │   ↓ confirms ──▶ A (timeout ⇒ abort) │
└───────────────────────────────────────┘      └──────────────────────────────────────┘
```

| Component | Crate | Responsibility |
|---|---|---|
| Peer networking | `openlogi-flow` (new) | Discovery, pairing, encrypted transport, peer message protocol. Pure network — knows no HID, no hook. |
| Flow orchestration | `openlogi-agent-core/src/flow/` (new) | Edge detection, device↔channel mapping, the handoff state machine, keyboard-follow linkage. Bridges `openlogi-flow` to device I/O and the hook. |
| Config | `openlogi-core` | `[flow]` TOML section: enabled, peer trust store reference, edge layout. |
| IPC surface | `openlogi-ipc` | Appended RPCs + snapshot fields for status, pairing, layout (see [IPC integration](#ipc-integration)). |
| GUI | `openlogi-desktop` | Settings page: discovered peers, pairing confirmation, screen-arrangement editor. |

`openlogi-flow` and `openlogi-ipc` must not depend on each other; all
conversion happens in `openlogi-agent-core`. The GUI never speaks the peer
protocol.

### Device↔channel mapping

Both machines see the same physical device (matched by serial / unit id) on
different Easy-Switch channels. During setup each peer reports, for every
shared device, the channel index the device uses to reach *it* (its own
`current_host` reading while it holds the device, cross-checked against
`HostsInfo`). The mapping `{device, peer} → channel` is exchanged over the
peer link and stored in config. Handoff then means: look up the target peer's
channel for each device being moved and write it with `set_current_host`.

### Handoff ordering and confirmation

One ordering law governs everything: **once `set_current_host` takes effect,
this machine's HID++ channel to the device is gone.** (The same law is already
documented in `session/host_switch.rs` for keyboard-initiated switches.)
Therefore:

1. Sender detects edge crossing, sends `Handoff { device_set, entry_edge,
   normalized_entry_point }` to the target peer, and only *after* that message
   is on the wire issues `set_current_host` for the pointer (keyboard follows
   via the existing link machinery).
2. Receiver arms a watch for the devices' arrival. Arrival is detected by the
   **positive-evidence signal**: the receiver-side HID++ connection
   notification (`0x41`, link established) or the transport-level equivalent
   for Bluetooth/direct devices — not by assuming the switch worked.
3. On arrival, the receiver warps the cursor to the entry point
   (`openlogi-inject`) and sends `HandoffComplete`.
4. If the sender gets no `HandoffComplete` within the deadline it surfaces a
   diagnostic; recovery paths are the device's own enhanced-host-switch
   fallback cookie (`0x1814` capability bit), the user pressing an Easy-Switch
   key, or the receiving side commanding the device back.

An anti-accident option ("hold Ctrl to cross", mirroring Options+) gates step
1 in config.

## Peer protocol

### Why not the local IPC stack

The local agent↔GUI IPC (tarpc + positional bincode, strict version equality,
golden byte tests) is correct **because** GUI, agent, and overlay ship
atomically in one bundle — a version mismatch is transient by construction, so
"strict equal or refuse" is the whole contract. None of that holds across two
machines: version skew is the *normal* state. The peer protocol therefore
shares neither the encoding, nor the version philosophy, nor any serde types
with `openlogi-ipc`/`openlogi-core`. Duplicating a few field definitions is
cheaper than turning every innocent core-type edit into a cross-machine
compatibility hazard that no golden test covers.

### Transport: QUIC (`quinn` + `rustls`), pinned-key identity

- One long-lived QUIC connection per peer pair. Multiplexed streams mean the
  latency-critical handoff exchange never queues behind a clipboard/file
  transfer (no head-of-line blocking); connection migration and fast resume
  cover the daily realities of laptops (sleep/wake, Wi-Fi roam, IP change).
- Identity is a persistent per-machine Ed25519 key. TLS uses self-signed
  certificates carrying that key; a custom `rustls` verifier accepts exactly
  the pinned peer keys from the trust store, **in both directions** (mutual
  authentication — lan-mouse's one-way trust is the counterexample).
- Pairing: peers exchange keys over the first (unauthenticated) connection and
  the user confirms a short code derived from both keys on both screens; on
  confirmation each side persists the other's key. The code authorizes the
  pinning, it is not key material.
- Discovery: mDNS (`mdns-sd` crate — pure Rust, no Avahi/Bonjour daemon
  dependency), service `_openlogi-flow._udp.local`, TXT records carrying
  instance id and supported protocol range so incompatible peers are filtered
  before any connection attempt. Internally, discovery is a *list of candidate
  sources* (mDNS and manually configured addresses, tried concurrently), each
  yielding candidate addresses for a peer key; the session layer authenticates
  whoever answers. Addresses are hints, never identity — which is also what
  makes user-provided overlay networks work (see
  [Non-LAN connectivity](#non-lan-connectivity-bring-your-own-network)).
- The alternative considered and kept as fallback: TCP + Noise (`snow`).
  Smaller dependency footprint, but hand-rolled framing/rekey/replay
  protection, a second connection to avoid HOL blocking, and no migration.
  Migration between the two later touches only `openlogi-flow` internals.

### Encoding: protobuf (`buffa`)

Cross-machine messages must survive version skew. Candidates weighed:

| Option | Evolution story | Verdict |
|---|---|---|
| protobuf ([`buffa`](https://github.com/anthropics/buffa)) | field tags, unknown fields preserved, `optional` semantics, open enums | **chosen** — the `.proto` file doubles as the protocol document |
| protobuf (`prost`) | same wire format; enum fields are `i32` with accessors that silently fold unknown values to the default | runner-up — see below |
| serde + CBOR (named fields) | self-describing, but renames break silently; discipline lives in review, not tooling | viable fallback |
| positional bincode across machines | append-only + send-gating done entirely by hand; every mistake is silent byte-level corruption | rejected — it works locally only because skew cannot happen there |
| Cap'n Proto / FlatBuffers | good evolution, rough Rust ergonomics | overkill for this message volume |
| gRPC (`tonic`) | protobuf's evolution, but drags in HTTP/2 alongside QUIC | protobuf without gRPC |

Within protobuf, `buffa` over `prost`, for fit against rules this design has
already committed to:

- **Open enums.** FLOW-PROTOCOL.md's evolution rule #3 requires an unknown or
  `UNSPECIFIED` enum value to surface as an error, never a silent default.
  buffa's `EnumValue<T>` (Known/Unknown) makes the violating code
  unwritable; prost's generated accessors fold unknown values into the
  default, so the same rule would live in review discipline instead of the
  type system — the exact trade that favored protobuf over CBOR in the
  first place.
- **Unknown-field preservation** by default, and zero-copy `MessageView<'a>`
  decode for `Chunk` frames. Nice, not decisive.
- Same codegen shape (`buffa-build` + `protoc` on PATH, like `prost-build`),
  and it deliberately ships no schema-less `#[derive(Message)]` — the
  `.proto` file stays the only contract, which is this design's stance.
- The accepted risk: buffa is pre-1.0 (breaking changes on minor versions)
  and young (2026), versus prost's decade of production use. The wire format
  is standard protobuf either way, so the codec is swappable: churn or a
  forced migration is contained inside `openlogi-flow`'s generated-code
  layer, and the crate pins `buffa = "0.x"` to a minor version.

`buffa`/`buffa-build` will be the workspace's first codegen dependency; this
section is the deliberate justification the root AGENTS.md asks for.
(Options+ itself speaks protobuf internally — see prior art — which is
corroborating, not binding.)

### Version negotiation

Three layers, the outer ones frozen forever (normative details:
[FLOW-PROTOCOL.md](FLOW-PROTOCOL.md); message schema:
[flow.v1.proto](flow.v1.proto)):

```
┌─ layer 0: envelope (frozen; any version can decode) ─────────────┐
│ Frame { kind: u16, flags: u16 = 0, len: u32, payload: bytes }    │
│ → unknown kind: skip len bytes; decode failure never desyncs     │
├─ layer 1: Hello (kind = 0x0001, encoding frozen) ────────────────┤
│ Hello { proto_min, proto_max, public_key, session_nonce,         │
│         machine_name, platform, app_version, capabilities }      │
├─ layer 2: negotiated messages ───────────────────────────────────┤
└──────────────────────────────────────────────────────────────────┘
```

- Both sides send `Hello`; `agreed = min(max_A, max_B)`. If
  `agreed < max(min_A, min_B)`, reply `HelloReject { reason }` and disconnect;
  the GUI names which side is stale.
- **Decode compatibility comes from append-only message evolution** (protobuf
  tags absorb the field-level cases). **Send compatibility comes from
  gating**: knowing the peer speaks version N, never send message kinds or
  semantics introduced after N. Gating is the discipline the local IPC never
  needed (strict equality made it moot) and the one that must be enforced in
  review here.
- `capabilities` is an open list (repeated enum, not a u64 bitset — no
  64-feature ceiling to patch around later) carrying orthogonal optional
  features (clipboard, file transfer, …) so an optional module never forces
  a version bump across the fleet.
- `proto_min` is raised only as an explicit product decision to drop old
  versions, never casually.

Two frozen invariants, restated as evolution rule #1 in FLOW-PROTOCOL.md: the
envelope layout, and `Hello`'s encoding — the same principle that keeps
`protocol_version` as method 0 of the local IPC forever. (`Hello` is kind
0x0001, not 0, so an all-zero header is invalid rather than a valid frame.)

### RPC shape: QUIC-native, no framework

Half of Flow's traffic is request/response (pairing steps, mapping exchange,
handoff-with-ack, clipboard fetch). QUIC provides the RPC primitive directly —
**one bidirectional stream per request**: open, write request, read response,
close. Correlation (the stream), cancellation (stream reset), and concurrency
(independent streams) come free. Fire-and-forget state notices ride
unidirectional streams; heartbeats ride datagrams; bulk clipboard/file data
gets its own streams. `openlogi-flow` needs a thin `call<Req, Resp>()` helper,
not an RPC framework. tarpc specifically is not reused here: its generated
request enum hides message layout inside a macro, while the peer contract
must be explicit (the `.proto` file) to keep the gating discipline auditable,
and its deadline/cancellation semantics assume a reliable local transport.

### Non-LAN connectivity: bring your own network

First, the use case, precisely: Flow's physical precondition is that the
device's radio reaches every participating machine, so the machines are
always within a few meters of each other. "Not on the same LAN" therefore
never means "in different places" — it means **same desk, segregated
networks**: a work laptop force-tunneled through a corporate VPN, a personal
desktop on home Wi-Fi, a machine on a guest VLAN or a 5G hotspot. Broadcast
and mDNS are dead across those boundaries (Logitech's own FAQ tells users to
involve their network admin), while the mouse hops between the machines just
fine. Genuinely remote machines are out of reach of the device and therefore
out of scope: that product is remote control / software KVM, a non-goal
above.

The stance: OpenLogi does not build or ship rendezvous/relay infrastructure
for this. The same philosophy that makes config sync the user's business
(plain TOML, sync it with whatever you already use) applies to connectivity:
**users who need cross-segment reach bring their own network** — WireGuard,
Tailscale/Headscale, ZeroTier, or any routed path. Flow's obligation is only
to *work over such networks*, which reduces to four requirements binding from
milestone 2 onward:

1. **Manual peer addresses are first-class config**, not a debug fallback: a
   peer may list hostnames or IPs, resolved through the OS resolver so
   `/etc/hosts`, ordinary DNS, and overlay magic-DNS names all work
   (lan-mouse precedent). mDNS is an optimization for the flat-LAN case,
   never a requirement for a link to form.
2. **Trust never derives from reachability.** Identity is the pinned key and
   the session is mutually authenticated E2E, so a shared or hostile overlay
   network adds no risk and no trusted party.
3. **The protocol stays path-agnostic**, and the transport must tolerate
   overlay realities: clamped MTUs (WireGuard 1420, Tailscale 1280 — keep
   quinn's conservative initial MTU / path-MTU discovery defaults) and NAT
   bindings that need keepalives.
4. **Pairing works over any reachable path.** The confirmation-code ceremony
   carries the trust, not network locality, so there is nothing special about
   pairing across an overlay.

Peer/layout config is deliberately syncable by the user (it contains only
public keys and names); the machine's own private key is per-machine state
stored outside `config.toml` and must never travel with a config sync.

Should real demand for a built-in rendezvous/relay ever materialize anyway,
Syncthing is the proven template (key-derived device IDs, self-hostable
`stdiscosrv` discovery and untrusted `strelaysrv` relays, direct →
hole-punched → relayed degradation), with `iroh` as an off-the-shelf
alternative — but BYON is the default answer, and it ships first by shipping
nothing.

## IPC integration

Everything Flow needs from the local IPC is ordinary appending under the
existing rules (`.claude/rules/ipc-protocol.md`): bump `PROTOCOL_VERSION`,
append methods/fields, regenerate the golden tests.

- RPCs: manual `switch_host(route, host)` (also the milestone-1 deliverable),
  flow enable/disable, pairing confirm/reject, layout set.
- State: a `flow` field appended to `AgentSnapshot` — enabled, peers with
  **coarse** link state, pairing session as `FlowPairingPhase { Discovering,
  ConfirmCode(..), Paired, Failed(..) }` following the `PairingPhase`
  "state, not stream" precedent (agent-restart self-healing comes with it).
- Churn rule: `Agent::observe` re-sends the whole snapshot on every generation
  bump, so per-heartbeat facts (RTT, last-seen timestamps) must not live in
  `AgentSnapshot`. Quantize link state to `Connected/Degraded/Lost`. If Flow
  ever needs high-frequency observation, give it its own cell following the
  `RingObservation` precedent (and extract the generic cell then, not before).

## Edge detection

Per-platform, in `openlogi-hook`'s existing cfg-gated style; the orchestrator
consumes a platform-neutral "crossed edge E at normalized position p" event.

- Geometry is **per-display exposed-edge segments**, never the union bounding
  rectangle of all displays (lan-mouse's macOS flaw). Edges adjacent to
  another local display are not handoff edges.
- macOS: cursor stream from the existing `CGEventTap` + display bounds via CG
  APIs; display-reconfiguration callbacks refresh the geometry.
- Windows: `WH_MOUSE_LL` positions + per-monitor rectangles.
- Linux X11: cursor position polling/XInput + XRandR geometry.
- Linux Wayland: no global cursor, but two proven routes exist (wlroots
  layer-shell 1 px edge surfaces + pointer constraints; InputCapture portal +
  libei) — deferred to its own milestone, not excluded.

## Config sketch

```toml
[flow]
enabled = true
# require Ctrl held while crossing an edge (accident guard, mirrors Options+)
require_modifier = false

[[flow.peers]]
name = "work-laptop"
public_key = "ed25519:…"          # pinned at pairing time
# optional; needed when mDNS cannot reach the peer (VPN/VLAN/overlay).
# Hostnames go through the OS resolver, so overlay magic-DNS names work.
addresses = ["work-laptop.tailnet.example", "10.0.0.7"]

[[flow.layout]]
edge = "right"                     # this machine's right edge
peer = "work-laptop"

[[flow.devices]]
key = "unit:0f1e2d3c"              # existing device identity key
peer_channels = { self = 0, "work-laptop" = 1 }
```

Exact shapes to be settled against `openlogi-core`'s config conventions when
milestone 2 lands.

## Milestones (demo-first)

1. **Software-initiated host switch.** IPC `switch_host(route, host)` + a CLI
   diag command, reusing `ChangeHostFeature`. Proof: command the device away,
   Easy-Switch it back. Device-side only, no network.
2. **Peer link.** `openlogi-flow`: discovery, pairing (code confirmation),
   encrypted session, Hello negotiation, device-mapping exchange. Proof:
   `openlogi flow peers` lists the other machine with a verified identity.
3. **Edge handoff, macOS first.** Edge detect → `Handoff` → switch → arrival
   evidence → cursor warp → `HandoffComplete`, with the timeout diagnostics.
   First end-to-end Flow demo.
4. **Keyboard follow + GUI.** Reuse the host-switch link machinery; settings
   page, layout editor, pairing UI.
5. **Clipboard sync.** Text first; files later over dedicated streams
   (capability-gated, no version bump).
6. **Windows / Linux-X11 edge detection**, then **Wayland** via layer-shell or
   InputCapture portal.

## Options+ protocol compatibility

Considered and **rejected** as a goal, for now:

- **Nothing to build against.** The protocol has never been publicly
  documented or reverse-engineered (Solaar #672); it is encrypted, and the
  discovery path is partly cloud-assisted (NodeStore). Interop would begin
  with a from-scratch RE effort of TLS-wrapped traffic between signed
  binaries.
- **A moving target with no contract.** Logitech can and does change the
  protocol with any Options+ release; an interop layer would break silently
  and repeatedly, and each breakage restarts the RE work.
- **Legal and positioning risk.** RE of an encrypted proprietary protocol for
  a shipping competitor-adjacent product is a different legal posture than
  implementing published HID++ specs (which is what the rest of OpenLogi
  does). It also drags OpenLogi toward Logitech's cloud endpoints, against the
  no-account/no-telemetry stance.
- **The payoff is thin.** The only won scenario is a mixed fleet (OpenLogi on
  one machine, Options+ on another). The hardware-level story already works
  there: the device is just paired to both machines, Easy-Switch keys and
  OpenLogi's own host switching keep functioning. Only the automatic
  edge-crossing handshake is lost, and installing OpenLogi on the second
  machine restores it.

Revisit only if the protocol is ever publicly documented or a maintained
third-party implementation appears; the decision then still weighs the
moving-target and positioning costs, not just feasibility.

## Open questions

- Bluetooth-direct devices: the arrival signal equivalent to the receiver's
  `0x41` notification needs verification per transport during milestone 3.
- Three-machine topologies: full mesh of pairwise links, or transitive trust?
  (Leaning full mesh; pairwise pairing is three codes at worst.)
- Clipboard formats beyond text (images, file lists) and their per-OS
  representations — deliberately deferred to milestone 5.
- Where the per-machine private key lives on each OS (keychain/keyring vs a
  mode-0600 file beside the config) — decided in milestone 2.
