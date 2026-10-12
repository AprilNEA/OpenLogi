//! niri supplies an initial window snapshot and ordered focus updates on one
//! event stream. Read only identity/focus fields so newer unrelated events and
//! older window payloads do not break per-application profiles.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::debug;

use super::{
    FrontmostSource, PollResult, PublishAppId, RECONNECT_DELAY, StopToken, poll_source_or_stop,
};

const REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Serialize)]
enum Request {
    EventStream,
    FocusedWindow,
}

#[derive(Debug, Deserialize)]
struct Window {
    id: u64,
    app_id: Option<String>,
    is_focused: bool,
}

#[derive(Debug, Deserialize)]
enum Response {
    Handled,
    FocusedWindow(Option<Window>),
}

#[derive(Debug, Deserialize)]
enum Event {
    WindowsChanged {
        windows: Vec<Window>,
    },
    WindowOpenedOrChanged {
        window: Window,
    },
    WindowClosed {
        id: u64,
    },
    WindowFocusChanged {
        id: Option<u64>,
    },
    #[serde(skip)]
    Other,
}

fn parse_event(bytes: &[u8]) -> io::Result<Event> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let object = value
        .as_object()
        .filter(|object| object.len() == 1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid niri event envelope"))?;
    if object.keys().any(|key| {
        matches!(
            key.as_str(),
            "WindowsChanged" | "WindowOpenedOrChanged" | "WindowClosed" | "WindowFocusChanged"
        )
    }) {
        Ok(serde_json::from_value(value)?)
    } else {
        Ok(Event::Other)
    }
}

#[derive(Default)]
struct Windows {
    windows: HashMap<u64, Window>,
}

impl Windows {
    fn apply(&mut self, event: Event) {
        match event {
            Event::WindowsChanged { windows } => {
                self.windows = windows
                    .into_iter()
                    .map(|window| (window.id, window))
                    .collect();
            }
            Event::WindowOpenedOrChanged { window } => {
                if window.is_focused {
                    for other in self.windows.values_mut() {
                        other.is_focused = false;
                    }
                }
                self.windows.insert(window.id, window);
            }
            Event::WindowClosed { id } => {
                self.windows.remove(&id);
            }
            Event::WindowFocusChanged { id } => {
                for window in self.windows.values_mut() {
                    window.is_focused = Some(window.id) == id;
                }
            }
            Event::Other => {}
        }
    }

    fn app_id(&self) -> Option<String> {
        self.windows
            .values()
            .find(|window| window.is_focused)
            .and_then(|window| window.app_id.clone())
            .filter(|id| !id.is_empty())
    }
}

struct Session {
    reader: BufReader<UnixStream>,
    pending: Vec<u8>,
}

impl Session {
    fn new(stream: UnixStream) -> io::Result<Self> {
        stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        Ok(Self {
            reader: BufReader::new(stream),
            pending: Vec::new(),
        })
    }

    fn request(&mut self, request: &Request) -> io::Result<Response> {
        let mut bytes = serde_json::to_vec(request)?;
        bytes.push(b'\n');
        self.reader.get_mut().write_all(&bytes)?;
        self.read_line()?;
        let response = serde_json::from_slice::<Result<Response, String>>(&self.pending)?;
        self.pending.clear();
        response.map_err(io::Error::other)
    }

    fn read_line(&mut self) -> io::Result<()> {
        let limit = MAX_FRAME_BYTES.saturating_sub(self.pending.len());
        let count =
            Read::take(&mut self.reader, limit as u64).read_until(b'\n', &mut self.pending)?;
        if count == 0 || self.pending.last() != Some(&b'\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete or oversized niri frame",
            ));
        }
        Ok(())
    }

    fn observe(&mut self, stop: &StopToken, publish: &PublishAppId) -> io::Result<()> {
        if !matches!(self.request(&Request::EventStream)?, Response::Handled) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected event-stream reply",
            ));
        }
        self.reader.get_mut().set_nonblocking(true)?;
        let mut windows = Windows::default();
        // The first WindowsChanged is the authoritative snapshot. In contrast
        // to a separate FocusedWindow query it cannot race subscription.
        while !stop.is_requested() {
            match self.read_line() {
                Ok(()) => {
                    let event = parse_event(&self.pending)?;
                    self.pending.clear();
                    if !matches!(event, Event::Other) {
                        windows.apply(event);
                        publish(windows.app_id());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if self.pending.len() >= MAX_FRAME_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "oversized niri event",
                        ));
                    }
                    match poll_source_or_stop(
                        Some(self.reader.get_ref().as_raw_fd()),
                        stop.wake_fd(),
                        None,
                    ) {
                        PollResult::StopRequested => break,
                        PollResult::SourceReady => {}
                        PollResult::Error | PollResult::DeadlineReached => {
                            return Err(io::Error::other("niri event poll failed"));
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

struct NiriSource {
    path: PathBuf,
}

impl FrontmostSource for NiriSource {
    fn frontmost_app_id(&mut self) -> Option<String> {
        let result = UnixStream::connect(&self.path)
            .and_then(Session::new)
            .and_then(|mut session| session.request(&Request::FocusedWindow));
        match result {
            Ok(Response::FocusedWindow(window)) => window
                .and_then(|window| window.app_id)
                .filter(|id| !id.is_empty()),
            Ok(Response::Handled) => None,
            Err(error) => {
                debug!(%error, "niri: foreground snapshot failed");
                None
            }
        }
    }

    fn observe(
        self: Box<Self>,
        stop: StopToken,
        publish: PublishAppId,
    ) -> Box<dyn FrontmostSource> {
        while !stop.is_requested() {
            let result = UnixStream::connect(&self.path)
                .and_then(Session::new)
                .and_then(|mut session| session.observe(&stop, &publish));
            if stop.is_requested() {
                break;
            }
            if let Err(error) = result {
                debug!(%error, "niri: foreground stream disconnected");
            }
            publish(None);
            // The next subscription replaces all old window state with niri's
            // initial snapshot. Never retain a profile from a lost connection.
            if stop.wait_timeout(RECONNECT_DELAY) {
                break;
            }
        }
        self
    }

    fn name(&self) -> &'static str {
        "niri"
    }
}

pub(super) fn candidate() -> Option<Box<dyn FrontmostSource>> {
    let path = std::env::var_os("NIRI_SOCKET").filter(|path| !path.is_empty())?;
    // Keep niri selected during temporary socket loss; its reconnect loop can
    // recover, whereas selecting XWayland would miss native windows forever.
    Some(Box::new(NiriSource { path: path.into() }))
}

#[cfg(test)]
mod tests;
