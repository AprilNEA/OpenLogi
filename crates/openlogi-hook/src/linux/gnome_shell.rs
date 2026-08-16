//! Frontmost backend for GNOME Shell (Wayland and X11), via a small companion
//! GNOME Shell extension that exports the focused window's WM_CLASS over D-Bus.
//!
//! GNOME (Mutter) implements neither wlr-foreign-toplevel nor any portal for
//! the focused window, and `org.gnome.Shell.Eval` is disabled by default, so a
//! privileged GNOME Shell extension is the only way to read the focused window
//! on a GNOME Wayland session. The extension lives in `gnome-shell-extension/`
//! in this crate and must be installed and enabled for this backend to
//! activate. When it is absent, [`GnomeShellSource::connect`] fails and backend
//! selection falls through to the next candidate (XWayland via X11).
//!
//! The extension returns the WM_CLASS — not the `.desktop` id — so the
//! identifier matches the X11 backend's, keeping per-app profile keys
//! consistent across X11, XWayland, and GNOME Wayland sessions.
//!
//! Only the session-bus connection is held in the backend; a lightweight proxy
//! is built per poll (no extra D-Bus traffic beyond the method call itself).

use std::time::Duration;

use tracing::debug;
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::proxy;

use super::FrontmostSource;

/// Cap on every D-Bus call to the extension. Without it, a stalled GNOME Shell
/// would block the polling thread forever (the probe runs inside the
/// `FRONTMOST_SOURCE` initializer, so a stall there would block every thread
/// that touches it).
const METHOD_TIMEOUT: Duration = Duration::from_secs(5);

/// D-Bus proxy for the OpenLogi GNOME Shell extension. Only the blocking proxy
/// is generated (`gen_async = false`), matching the synchronous poll contract.
#[proxy(
    interface = "org.openlogi.Frontmost",
    default_service = "org.openlogi.Frontmost",
    default_path = "/org/openlogi/Frontmost",
    gen_async = false
)]
trait Frontmost {
    /// WM_CLASS of the focused window, or "" when nothing is focused.
    #[zbus(name = "GetFocusedWmClass")]
    fn get_focused_wm_class(&self) -> zbus::Result<String>;
}

/// Frontmost backend talking to the OpenLogi GNOME Shell extension over the
/// session bus.
struct GnomeShellSource {
    conn: Connection,
}

impl GnomeShellSource {
    fn connect() -> Option<Self> {
        let conn = Builder::session()
            .map_err(|e| debug!("gnome-shell: no session bus: {e}"))
            .ok()?
            .method_timeout(METHOD_TIMEOUT)
            .build()
            .map_err(|e| debug!("gnome-shell: connection build failed: {e}"))
            .ok()?;
        // Probe reachability: a successful call (even an empty result) means the
        // OpenLogi extension is installed and exporting the service. An error
        // means it is absent/disabled, so this backend must not be selected.
        let proxy = FrontmostProxy::new(&conn)
            .map_err(|e| debug!("gnome-shell: proxy build failed: {e}"))
            .ok()?;
        proxy
            .get_focused_wm_class()
            .map_err(|e| debug!("gnome-shell: OpenLogi extension not reachable: {e}"))
            .ok()?;
        Some(Self { conn })
    }
}

impl FrontmostSource for GnomeShellSource {
    fn frontmost_bundle_id(&self) -> Option<String> {
        let proxy = FrontmostProxy::new(&self.conn)
            .map_err(|e| debug!("gnome-shell: proxy build failed: {e}"))
            .ok()?;
        let wm_class = proxy
            .get_focused_wm_class()
            .map_err(|e| debug!("gnome-shell: poll failed (extension gone or bus down?): {e}"))
            .ok()?;
        (!wm_class.is_empty()).then_some(wm_class)
    }

    fn name(&self) -> &'static str {
        "gnome-shell"
    }
}

/// Candidate constructor registered in [`super::wayland_candidates`].
pub(super) fn candidate() -> Option<Box<dyn FrontmostSource>> {
    GnomeShellSource::connect().map(|s| Box::new(s) as Box<dyn FrontmostSource>)
}
