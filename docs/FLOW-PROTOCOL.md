# Flow peer protocol v1 — wire specification

Companion to [FLOW-DESIGN.md](FLOW-DESIGN.md), which records *why* this
protocol looks the way it does (transport choice, encoding choice, BYON,
no-relay). This document is the *what*: the normative contract two OpenLogi
agents implement. The authoritative message schema is
[`flow.v1.proto`](../crates/openlogi-flow/proto/flow.v1.proto); this file
defines everything protobuf cannot express — framing, stream binding, state
machines, canonical byte forms, timeouts, and the evolution rules.

The schema lives under `crates/openlogi-flow/proto/` so code generation and
the wire contract cannot drift apart. This normative specification remains in
`docs/`; edits to either are reviewed against the evolution rules at the
bottom.

## Transport binding

- **QUIC** via `quinn` + `rustls`, mutual authentication with pinned Ed25519
  keys (design doc §Transport). ALPN: **`olf/1`**. The ALPN versions the
  *envelope*, not the message set — it changes only if the frame layout
  itself ever has to change, which the design intends never to happen.
- One connection per peer pair. Either side may initiate; if two connections
  race (simultaneous dial), the side with the lexicographically smaller
  public key closes its *outgoing* connection and keeps the incoming one.
- **Control stream**: the connection initiator opens one bidirectional
  stream immediately; each side writes exactly one `Hello` frame and reads
  the peer's. This stream carries nothing else and stays open for the
  connection's lifetime (its closure is equivalent to connection close).
- **RPC**: one bidirectional stream per request — open, write one request
  frame, read one response, close. The stream is the correlation and the
  cancellation token (reset = cancelled). Responses are the request's paired
  response kind or `Error`.
- **Notifications**: one unidirectional stream per notification, one frame,
  FIN. QUIC makes short streams cheap; one-shot streams avoid inventing an
  ordering relationship between unrelated notices.
- **Bulk data**: rides the same bidirectional stream as the fetch that asked
  for it (`ClipboardData` head, `Chunk`*, `ChunkEnd`).
- **Datagrams**: `Ping`/`Pong` only (RTT + application liveness; QUIC
  keepalive only proves the transport). Datagram loss is acceptable by
  definition; anything that must arrive rides a stream.

| Frame family | Carried on |
|---|---|
| `Hello`, `HelloReject` | control stream |
| `PairStart→Prompted`, `PairConfirm→Outcome`, `GetPeerInfo→PeerInfo`, `HandoffRequest→Accept/Reject`, `ClipboardFetch`/`FileFetch`→`ClipboardData`+`Chunk`*+`ChunkEnd` | one bi-stream per request |
| `PairAbort`, `AnnounceDevices`, `PeerState`, `HandoffResult`, `HandoffCancel`, `ClipboardAnnounce` | one uni-stream each |
| `Ping`, `Pong` | datagrams |
| `Error` | response position on any bi-stream |

## Framing

Every stream payload and datagram is a sequence of frames:

```
┌──────────┬───────────┬──────────┬─────────────────┐
│ kind u16 │ flags u16 │ len u32  │ payload (len B) │
│   LE     │ LE, =0    │   LE     │ protobuf bytes  │
└──────────┴───────────┴──────────┴─────────────────┘
```

- `kind`: a `FrameKind` value. The all-zero header is invalid by
  construction (`Hello` is 0x0001), so a zeroed buffer can never parse as a
  meaningful frame.
- `flags`: reserved, must be sent as 0. A receiver seeing unknown flag bits
  must not guess: drop the frame if it is a notification, answer
  `Error UNSUPPORTED_FLAGS` if it is a request. (Future use: e.g. payload
  compression — a capability would gate *sending* it, the flag would mark
  the frame.)
- `len`: payload byte length. Caps: **1 MiB** general, **64 KiB** for
  `Chunk`. Oversized: close the offending stream, answer
  `Error TOO_LARGE` where a response is expected. The caps bound memory per
  frame; bulk payloads of any size are sequences of capped chunks.
- Unknown `kind`: on a uni-stream, skip `len` bytes and ignore (this is what
  makes new notifications backward-deployable); as a request on a bi-stream,
  answer `Error UNSUPPORTED_KIND`. Decode never desyncs: the envelope always
  says how much to skip.
- Payloads are protobuf (`openlogi.flow.v1`); a payload that fails to decode
  as its kind's message is `Error INVALID` / frame-drop by the same
  notification-vs-request rule.

## Connection lifecycle & negotiation

```
dial ──► TLS (pinned-key verifier) ──► Hello exchange ──► trusted session
                    │                        │
                    │ unknown key            │ version disjoint / key mismatch
                    ▼                        ▼
             untrusted session          HelloReject + close
             (pairing family only)
```

