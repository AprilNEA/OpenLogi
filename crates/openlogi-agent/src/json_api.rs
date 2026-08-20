//! The JSON haptics API: a newline-delimited request/response protocol on the
//! agent's second local socket, for third-party apps that want to trigger
//! device haptics.
//!
//! # Why this lives in the agent
//!
//! A relay process was the obvious shape while a *websocket* was on the table —
//! a network listener has no business inside the binary that owns the input
//! hook and holds Accessibility. A local socket has no network reach, so that
//! argument does not transfer: a relay would add a second binary to launch,
//! supervise and version, another hop of latency, and one more way for the
//! feature to be silently dead, in exchange for no boundary that isn't already
//! there. Any local process that can open this socket can already open
//! `agent.sock` and drive the whole agent.
//!
//! What *does* carry over is scope. This endpoint plays waveforms and lists
//! what can be buzzed. It cannot write DPI, pair a device, or read config, so
//! it is strictly weaker than the socket beside it — and it stays that way.
//!
//! # Protocol
//!
//! One JSON object per line in each direction; a response carries back the
//! request's `id` when it had one. Requests on a connection are handled in
//! order, so a client that only ever has one in flight can ignore `id`.
//!
//! ```text
//! -> {"cmd":"hello"}
//! <- {"ok":{"protocol":1,"agent":"0.7.1"}}
//! -> {"id":1,"cmd":"devices"}
//! <- {"id":1,"ok":{"devices":[{"key":"…","name":"MX Master 4"}]}}
//! -> {"id":2,"cmd":"play","waveform":"damp"}
//! <- {"id":2,"ok":{"accepted":true}}
//! ```
//!
//! `accepted` is not `played`: the agent queues the waveform on the same
//! single-flight worker the Actions Ring uses, so a caller cannot saturate the
//! receiver's one in-flight HID++ transaction. See `Agent::play_haptic`.
//!
//! Errors are `{"error":{"code":"…","message":"…"}}`. The codes are
//! `bad_request`, `device_not_found`, `feature_unsupported` and
//! `device_error`; `code` is the stable half, `message` is for humans.

use std::future::Future;
use std::io;

use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::tokio::Listener;
use interprocess::local_socket::tokio::prelude::*;
use openlogi_core::hid::{HapticWaveform, WriteError};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tracing::{info, warn};

use crate::server::AgentServer;

/// Version of the JSON protocol, answered by `hello`.
///
/// Unlike the binary contract next door this one is self-describing, so a
/// client can add fields without a lockstep rebuild; the number moves only if
/// an existing field changes meaning. Additive changes do not bump it.
const PROTOCOL: u32 = 1;

/// Longest accepted request line. A JSON API on a stream needs *some* bound or
/// a client that never sends a newline grows the agent's memory without limit;
/// every legal request here is a few hundred bytes at most.
///
/// Applied per line rather than per connection, so a long-lived client that
/// sends thousands of requests is never cut off for the crime of being useful.
const MAX_LINE: usize = 8 * 1024;

/// One request. `id`, when present, is echoed on the response so a client with
/// several calls in flight can match them up.
#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default)]
    id: Option<u64>,
    #[serde(flatten)]
    command: Command,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    /// Report the protocol version and the agent's own version.
    Hello,
    /// List the online devices that have a haptic engine.
    Devices,
    /// Play one waveform.
    Play {
        #[serde(default)]
        waveform: Waveform,
        /// A `key` from `devices`; omitted means the active device.
        #[serde(default)]
        device: Option<String>,
    },
}

/// The waveform vocabulary as a caller spells it.
///
/// Deliberately its own enum rather than a serde rename on
/// [`HapticWaveform`]: this one is a published API name and the other is an
/// internal wire type, so neither should be able to rename the other by
/// accident.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Waveform {
    /// The light boundary tick.
    #[default]
    Subtle,
    /// The firmer confirmation pulse.
    Damp,
}

impl From<Waveform> for HapticWaveform {
    fn from(waveform: Waveform) -> Self {
        match waveform {
            Waveform::Subtle => Self::SubtleCollision,
            Waveform::Damp => Self::DampStateChange,
        }
    }
}

