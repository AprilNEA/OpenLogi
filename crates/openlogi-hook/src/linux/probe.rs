//! Flow's pointer and display queries on an X11 session. Root-window pixels
//! are the desktop space here.
//!
//! A native Wayland session is refused: XWayland reports the cursor only over
//! X11 windows, and `WarpPointer` there does not move the compositor's cursor.

use std::io;

use tracing::debug;
use x11rb::connection::Connection as _;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, KeyButMask, Window};
use x11rb::rust_connection::RustConnection;

use super::foreground::{SessionKind, detect_session_kind};
use crate::{DesktopPoint, DisplayBounds, KeyModifiers, PointerState, WarpError};

/// The X11 probe: one connection to the display, owned by the probe thread.
pub(crate) struct Probe {
    conn: RustConnection,
    root: Window,
}

impl Probe {
    pub(crate) fn connect() -> Option<Self> {
        if detect_session_kind() == SessionKind::Wayland {
            debug!("Flow pointer probe is unavailable on a Wayland session");
            return None;
        }
        let (conn, screen) = RustConnection::connect(None)
            .map_err(|error| debug!(%error, "X11 is unavailable for the Flow pointer probe"))
            .ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        Some(Self { conn, root })
    }

    pub(crate) fn state(&mut self) -> Option<PointerState> {
        let reply = self
            .conn
            .query_pointer(self.root)
            .ok()?
            .reply()
            .map_err(|error| debug!(%error, "QueryPointer failed"))
            .ok()?;
        Some(PointerState {
            position: DesktopPoint {
                x: f64::from(reply.root_x),
                y: f64::from(reply.root_y),
            },
            modifiers: modifiers(reply.mask),
        })
    }

    pub(crate) fn displays(&mut self) -> Vec<DisplayBounds> {
        match self.monitors() {
            Some(monitors) if !monitors.is_empty() => monitors,
            _ => self.root_bounds().into_iter().collect(),
        }
    }

    /// The whole root window as it is now, for a server whose RandR lists no
    /// monitors. The connection setup's size is a snapshot from connecting,
    /// and a resolution change resizes the root after it.
    fn root_bounds(&self) -> Option<DisplayBounds> {
        let geometry = self
            .conn
            .get_geometry(self.root)
            .ok()?
            .reply()
            .map_err(|error| debug!(%error, "GetGeometry of the root window failed"))
            .ok()?;
        Some(DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: f64::from(geometry.width),
            height: f64::from(geometry.height),
        })
    }

    /// The active monitors from RandR 1.5. `None` when the server lacks the
    /// request.
    fn monitors(&self) -> Option<Vec<DisplayBounds>> {
        let reply = self
            .conn
            .randr_get_monitors(self.root, true)
            .ok()?
            .reply()
            .map_err(|error| debug!(%error, "RandR GetMonitors failed; using the root window"))
            .ok()?;
        Some(
            reply
                .monitors
                .iter()
                .map(|monitor| DisplayBounds {
                    x: f64::from(monitor.x),
                    y: f64::from(monitor.y),
                    width: f64::from(monitor.width),
                    height: f64::from(monitor.height),
                })
                .collect(),
        )
    }

    pub(crate) fn warp(&mut self, to: DesktopPoint) -> Result<(), WarpError> {
        let (x, y) = (root_pixel(to.x), root_pixel(to.y));
        self.conn
            .warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, x, y)
            .map_err(rejected)?;
        self.conn.flush().map_err(rejected)
    }
}

/// Map the X11 modifier state. Mod1 is Alt and Mod4 is Super in the default
/// keymaps; a session that remaps them reports those keys elsewhere.
fn modifiers(mask: KeyButMask) -> KeyModifiers {
    KeyModifiers {
        shift: mask.contains(KeyButMask::SHIFT),
        control: mask.contains(KeyButMask::CONTROL),
        option: mask.contains(KeyButMask::MOD1),
        command: mask.contains(KeyButMask::MOD4),
    }
}

/// The nearest root-window pixel, which X11 carries as an `i16`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the rounded value is clamped to the i16 range before conversion"
)]
fn root_pixel(coordinate: f64) -> i16 {
    coordinate
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

fn rejected(error: x11rb::errors::ConnectionError) -> WarpError {
    WarpError::Rejected(io::Error::other(error))
}

#[cfg(test)]
mod tests {
    use x11rb::protocol::xproto::KeyButMask;

    use super::{modifiers, root_pixel};
    use crate::KeyModifiers;

    #[test]
    fn alt_and_super_map_to_option_and_command() {
        assert_eq!(
            modifiers(KeyButMask::MOD1 | KeyButMask::MOD4 | KeyButMask::BUTTON1),
            KeyModifiers {
                shift: false,
                control: false,
                option: true,
                command: true,
            }
        );
        assert!(modifiers(KeyButMask::LOCK | KeyButMask::MOD2).is_empty());
    }

    #[test]
    fn root_pixels_round_and_saturate() {
        assert_eq!(root_pixel(959.5), 960);
        assert_eq!(root_pixel(-0.4), 0);
        assert_eq!(root_pixel(1e9), i16::MAX);
        assert_eq!(root_pixel(-1e9), i16::MIN);
    }
}