1. TLS: the verifier accepts a pinned peer key → session starts *trusted*;
   accepts an unknown key only while pairing mode is active → session starts
   *untrusted*.
2. Both sides send `Hello`. `agreed = min(proto_max_A, proto_max_B)`; if
   `agreed < max(proto_min_A, proto_min_B)` → `HelloReject VERSION_DISJOINT`
   and close (the GUI names which side is stale). `Hello.public_key` must
   equal the TLS identity → else `HelloReject KEY_MISMATCH`.
3. On an **untrusted** session, only link-family and pairing-family frames
   are legal; anything else is answered `Error NOT_PAIRED` and ignored.
4. On becoming trusted (pinned key, or pairing just completed): each side
   sends `AnnounceDevices` and `PeerState` unsolicited. Reconnect repeats
   them — every state notification is the *whole* current state with a
   revision, so a lost notice heals on the next.

Send-gating: after `Hello`, a peer never sends a frame kind, field, or
semantic introduced later than `agreed`, and never sends a
capability-gated family the peer did not list. Decode tolerance (unknown
kinds/fields skipped) is the safety net, gating is the contract.

## Pairing

Trust ceremony (design doc §Transport chose pinned keys; this is how pins
are created). Runs on an untrusted session:

```
A                                   B
│ ──── PairStart ────────────────► │  B shows prompt: name, code, fingerprint
│ ◄─── PairPrompted{timeout} ───── │  A shows its prompt
│        (users compare the 6-digit code on both screens)
│ ──── PairConfirm ──────────────► │
│ ◄─── PairOutcome{PENDING_LOCAL} ─│  (B's user hasn't confirmed yet)
│ ◄─── PairConfirm ─────────────── │  (B confirms; roles are symmetric)
│ ──── PairOutcome{PAIRED} ──────► │  both persist the peer key; session
│                                  │  becomes trusted in place
```

- Completion condition: a side considers pairing complete when it has both
  **sent and received** a `PairConfirm`; it persists the peer's key and
  answers/expects `PairOutcome{PAIRED}`.
- **Code derivation (SAS)**: both sides compute
  `HKDF-SHA256(ikm = min(pkA,pkB) ‖ max(pkA,pkB), salt = min(nA,nB) ‖ max(nA,nB), info = "openlogi-flow-sas-v1")`,
  take the first 4 output bytes as a big-endian integer, `mod 1_000_000`,
  render as 6 digits. `pk` = the 32-byte Ed25519 keys, `n` = the 16-byte
  `session_nonce`s from the two `Hello`s; `min`/`max` byte-lexicographic. A
  man-in-the-middle necessarily terminates two distinct sessions and shows
  two different codes — the human comparison is the authentication.
- Abort paths: `PairAbort{USER_CANCELLED | CODE_MISMATCH}` from either side;
  `PairOutcome{TIMEOUT}` after `PairPrompted.timeout_ms` (default 120 s);
  `PairOutcome{REJECTED}` if a user declines. All abort paths discard the
  session nonces; retry starts a fresh connection with fresh nonces.

## Device identity

Two `DeviceIdentity` values denote the same physical device iff their
`ids` sets intersect on any entry (kind + canonical bytes equal). Canonical
byte forms:

| `IdentifierKind` | canonical `value` |
|---|---|
| `SERIAL` | the serial string's UTF-8 bytes, no padding/case folding |
| `UNIT_ID` | HID++ 0x0003 unitId, 4 bytes big-endian |
| `BLUETOOTH_ADDRESS` | 6 bytes, transmission order (MSB first) |

A peer announces every identifier it can obtain for a device; correlation
strengthens as transports contribute more identifiers. Bluetooth-direct
devices may expose only `UNIT_ID` + `BLUETOOTH_ADDRESS`; receiver-paired
devices typically expose `SERIAL` + `UNIT_ID`. `name`, `category`, `models`
are presentation/diagnostics and never correlate.

## Handoff

State machine, sender view (receiver mirrors it):

```
IDLE ──edge trigger──► REQUESTED ──Accept──► SWITCHING ──0x41 at receiver──► DONE
                          │                     │    ▲
                          │ Reject/timeout      │    │ HandoffResult
                          ▼                     │    │ (uni, by transfer_id)
                        IDLE ◄── set_current_host failed:
                                 send HandoffCancel{SWITCH_FAILED}
```

- The sender **never** issues `set_current_host` before `HandoffAccept`.
  After issuing it the sender is blind to the device (the radio has left);
  everything after that point is the receiver's to report.