#[derive(Debug, Serialize)]
struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(flatten)]
    body: Body,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Body {
    Ok(Reply),
    Error(ApiError),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Reply {
    Hello {
        protocol: u32,
        agent: &'static str,
    },
    Devices {
        devices: Vec<DeviceEntry>,
    },
    /// Queued, not necessarily felt — see the module docs.
    Played {
        accepted: bool,
    },
}

/// One buzzable device. `key` is stable across reconnects and reboots (it is
/// the same identifier `config.toml` uses); `name` is for humans only.
#[derive(Debug, Serialize)]
struct DeviceEntry {
    key: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct ApiError {
    code: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }
}

impl From<WriteError> for ApiError {
    fn from(error: WriteError) -> Self {
        let code = match error {
            WriteError::DeviceNotFound => "device_not_found",
            WriteError::FeatureUnsupported { .. } => "feature_unsupported",
            _ => "device_error",
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

/// Serve one request. Infallible by design: every outcome is a response, so a
/// bad line never costs the caller their connection.
async fn handle(server: &AgentServer, command: Command) -> Body {
    match command {
        Command::Hello => Body::Ok(Reply::Hello {
            protocol: PROTOCOL,
            agent: env!("CARGO_PKG_VERSION"),
        }),
        Command::Devices => {
            let devices = server
                .orchestrator
                .lock()
                .await
                .haptic_devices()
                .into_iter()
                .map(|(key, name)| DeviceEntry { key, name })
                .collect();
            Body::Ok(Reply::Devices { devices })
        }
        Command::Play { waveform, device } => {
            let resolved = server
                .orchestrator
                .lock()
                .await
                .haptic_route_for_key(device.as_deref());
            match resolved {
                Ok(route) => {
                    server.ring_haptics.play_external(route, waveform.into());
                    Body::Ok(Reply::Played { accepted: true })
                }
                Err(error) => Body::Error(error.into()),
            }
        }
    }
}

/// Read requests from one client until it disconnects, answering each through
/// `dispatch`.
///
/// A malformed line is answered and the connection continues — a client
/// debugging its JSON should see the complaint, not a closed socket. Only an
/// I/O failure or an over-long line ends the session, the latter because the
/// unread tail of that line would otherwise be parsed as a fresh request.
///
/// Generic over the stream and the dispatcher so the framing rules above can
/// be tested over an in-memory pipe, without an agent behind them: everything
/// this function decides is about bytes, not devices.
async fn serve_connection<S, D, F>(stream: S, mut dispatch: D)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite,
    D: FnMut(Command) -> F,
    F: Future<Output = Body>,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    loop {
        let mut line = Vec::new();
        // Re-`take` each pass so the cap bounds one line, not the connection.
        let count = match (&mut reader)
            .take(MAX_LINE as u64)
            .read_until(b'\n', &mut line)
            .await
        {
            // Clean disconnect.
            Ok(0) => return,
            Ok(count) => count,
            Err(error) => {
                warn!(%error, "haptic API read failed");
                return;
            }
        };
        // A full read with no terminator means the line outgrew the cap; the
        // rest of it is still queued, so there is no honest way to continue.
        if count == MAX_LINE && !line.ends_with(b"\n") {
            let response = Response {
                id: None,
                body: Body::Error(ApiError::bad_request(format!(
                    "request line exceeds {MAX_LINE} bytes"
                ))),
            };
            let _ = respond(&mut write, &response).await;
            return;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }

        let response = match serde_json::from_slice::<Request>(&line) {
            Ok(request) => Response {
                id: request.id,
                body: dispatch(request.command).await,
            },
            Err(error) => Response {
                id: None,
                body: Body::Error(ApiError::bad_request(error.to_string())),
            },
        };
        if respond(&mut write, &response).await.is_err() {
            return;
        }
    }
}

/// Write one response as a single line.
async fn respond<W: tokio::io::AsyncWrite + Unpin>(
    write: &mut W,
    response: &Response,
) -> io::Result<()> {
    let mut encoded = serde_json::to_vec(response).map_err(io::Error::other)?;
    encoded.push(b'\n');
    write.write_all(&encoded).await.inspect_err(|error| {
        warn!(%error, "haptic API write failed");
    })
}

/// Bind the JSON haptics socket and serve it until the process exits.
///
/// Only called when `app_settings.haptic_api` is set; a bind failure disables
/// the API with a warning rather than taking the agent down, since haptics for
/// third-party apps must never be the reason device control stops working.
pub async fn run(server: AgentServer) {
    let listener = match bind() {
        Ok(listener) => listener,
        Err(error) => {
            warn!(%error, "could not bind the JSON haptics socket; API disabled");
            return;
        }
    };
    info!("JSON haptics API listening");

    loop {
        match listener.accept().await {
            Ok(stream) => {
                let server = server.clone();
                tokio::spawn(serve_connection(stream, move |command| {
                    let server = server.clone();
                    async move { handle(&server, command).await }
                }));
            }
            Err(error) => warn!(%error, "haptic API accept failed"),
        }
    }
}

/// Bind the listener, reclaiming a socket left behind by an unclean exit
/// exactly as `openlogi_ipc::transport::bind` does for the agent's own.
///
/// Access matches the endpoint beside it on both platforms: the runtime
/// directory's permissions on Unix, and the default pipe DACL on Windows —
/// which grants the creating user *and administrators*. Both are fine for a
/// single-user desktop; neither isolates users on a shared machine, and
/// tightening either is a hardening point rather than a property to assume.
fn bind() -> io::Result<Listener> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};

        let path = openlogi_core::paths::haptic_socket_path()
            .map_err(|error| io::Error::other(error.to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        ListenerOptions::new()
            .name(path.to_fs_name::<GenericFilePath>()?)
            .try_overwrite(true)
            .create_tokio()
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};

        ListenerOptions::new()
            .name("openlogi-haptic.sock".to_ns_name::<GenericNamespaced>()?)
            .try_overwrite(true)
            .create_tokio()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::{
        ApiError, Body, Command, MAX_LINE, Reply, Request, Response, Waveform, serve_connection,
    };
    use openlogi_core::hid::{HapticWaveform, WriteError};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    fn parse(line: &str) -> Request {
        serde_json::from_str(line).expect("a well-formed request parses")
    }

    fn encode(response: &Response) -> String {
        serde_json::to_string(response).expect("responses encode")
    }

    #[test]
    fn a_play_defaults_to_the_subtle_waveform_on_the_active_device() {
        let request = parse(r#"{"cmd":"play"}"#);
        assert_eq!(request.id, None);
        let Command::Play { waveform, device } = request.command else {
            panic!("expected a play command");
        };
        assert!(device.is_none(), "no device named means the active one");
        assert!(matches!(
            HapticWaveform::from(waveform),
            HapticWaveform::SubtleCollision
        ));
    }

    #[test]
    fn a_play_can_name_its_waveform_and_device() {
        let request = parse(r#"{"id":7,"cmd":"play","waveform":"damp","device":"abc"}"#);
        assert_eq!(request.id, Some(7));
        let Command::Play { waveform, device } = request.command else {
            panic!("expected a play command");
        };
        assert_eq!(device.as_deref(), Some("abc"));
        assert!(matches!(
            HapticWaveform::from(waveform),
            HapticWaveform::DampStateChange
        ));
    }

    /// An unknown waveform must be refused, not silently played as the
    /// default — a caller with a typo would otherwise never learn of it.
    #[test]
    fn an_unknown_waveform_is_rejected() {
        let parsed = serde_json::from_str::<Request>(r#"{"cmd":"play","waveform":"buzz"}"#);
        assert!(parsed.is_err(), "an unknown waveform must not parse");
    }

    #[test]
    fn an_unknown_command_is_rejected() {
        let parsed = serde_json::from_str::<Request>(r#"{"cmd":"reboot"}"#);
        assert!(parsed.is_err(), "an unknown command must not parse");
    }

    #[test]
    fn a_response_carries_the_request_id_back_and_omits_it_otherwise() {
        let with_id = Response {
            id: Some(7),
            body: Body::Ok(Reply::Played { accepted: true }),
        };
        assert_eq!(encode(&with_id), r#"{"id":7,"ok":{"accepted":true}}"#);

        let without = Response {
            id: None,
            body: Body::Ok(Reply::Played { accepted: true }),
        };
        assert_eq!(encode(&without), r#"{"ok":{"accepted":true}}"#);
    }

    /// The `code` is what a client branches on, so each device failure has to
    /// keep its own — collapsing them would make "asleep" and "no haptic
    /// engine" indistinguishable, and only one of those is worth retrying.
    #[test]
    fn device_failures_keep_distinguishable_codes() {
        let not_found = ApiError::from(WriteError::DeviceNotFound);
        assert_eq!(not_found.code, "device_not_found");

        let unsupported = ApiError::from(WriteError::FeatureUnsupported {
            feature_hex: 0x19b0,
        });
        assert_eq!(unsupported.code, "feature_unsupported");

        let other = ApiError::from(WriteError::AgentUnavailable);
        assert_eq!(other.code, "device_error");
    }

    #[test]
    fn a_hello_reply_names_both_versions() {
        let response = Response {
            id: None,
            body: Body::Ok(Reply::Hello {
                protocol: 1,
                agent: "9.9.9",
            }),
        };
        assert_eq!(
            encode(&response),
            r#"{"ok":{"protocol":1,"agent":"9.9.9"}}"#
        );
    }

    /// Drive `serve_connection` over an in-memory pipe with a dispatcher that
    /// answers everything, so what is under test is the framing — not devices.
    fn connect() -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(serve_connection(server, |command| async move {
            match command {
                Command::Hello => Body::Ok(Reply::Hello {
                    protocol: 1,
                    agent: "test",
                }),
                Command::Devices => Body::Ok(Reply::Devices {
                    devices: Vec::new(),
                }),
                Command::Play { .. } => Body::Ok(Reply::Played { accepted: true }),
            }
        }));
        (client, task)
    }

    async fn send(write: &mut (impl tokio::io::AsyncWrite + Unpin), line: &str) {
        write
            .write_all(line.as_bytes())
            .await
            .expect("the connection accepts a request");
    }

    #[tokio::test]
    async fn several_requests_share_one_connection_and_answer_in_order() {
        let (client, _task) = connect();
        let (read, mut write) = tokio::io::split(client);
        let mut lines = BufReader::new(read).lines();

        send(&mut write, "{\"id\":1,\"cmd\":\"hello\"}\n").await;
        send(&mut write, "{\"id\":2,\"cmd\":\"play\"}\n").await;

        let first = lines.next_line().await.expect("read").expect("a response");
        let second = lines.next_line().await.expect("read").expect("a response");
        assert!(first.contains(r#""id":1"#) && first.contains(r#""protocol":1"#));
        assert!(second.contains(r#""id":2"#) && second.contains(r#""accepted":true"#));
    }

    /// The point of answering rather than hanging up: a client debugging its
    /// JSON has to be able to fix the line and try again on the same socket.
    #[tokio::test]
    async fn a_malformed_line_is_answered_and_the_connection_survives() {
        let (client, _task) = connect();
        let (read, mut write) = tokio::io::split(client);
        let mut lines = BufReader::new(read).lines();

        send(&mut write, "not json at all\n").await;
        let complaint = lines.next_line().await.expect("read").expect("a response");
        assert!(
            complaint.contains(r#""code":"bad_request""#),
            "expected a bad_request, got {complaint}"
        );

        send(&mut write, "{\"cmd\":\"hello\"}\n").await;
        let recovered = lines.next_line().await.expect("read").expect("a response");
        assert!(
            recovered.contains(r#""protocol":1"#),
            "the connection must survive a bad line, got {recovered}"
        );
    }

    #[tokio::test]
    async fn blank_lines_are_skipped_rather_than_answered() {
        let (client, _task) = connect();
        let (read, mut write) = tokio::io::split(client);
        let mut lines = BufReader::new(read).lines();

        send(&mut write, "\n   \n{\"cmd\":\"hello\"}\n").await;
        let response = lines.next_line().await.expect("read").expect("a response");
        assert!(
            response.contains(r#""protocol":1"#),
            "blank lines must not produce responses of their own, got {response}"
        );
    }

    /// The cap has to bound one line, not the connection: an earlier version
    /// took it over the whole stream, which silently killed any client after
    /// `MAX_LINE` bytes of perfectly good traffic.
    #[tokio::test]
    async fn the_length_cap_applies_per_line_not_per_connection() {
        let (client, _task) = connect();
        let (read, mut write) = tokio::io::split(client);
        let mut lines = BufReader::new(read).lines();

        let request = "{\"cmd\":\"hello\"}\n";
        let rounds = (MAX_LINE / request.len()) + 2;
        for round in 0..rounds {
            send(&mut write, request).await;
            let response = lines.next_line().await.expect("read").expect("a response");
            assert!(
                response.contains(r#""protocol":1"#),
                "connection died at request {round} of {rounds}: {response}"
            );
        }
    }

    /// An over-long line is refused *and* closes the socket: its unread tail
    /// would otherwise be parsed as the next request.
    #[tokio::test]
    async fn an_over_long_line_is_refused_and_ends_the_connection() {
        let (client, _task) = connect();
        let (read, mut write) = tokio::io::split(client);
        let mut lines = BufReader::new(read).lines();

        let flood = format!(
            "{{\"cmd\":\"hello\",\"pad\":\"{}\"}}",
            "x".repeat(MAX_LINE * 2)
        );
        // The server stops reading mid-flood, so a short write may fail here.
        let _ = write.write_all(flood.as_bytes()).await;

        let complaint = lines.next_line().await.expect("read").expect("a response");
        assert!(
            complaint.contains(r#""code":"bad_request""#) && complaint.contains("exceeds"),
            "expected a length complaint, got {complaint}"
        );
        assert_eq!(
            lines.next_line().await.expect("read"),
            None,
            "the connection must close rather than parse the tail as a request"
        );
    }

    #[tokio::test]
    async fn a_clean_disconnect_ends_the_loop_without_a_response() {
        let (client, task) = connect();
        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("the loop returns when the client goes away")
            .expect("and does not panic");
    }

    #[test]
    fn a_default_waveform_is_the_subtle_one() {
        assert!(matches!(
            HapticWaveform::from(Waveform::default()),
            HapticWaveform::SubtleCollision
        ));
    }
}
