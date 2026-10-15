//! The X11 frontmost source: `_NET_ACTIVE_WINDOW` on the root window, then
//! the focused window's `WM_CLASS`. It is the whole answer on an X11 session
//! and the XWayland fallback on every other.

use std::os::fd::AsRawFd;

use tracing::debug;
use x11rb::connection::Connection as _;
use x11rb::properties::WmClass;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, Window,
};
use x11rb::rust_connection::RustConnection;

use super::{
    FrontmostSource, PollResult, PublishAppId, RECONNECT_DELAY, StopToken, poll_source_or_stop,
};

/// The WM_CLASS class component is the shared foreground/pointer profile ID.
pub(in crate::linux) fn window_app_id(conn: &RustConnection, window: Window) -> Option<String> {
    let wm = WmClass::get(conn, window).ok()?.reply_unchecked().ok()??;
    std::str::from_utf8(wm.class())
        .ok()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Frontmost backend backed by X11 `_NET_ACTIVE_WINDOW` + `WM_CLASS`.
///
/// Works on an X11 session, and on a Wayland session for XWayland windows;
/// native Wayland windows are invisible through this path and yield `None`.
pub(in crate::linux) struct X11Source {
    pub(in crate::linux) conn: RustConnection,
    pub(in crate::linux) root: Window,
    net_active_window: Atom,
}

impl X11Source {
    /// Connect to the X server and resolve the `_NET_ACTIVE_WINDOW` atom.
    /// Returns `None` when no X display is reachable (a Wayland session without
    /// XWayland, or `$DISPLAY` unset).
    pub(in crate::linux) fn connect() -> Option<Self> {
        let (conn, screen_num) = RustConnection::connect(None)
            .map_err(|e| debug!("X11 not available, frontmost will return None: {e}"))
            .ok()?;
        let root = conn.setup().roots[screen_num].root;
        let net_active_window = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        Some(Self {
            conn,
            root,
            net_active_window,
        })
    }

    fn subscribe(&self) -> bool {
        self.set_event_mask(EventMask::PROPERTY_CHANGE)
            .map_err(|e| debug!("frontmost: failed to subscribe to X11 root changes: {e}"))
            .is_ok()
    }

    fn unsubscribe(&self) {
        if let Err(error) = self.set_event_mask(EventMask::NO_EVENT) {
            debug!("frontmost: failed to unsubscribe from X11 root changes: {error}");
        }
    }

    fn set_event_mask(&self, event_mask: EventMask) -> Result<(), String> {
        let cookie = self
            .conn
            .change_window_attributes(
                self.root,
                &ChangeWindowAttributesAux::new().event_mask(event_mask),
            )
            .map_err(|error| error.to_string())?;
        cookie.check().map_err(|error| error.to_string())?;
        self.conn.flush().map_err(|error| error.to_string())
    }

    fn run_until_stopped_or_disconnected(
        &mut self,
        stop: &StopToken,
        publish: &PublishAppId,
    ) -> bool {
        if !self.subscribe() {
            return false;
        }

        // The event mask is installed before this read, so a focus change can
        // only be represented by the snapshot, a queued PropertyNotify, or both.
        publish(self.frontmost_app_id());

        loop {
            match poll_source_or_stop(Some(self.conn.stream().as_raw_fd()), stop.wake_fd(), None) {
                PollResult::StopRequested => {
                    self.unsubscribe();
                    return true;
                }
                PollResult::SourceReady => {}
                PollResult::DeadlineReached => continue,
                PollResult::Error => return false,
            }

            let mut active_window_changed = false;
            loop {
                match self.conn.poll_for_event() {
                    Ok(Some(Event::PropertyNotify(event)))
                        if event.window == self.root && event.atom == self.net_active_window =>
                    {
                        active_window_changed = true;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        debug!("frontmost: X11 connection lost: {error}");
                        return false;
                    }
                }
            }
            if active_window_changed {
                publish(self.frontmost_app_id());
            }
        }
    }
}

impl FrontmostSource for X11Source {
    fn frontmost_app_id(&mut self) -> Option<String> {
        // _NET_ACTIVE_WINDOW on the root window holds the focused window's XID.
        let window: Window = self
            .conn
            .get_property(
                false,
                self.root,
                self.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()?;
        if window == 0 {
            return None;
        }

        window_app_id(&self.conn, window)
    }

    fn observe(
        mut self: Box<Self>,
        stop: StopToken,
        publish: PublishAppId,
    ) -> Box<dyn FrontmostSource> {
        loop {
            if self.run_until_stopped_or_disconnected(&stop, &publish) {
                return self;
            }
            publish(None);
            if stop.wait_timeout(RECONNECT_DELAY) {
                return self;
            }
            if let Some(reconnected) = Self::connect() {
                debug!("frontmost: X11 connection restored");
                *self = reconnected;
            }
        }
    }

    fn name(&self) -> &'static str {
        "x11"
    }
}
