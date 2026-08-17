# macOS HID Write Completion Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep a HID++ request alive when macOS reports the known ambiguous write-callback timeout, while preserving errors for definitive and response-less writes.

**Architecture:** Add a typed raw-write error at the `RawHidChannel` boundary. The macOS `async-hid` adapter classifies only `0xE00002D6` as unknown completion; the HID++ request path may then await a matching response until its existing deadline, while all response-less paths continue returning the error.

**Tech Stack:** Rust 2024, `async_trait`, `thiserror`, `futures`, `async-hid`, IOKit-backed HID on macOS.

**Design:** `docs/superpowers/specs/2026-08-17-macos-hid-write-timeout-design.md`

---

## Chunk 1: Typed transport semantics

### Task 1: Preserve response-bearing requests after an unknown write completion

**Files:**
- Modify: `crates/openlogi-hidpp/src/channel/raw.rs`
- Modify: `crates/openlogi-hidpp/src/channel.rs`
- Modify: `crates/openlogi-hidpp/src/channel/tests.rs`
- Modify: `crates/openlogi-hid/src/transport.rs`
- Modify: `crates/openlogi-hid/src/transport/windows.rs`
- Modify: `crates/openlogi-hid/src/scripted_channel.rs`
- Modify: `crates/openlogi-hid/src/pairing/tests.rs`

- [ ] **Step 1: Add failing channel characterization tests**

Extend `MockRawHidChannel` with a one-shot write result selector and add tests equivalent to:

```rust
#[test]
fn unknown_write_completion_can_be_confirmed_by_a_delayed_response() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = HidppChannel::from_raw_channel(raw).await.unwrap();
        let response = short_msg(0x20);
        handle.fail_next_write_with_unknown_completion();

        let send = channel.send_with_timeout(
            short_msg(0x10),
            move |candidate| *candidate == response,
            Duration::from_secs(1),
        );
        let respond = async {
            futures_timer::Delay::new(Duration::from_millis(50)).await;
            handle.send_incoming(response).await;
        };

        let (actual, ()) = futures::join!(send, respond);
        assert_eq!(actual.unwrap(), response);
        assert_pending_empty(&channel);
    });
}

#[test]
fn unknown_write_completion_without_response_reaches_request_timeout() {
    // Configure CompletionUnknown, use a 25 ms deadline, assert Timeout and an
    // empty pending queue.
}

#[test]
fn definitive_write_failure_returns_immediately() {
    // Configure Failed, use a one-second deadline, assert Implementation,
    // elapsed < one second, and an empty pending queue.
}

#[test]
fn response_less_writes_surface_unknown_completion() {
    // Assert both send_and_forget and write_raw_report return Implementation.
}
```

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:

```sh
cargo test -p openlogi-hidpp unknown_write_completion
cargo test -p openlogi-hidpp definitive_write_failure
cargo test -p openlogi-hidpp response_less_writes_surface_unknown_completion
```

Expected: compilation/test failure because the typed write outcome and channel handling do not exist yet.

- [ ] **Step 3: Add the typed raw-write error**

In `channel/raw.rs`, add a public error preserving its source:

```rust
#[derive(Debug, thiserror::Error)]
pub enum RawHidWriteError {
    #[error("the HID report write failed")]
    Failed(#[source] Box<dyn Error + Send + Sync>),
    #[error("the HID report write completion is unknown")]
    CompletionUnknown(#[source] Box<dyn Error + Send + Sync>),
}

impl RawHidWriteError {
    pub fn failed(error: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self::Failed(error.into())
    }

    pub fn completion_unknown(error: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self::CompletionUnknown(error.into())
    }
}
```

Add rustdoc to the enum, variants, and constructors. Change only
`RawHidChannel::write_report` to return `Result<usize, RawHidWriteError>` and
re-export `RawHidWriteError` from `channel.rs`.

- [ ] **Step 4: Adapt all raw-channel implementations**

Update the five implementations found by:

```sh
rg -n 'impl RawHidChannel for' crates
```

All existing errors map to `RawHidWriteError::failed`. Do not classify any
error as unknown in this step. Preserve the Windows native fallback and the
macOS disconnect state transition exactly.

