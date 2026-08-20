//! A minimal third-party client for the agent's `play_haptic` RPC: connect,
//! check the protocol, buzz a device.
//!
//! This is the reference for integrating your own app against a running
//! OpenLogi agent, and doubles as the manual test for the RPC — there is no
//! `openlogi haptic` subcommand, because `openlogi-cli` is published to
//! crates.io and `openlogi-ipc` deliberately is not.
//!
//! ```sh
//! cargo run -p openlogi-ipc --example haptic              # subtle, active device
//! cargo run -p openlogi-ipc --example haptic -- damp      # the firmer pulse
//! ```
//!
//! The agent must already be running (the GUI starts it; `cargo run -p
//! openlogi-agent` does too). Everything here goes over the same local socket
//! the GUI uses — no network, no port, no listener.

#![expect(
    clippy::expect_used,
    reason = "a hand-run example reports failures by panicking"
)]

use openlogi_core::hid::HapticWaveform;
use openlogi_ipc::{PROTOCOL_VERSION, client};
use tarpc::context;

#[tokio::main]
async fn main() {
    let waveform = match std::env::args().nth(1).as_deref() {
        None | Some("subtle") => HapticWaveform::SubtleCollision,
        Some("damp") => HapticWaveform::DampStateChange,
        Some(other) => {
            eprintln!("unknown waveform `{other}` — expected `subtle` or `damp`");
            return;
        }
    };

    let connection = client::connect()
        .await
        .expect("the agent is running and answering on its local socket");

    // The wire format is positional, so a skew is not something to work
    // around — a real client should report it and stop, exactly like this.
    let agent_version = connection.version;
    assert_eq!(
        agent_version, PROTOCOL_VERSION,
        "protocol skew: the agent speaks v{agent_version}, this client speaks v{PROTOCOL_VERSION}"
    );

    // `None` = whichever device the agent considers active. Pass
    // `Some(route)` — a `DeviceRoute` from `inventory()` — to target one
    // explicitly.
    match connection
        .client
        .play_haptic(context::current(), None, waveform)
        .await
        .expect("the agent answered the call")
    {
        // Accepted, not played: the agent queues the waveform on its
        // single-flight worker so callers cannot saturate the HID++ channel.
        Ok(()) => println!("{waveform:?} accepted"),
        Err(error) => println!("the agent refused it: {error}"),
    }
}
