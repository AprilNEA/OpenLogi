//! Frontmost backend for GNOME Shell (Wayland and X11), via a small companion
//! GNOME Shell extension that exports the focused window's WM_CLASS over D-Bus
//! and signals every focus change.
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
//! The observer subscribes to the signal and name-owner changes before reading
//! the method snapshot. The method therefore closes startup and extension-
//! restart races without becoming a periodic polling path.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use futures_lite::{StreamExt as _, future};
use tracing::{debug, warn};
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::proxy;

use super::{FrontmostSource, PublishAppId, RECONNECT_DELAY, StopToken, lock_unpoisoned};

/// Cap on every D-Bus call to the extension. Without it, a stalled GNOME Shell
/// could block backend selection or an explicit snapshot read indefinitely.
const METHOD_TIMEOUT: Duration = Duration::from_secs(5);

/// D-Bus proxy for the OpenLogi GNOME Shell extension. The blocking proxy owns
/// idle snapshots; the async proxy lets one worker select focus and owner
/// streams without introducing a timer.
#[proxy(
    interface = "org.openlogi.Frontmost",
    default_service = "org.openlogi.Frontmost",
    default_path = "/org/openlogi/Frontmost",
    gen_blocking = true
)]
trait Frontmost {
    /// WM_CLASS of the focused window, or "" when nothing is focused.
    #[zbus(name = "GetFocusedWmClass")]
    fn get_focused_wm_class(&self) -> zbus::Result<String>;

    /// Emitted whenever GNOME's focused window changes.
    #[zbus(signal, name = "FocusedWmClassChanged")]
    fn focused_wm_class_changed(&self, wm_class: &str) -> zbus::Result<()>;
}

/// Frontmost backend talking to the OpenLogi GNOME Shell extension over the
/// session bus.
struct GnomeShellSource {
    conn: Option<Connection>,
}

impl GnomeShellSource {
    fn connect_bus() -> Option<Connection> {
        Builder::session()
            .map_err(|e| debug!("gnome-shell: no session bus: {e}"))
            .ok()?
            .method_timeout(METHOD_TIMEOUT)
            .build()
            .map_err(|e| debug!("gnome-shell: connection build failed: {e}"))
            .ok()
    }

    fn connect() -> Option<Self> {
        let conn = Self::connect_bus()?;
        // Probe reachability: a successful call (even an empty result) means the
        // OpenLogi extension is installed and exporting the service. An error
        // means it is absent/disabled, so this backend must not be selected.
        Self::snapshot(&conn)
            .map_err(|e| debug!("gnome-shell: OpenLogi extension not reachable: {e}"))
            .ok()?;
        Some(Self { conn: Some(conn) })
    }

    fn snapshot(conn: &Connection) -> zbus::Result<Option<String>> {
        let proxy = FrontmostProxyBlocking::new(conn)?;
        proxy.get_focused_wm_class().map(app_id)
    }

    async fn observe_connection(
        conn: &Connection,
        stop: &StopToken,
        publish: &PublishAppId,
    ) -> ConnectionOutcome {
        let proxy = match FrontmostProxy::new(conn.inner()).await {
            Ok(proxy) => proxy,
            Err(error) => {
                debug!("gnome-shell: async proxy build failed: {error}");
                return ConnectionOutcome::Disconnected;
            }
        };
        // Register both streams before the method read. A focus transition or
        // extension restart is therefore represented by the snapshot, a queued
        // signal/owner event, or both (the publication hub suppresses duplicates).
        let mut focus_changes = match proxy.receive_focused_wm_class_changed().await {
            Ok(changes) => changes,
            Err(error) => {
                debug!("gnome-shell: focus-signal subscription failed: {error}");
                return ConnectionOutcome::Disconnected;
            }
        };
        let mut owner_changes = match proxy.inner().receive_owner_changed().await {
            Ok(changes) => changes,
            Err(error) => {
                debug!("gnome-shell: owner-change subscription failed: {error}");
                return ConnectionOutcome::Disconnected;
            }
        };
        match proxy.get_focused_wm_class().await {
            Ok(wm_class) => publish(app_id(wm_class)),
            Err(error) => {
                debug!("gnome-shell: initial snapshot failed: {error}");
                return ConnectionOutcome::Disconnected;
            }
        }

        loop {
            if stop.is_requested() {
                return ConnectionOutcome::Stopped;
            }
            let event = future::race(
                async { GnomeEvent::Focus(focus_changes.next().await) },
                async { GnomeEvent::Owner(owner_changes.next().await) },
            )
            .await;
            match event {
                GnomeEvent::Focus(Some(signal)) => match signal.args() {
                    Ok(args) => publish(app_id(args.wm_class().to_string())),
                    Err(error) => warn!("gnome-shell: malformed focus signal: {error}"),
                },
                GnomeEvent::Owner(Some(Some(_))) => {
                    // A replacement extension owner may not emit a focus signal
                    // until the next transition, so recover its current snapshot.
                    match proxy.get_focused_wm_class().await {
                        Ok(wm_class) => publish(app_id(wm_class)),
                        Err(error) => debug!("gnome-shell: recovery snapshot failed: {error}"),
                    }
                }
                GnomeEvent::Owner(Some(None)) => publish(None),
                GnomeEvent::Focus(None) | GnomeEvent::Owner(None) => {
                    return if stop.is_requested() {
                        ConnectionOutcome::Stopped
                    } else {
                        ConnectionOutcome::Disconnected
                    };
                }
            }
        }
    }
}