- [ ] **Step 5: Implement the request/response semantics**

Extract the message framing/write portion of `send_and_forget` into a private
helper returning `RawHidWriteError`. In `send_with_timeout`, keep the existing
single overall deadline and use this shape inside it:

```rust
match self.write_message(msg).await {
    Ok(()) => {}
    Err(RawHidWriteError::CompletionUnknown(error)) => {
        trace!(dev, feat, error = %error, "HID write completion unknown; awaiting response");
    }
    Err(error) => return Err(ChannelError::Implementation(Box::new(error))),
}
receiver.await.map_err(|_| ChannelError::NoResponse)
```

`send_and_forget` and `write_raw_report_with_timeout` must map either variant to
`ChannelError::Implementation`. Do not retry writes. Keep final-timeout cleanup
and listener matching unchanged.

- [ ] **Step 6: Run focused HID++ tests**

Run:

```sh
cargo test -p openlogi-hidpp channel::tests
```

Expected: all channel tests pass, including the pre-existing final-timeout and
late-response tests.

- [ ] **Step 7: Run dependent-crate tests**

Run:

```sh
cargo test -p openlogi-hid
```

Expected: all tests pass and every `RawHidChannel` implementation compiles.

- [ ] **Step 8: Commit the typed request semantics**

```sh
git add crates/openlogi-hidpp/src/channel/raw.rs \
  crates/openlogi-hidpp/src/channel.rs \
  crates/openlogi-hidpp/src/channel/tests.rs \
  crates/openlogi-hid/src/transport.rs \
  crates/openlogi-hid/src/transport/windows.rs \
  crates/openlogi-hid/src/scripted_channel.rs \
  crates/openlogi-hid/src/pairing/tests.rs
git commit -m "fix(hidpp): await responses after unknown writes"
```

## Chunk 2: macOS classification and verification

### Task 2: Classify the IOKit callback timeout at the async-hid adapter

**Files:**
- Modify: `crates/openlogi-hid/src/transport.rs`
- Modify: `crates/openlogi-hid/src/transport/tests.rs`

- [ ] **Step 1: Add failing macOS classifier tests**

Add a pure helper test on macOS:

```rust
#[cfg(target_os = "macos")]
#[test]
fn macos_writer_callback_timeout_has_unknown_completion() {
    let error = async_hid::HidError::message(
        "report writer callback error: 0xE00002D6",
    );
    assert!(matches!(
        classify_output_write_error(error),
        RawHidWriteError::CompletionUnknown(_)
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn another_macos_writer_callback_error_is_definitive() {
    let error = async_hid::HidError::message(
        "report writer callback error: 0xE00002C0",
    );
    assert!(matches!(
        classify_output_write_error(error),
        RawHidWriteError::Failed(_)
    ));
}
```

Also assert `HidError::Disconnected` maps to `Failed`.

- [ ] **Step 2: Run the classifier tests and verify they fail**

Run:

```sh
cargo test -p openlogi-hid macos_writer_callback
```

Expected: compilation failure because `classify_output_write_error` does not exist.

- [ ] **Step 3: Implement the exact macOS classifier**

In `transport.rs`, add a macOS-only message constant and a platform-bounded
classifier:

```rust
#[cfg(target_os = "macos")]
const MACOS_WRITE_CALLBACK_TIMEOUT: &str =
    "report writer callback error: 0xE00002D6";

fn classify_output_write_error(error: async_hid::HidError) -> RawHidWriteError {
    #[cfg(target_os = "macos")]
    if matches!(
        &error,
        async_hid::HidError::Message(message)
            if message.as_ref() == MACOS_WRITE_CALLBACK_TIMEOUT
    ) {
        return RawHidWriteError::completion_unknown(error);
    }
    RawHidWriteError::failed(error)
}
```

Call it from `AsyncHidChannel::write_report` after preserving the existing
`Disconnected` handling. Document why exact string matching is necessary with
`async-hid 0.5.2` and why it is confined to macOS.

- [ ] **Step 4: Run focused transport and channel tests**

Run:

```sh
cargo test -p openlogi-hid macos_writer_callback
cargo test -p openlogi-hidpp channel::tests
cargo test -p openlogi-hid
```

Expected: all pass.

- [ ] **Step 5: Commit the adapter classification**

```sh
git add crates/openlogi-hid/src/transport.rs crates/openlogi-hid/src/transport/tests.rs
git commit -m "fix(hid): classify macos callback timeout as unknown"
```

### Task 3: Run the complete software gate

**Files:**
- Verify only; modify code only to fix confirmed gate findings.

- [ ] **Step 1: Format and inspect the diff**

```sh
cargo fmt --all
git diff --check
git diff --stat upstream/master...HEAD
git diff upstream/master...HEAD -- crates/openlogi-hidpp crates/openlogi-hid
```

- [ ] **Step 2: Run every required local gate**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc -p openlogi-hid -p openlogi-hidpp \
  -p openlogi-hidpp-derive --no-deps --document-private-items
```

Expected: all four commands exit zero. If Xcode's Metal cache is sandbox-blocked,
rerun the unchanged command with the required filesystem permission; do not
weaken the gate.

- [ ] **Step 3: Re-fetch and rebase before delivery**

```sh
git fetch upstream master
git rebase upstream/master
```

Rerun the entire four-command gate after any rebase that changes the commit.

## Chunk 3: Exact hardware and public communication

### Task 4: Validate the original MX Keys safely

**Files:**
- No source changes expected.
- Record exact commands, revisions, timings, and outcomes for the PR and issue drafts.

- [ ] **Step 1: Confirm exact hardware and process state**

Use `system_profiler SPBluetoothDataType` to confirm `046d:b35b`, firmware,
battery, and BLE connection. Ensure no official OpenLogi or Logi Options+
process competes for the HID channel.

- [ ] **Step 2: Re-run traced CLI discovery and feature reads**

Run the branch CLI repeatedly with `OPENLOGI_LOG=trace`. Expected: no early
failure at 251 ms; delayed responses match their pending requests; repeated
inventory/features/battery reads are stable.

- [ ] **Step 3: Verify Agent and GUI inventory**

Run the branch Agent and GUI using the development profile. Expected: MX Keys
appears in inventory and GUI while normal typing continues.

- [ ] **Step 4: Perform reversible settings checks**

Read each initial value before writing. Test backlight and Fn-lock only when the
initial value can be read and the same value can be restored. Record every
before/change/restore result.

- [ ] **Step 5: Verify reconnect behavior**

With user coordination, reconnect or power-cycle the keyboard. Confirm stable
rediscovery and any expected volatile-setting reapplication. Do not claim this
step if the hardware action was not performed.

### Task 5: Prepare and publish issue #521 update

**Files:**
- Draft only; no repository file required unless the user requests one.

- [ ] **Step 1: Draft the English issue comment**

Include:

- exact device/OS/release/master/branch revisions;
- measured callback and response timing;
- the distinction between IOKit wait timeout and proven delivery failure;
- the early pending-removal mechanism and GUI consequence;
- the narrow typed fix and raw-write safety boundary;
- exact local gate and hardware outcomes;
- the separate planned `async-hid` follow-up.

- [ ] **Step 2: Present the draft to the user**

Repository policy requires approval before public posting. Do not publish until
the user approves the exact English text.

- [ ] **Step 3: Publish the approved comment**

Use the authenticated GitHub session to post to
`https://github.com/AprilNEA/OpenLogi/issues/521`, then reopen the page and
verify the comment URL and rendered content.

### Task 6: Prepare PR delivery artifacts

**Files:**
- Draft only; no repository file required unless the user requests one.

- [ ] **Step 1: Prepare the PR title and body**

Title:

```text
fix(hid): preserve delayed macOS HID++ responses
```

Body sections must follow repository policy: `## Summary`, `## Changes`,
`## Testing`, and a final `Fixes #521` only if this PR completes the full issue.
If other MX Keys feature PRs remain, use `Part of #521` instead of closing it.

- [ ] **Step 2: Show the exact branch diff and drafts to the user**

Report commit list, changed files, all gate results, hardware limitations, and
the proposed PR title/body. Push or open the PR only after user approval.
