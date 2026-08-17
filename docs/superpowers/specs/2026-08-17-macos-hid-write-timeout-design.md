# macOS HID Write Completion Design

## Context

OpenLogi issue #521 tracks Logitech MX Keys support. Exact-device validation on
an MX Keys connected directly over Bluetooth LE (`046d:b35b`) reproduced the
same transport symptom previously observed on an MX Keys Mini:

- `async-hid` asks `IOHIDDeviceSetReportWithCallback` to wait 250 ms;
- macOS completes the callback with `0xE00002D6` (`kIOReturnTimeout`) at about
  251 ms;
- the keyboard's valid HID++ response arrives about 500-685 ms after the write;
- OpenLogi has already treated the callback error as a definitive write failure,
  removed the pending request, and therefore logs the response as unmatched.

The report's delivery is unknown when the callback times out. The timeout proves
that macOS stopped waiting for write completion; it does not prove that the
device did not receive the report. A later matching HID++ response is stronger
evidence that it did.

## Goals

- Preserve a pending HID++ request after the one known macOS callback timeout.
- Accept a later matching response within OpenLogi's existing five-second
  request deadline.
- Keep definitive write failures immediate.
- Keep response-less writes honest: raw lighting writes and `send_and_forget`
  must still report an unknown completion as an error.
- Make the distinction typed at the raw-HID boundary rather than teaching the
  protocol channel about an `async-hid` error string.
- Leave Linux, Windows, IPC, configuration, and HID++ wire formats unchanged.

## Non-goals

- Do not change or patch the `async-hid` dependency in this PR.
- Do not globally extend all HID write callback timeouts.
- Do not retry a report whose delivery is unknown; that could duplicate a
  non-idempotent device operation.
- Do not treat every timeout-looking string as an unknown completion.
- Do not hide the error for operations that have no response to confirm delivery.

An upstream `async-hid` issue/PR is a separate second step after this OpenLogi PR.

## Design

### Typed raw-write failure

`openlogi-hidpp` will define a public `RawHidWriteError` at the
`RawHidChannel` boundary with two variants:

- `Failed`: the transport reports a definitive write failure;
- `CompletionUnknown`: the transport stopped waiting without establishing
  whether the report reached the device.

Both variants retain the original error as their source. `RawHidChannel::write_report`
will return this type. The five in-workspace implementations will map their
existing failures to `Failed` unless the concrete transport can establish the
narrower `CompletionUnknown` meaning.

### macOS transport classification

`AsyncHidChannel` owns the dependency-specific classification. On macOS only,
the exact `async_hid::HidError::Message` value
`report writer callback error: 0xE00002D6` maps to `CompletionUnknown`.
All other `async-hid` errors map to `Failed`; `Disconnected` continues to mark
the channel disconnected.

The exact text comparison is intentionally isolated to this adapter because
`async-hid 0.5.2` exposes the IOKit callback code only through an unstructured
message. Neither `openlogi-hidpp` nor callers will match dependency strings.

### HID++ request flow

`HidppChannel::send_with_timeout` will use a private typed message-write helper
instead of calling the public response-less `send_and_forget` path.

1. Register the pending response matcher.
2. Start the report write under the existing overall request deadline.
3. On success, wait for the matching response.
4. On `CompletionUnknown`, do not retry and do not remove the matcher; continue
   waiting for the matching response.
5. On `Failed`, return immediately and remove only this pending matcher.
6. On the overall deadline, return `ChannelError::Timeout` and remove only this
   pending matcher.

If the response arrived before the macOS callback completed, the oneshot retains
it and the request resolves as soon as the write result is classified.

### Response-less writes

`HidppChannel::send_and_forget` and `write_raw_report_with_timeout` will map
either raw-write error variant to `ChannelError::Implementation`. They have no
matching response that can turn unknown delivery into confirmed delivery.

This is especially important for the 64-byte `0x12` per-key lighting path: an
unknown completion must remain visible rather than being reported as success.

## Error handling and observability

- An unknown completion followed by a matching response is successful.
- An unknown completion without a response ends as the existing
  `ChannelError::Timeout` at the request deadline.
- A definitive write failure remains `ChannelError::Implementation`.
- The unknown-completion branch emits a trace message with the request header and
  source error so future diagnostics show why OpenLogi kept waiting.
- Existing late-response-after-final-timeout behavior remains unchanged: once
  the five-second deadline expires, a later response is unmatched.

## Tests

`openlogi-hidpp` channel tests will cover:

- unknown write completion followed by a delayed matching response succeeds;
- unknown completion without a response reaches the overall timeout and cleans
  the pending matcher;
- definitive write failure returns immediately and cleans the pending matcher;
- `send_and_forget` still returns an unknown-completion error;
- raw report writes still return an unknown-completion error;
- existing final-timeout and late-response tests continue to pass.

`openlogi-hid` transport tests will cover:

- the exact macOS callback timeout message maps to `CompletionUnknown` on macOS;
- another callback code remains `Failed`;
- disconnected errors remain definitive and preserve disconnect handling.

The final commit must pass the repository's full local gate:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc -p openlogi-hid -p openlogi-hidpp \
  -p openlogi-hidpp-derive --no-deps --document-private-items
```

## Exact-device validation

After the software gate, validate on the connected original MX Keys (`046d:b35b`):

1. stable repeated CLI inventory and feature reads;
2. device visible in Agent inventory and GUI;
3. battery read;
4. backlight read, reversible off/on change, and restoration;
5. Fn-lock read, reversible change, and restoration when exposed by the UI/CLI;
6. reconnect or power-cycle discovery and restoration of volatile state;
7. normal keyboard input throughout the test.

No write test runs unless the initial value can be read and restored.

## Issue communication

Before publication, present an English draft for user approval. The issue #521
comment will state:

- exact hardware, transport, OS, release, and master revision tested;
- the measured 251 ms callback timeout and 500-685 ms valid response latency;
- why `kIOReturnTimeout` means unknown completion rather than proven delivery
  failure in this observed request/response flow;
- why OpenLogi removed the matcher too early and omitted the device from GUI
  inventory;
- the narrow OpenLogi fix and its response-less-write safety boundary;
- exact software and hardware validation results;
- that an `async-hid` upstream follow-up will be handled separately.