fn app_id(wm_class: String) -> Option<String> {
    (!wm_class.is_empty()).then_some(wm_class)
}

enum GnomeEvent<T, U> {
    Focus(Option<T>),
    Owner(Option<U>),
}

enum ConnectionOutcome {
    Stopped,
    Disconnected,
}

impl FrontmostSource for GnomeShellSource {
    fn frontmost_app_id(&mut self) -> Option<String> {
        if self.conn.is_none() {
            self.conn = Self::connect_bus();
        }
        let result = Self::snapshot(self.conn.as_ref()?)
            .map_err(|e| debug!("gnome-shell: snapshot failed: {e}"));
        if result.is_err() {
            self.conn = None;
        }
        result.ok().flatten()
    }

    fn observe(
        mut self: Box<Self>,
        stop: StopToken,
        publish: PublishAppId,
    ) -> Box<dyn FrontmostSource> {
        // zbus owns its socket reader internally. A helper closes the worker's
        // current connection when teardown is requested, waking both streams
        // so the owning worker can synchronously return and join.
        let active_connection = Arc::new(Mutex::new(None::<Connection>));
        let stop_state = stop.state();
        let connection_to_close = Arc::clone(&active_connection);
        let stopper = thread::Builder::new()
            .name("openlogi-frontmost-gnome-stop".into())
            .spawn(move || {
                stop_state.wait();
                if let Some(conn) = lock_unpoisoned(&connection_to_close).take()
                    && let Err(error) = conn.close()
                {
                    debug!("gnome-shell: failed to close observer connection: {error}");
                }
            })
            .unwrap_or_else(|error| panic!("failed to start GNOME stop helper: {error}"));

        loop {
            if stop.is_requested() {
                break;
            }
            if self.conn.is_none() {
                self.conn = Self::connect_bus();
            }
            let Some(conn) = self.conn.as_ref() else {
                publish(None);
                if stop.wait_timeout(RECONNECT_DELAY) {
                    break;
                }
                continue;
            };
            *lock_unpoisoned(&active_connection) = Some(conn.clone());
            let outcome =
                futures_lite::future::block_on(Self::observe_connection(conn, &stop, &publish));
            lock_unpoisoned(&active_connection).take();
            self.conn = None;

            match outcome {
                ConnectionOutcome::Stopped => break,
                ConnectionOutcome::Disconnected => {
                    publish(None);
                    if stop.wait_timeout(RECONNECT_DELAY) {
                        break;
                    }
                }
            }
        }

        // The helper is awake whenever the loop exits; joining proves no
        // connection clone can be closed after this source returns to idle use.
        if let Err(panic) = stopper.join() {
            warn!("gnome-shell: stop helper panicked: {panic:?}");
        }
        self.conn = None;
        self
    }

    fn name(&self) -> &'static str {
        "gnome-shell"
    }
}

/// Candidate constructor registered in [`super::wayland_candidates`].
pub(super) fn candidate() -> Option<Box<dyn FrontmostSource>> {
    GnomeShellSource::connect().map(|s| Box::new(s) as Box<dyn FrontmostSource>)
}

#[cfg(test)]
mod tests {
    use super::app_id;

    #[test]
    fn empty_wm_class_represents_no_focused_application() {
        assert_eq!(app_id(String::new()), None);
        assert_eq!(
            app_id("org.example.App".into()).as_deref(),
            Some("org.example.App")
        );
    }
}