- The receiver arms an arrival watch on `Accept` (HID++ 1.0 connection
  notification sub-ID 0x41, or the equivalent arrival evidence for
  Bluetooth-direct — open question in the design doc) and reports
  `HandoffResult` per device: `ARRIVED`, `PARTIAL`, `TIMEOUT`.
- `transfer_id` is random per attempt and is the idempotency key: a
  duplicate `HandoffRequest` (sender retry after a lost response) is
  answered from the existing transfer's state, never double-armed.
  `ALREADY_PENDING` rejects a *different* transfer while one is armed.
- Multiple devices (keyboard-follow) ride one request; per-device outcomes
  come back in `HandoffResult.arrivals`. Partial arrival is reported, not
  rolled back — the devices that moved are usable, the UI says what stalled.
- Defaults: `arm_timeout_ms` 3000; sender give-up = accept's
  `arm_timeout_ms` + 2000 of network slack. A sender that gives up late
  simply logs a stale `HandoffResult`.
- `EntryPoint.side` is pre-translated to the **receiver's** frame by the
  sender's layout config; `t` is normalized 0..1 along the sender's exiting
  edge; the receiver maps proportionally onto its own exposed edge segments
  (per-display segments, not a union bounding rect) and clamps to the
  nearest valid point.

## Clipboard & files

Lazy, pull-based: bytes move only on paste. Every target OS supports
promised/delayed clipboard rendering (NSPasteboard promises, Windows delayed
rendering, X11/Wayland are lazy by construction).

- On local clipboard change: `ClipboardAnnounce{sequence, formats}` to every
  connected trusted peer that listed `CLIPBOARD_TEXT`. `sequence` is a
  monotonic per-peer generation; announces for stale sequences are ignored,
  fetches quoting a superseded sequence are answered `Error INVALID`.
- On paste at a peer: `ClipboardFetch{sequence, mime, offset}` →
  `ClipboardData` + `Chunk`* + `ChunkEnd`. `offset` resumes an interrupted
  transfer; `ChunkEnd.sha256` (when present) covers the assembled bytes from
  the requested offset.
- Files (`CLIPBOARD_FILES`): the announce lists mime
  `application/x-openlogi-files`; its fetched payload decodes as `FileList`;
  each file then streams via `FileFetch{sequence, file_index, offset}` with
  the identical bulk shape. `relative_path` must be relative, `/`-separated,
  free of `..` — the receiver rejects violations and places files under its
  own download directory.

## Robustness rules

- **Revisions**: every state notification (`PeerInfo`, `AnnounceDevices`,
  `PeerState`) carries a monotonic per-peer `revision`; receivers drop
  anything older than what they have. Notifications are whole-state, never
  deltas — loss heals on the next send, reconnect resends current state.
- **Idempotency**: `transfer_id` dedupes handoffs; `sequence` versions
  clipboard generations; pairing confirms are idempotent (re-received
  `PairConfirm` re-answers current `PairOutcome`).
- **Timeouts** are receiver-declared where one side waits on the other
  (`PairPrompted.timeout_ms`, `HandoffAccept.arm_timeout_ms`) so the two
  sides never disagree about who gave up first.
- **Wall-clock time is diagnostics-only** (`sent_at_ms`, `modified_at_ms`);
  no protocol decision compares clocks across machines. Durations are
  relative milliseconds.
- **Path-agnostic** (BYON): nothing above assumes LAN — no broadcast, no
  MTU assumptions beyond QUIC's own, keepalive tolerant of high-RTT links,
  addresses are hints and identity is only ever the pinned key.
- Three-machine mesh: every exchange is strictly pairwise; `channel_to_me`
  is relative to the announcing peer, so N peers form N·(N−1)/2 independent
  sessions with no shared state.

## Evolution rules (the contract)

1. The envelope layout and the link family (kinds 0x0001–0x000F, `Hello`'s
   existing fields foremost) are **frozen forever**. New ALPN only if the
   envelope itself must change — intended never.
2. protobuf field numbers are never reused or renumbered; removal is
   `reserved N;`. `FrameKind` values are never reused; new kinds go at the
   end of their family range, new families take a fresh 0x10-aligned range.
3. Every enum keeps `*_UNSPECIFIED = 0`; receiving it (or an unknown value)
   where a decision is required is `Error INVALID`, never a silent default.
4. New frame kinds, fields with new *semantics*, and behavioral changes are
   introduced under a version bump (send-gated on `agreed`) or under a
   `Capability` (send-gated on the peer's list). Purely additive
   informational fields may ride the existing version — old peers skip them.
5. `proto_min` rises only as an explicit product decision, never casually.
6. Every kind and field carries a `since:` note in the proto file from v2
   onward; v1 is the baseline.
