//! Frontmost backend using the wlroots `zwlr_foreign_toplevel_management_v1`
//! protocol.
//!
//! The manager hands out one handle per toplevel window; each handle reports
//! its `app_id` and a `state` set. The frontmost window is the toplevel whose
//! state set contains `activated`, and its `app_id` is what we return.
//!
//! Note on the returned identifier: this is the xdg-shell `app_id` (e.g.
//! "org.mozilla.firefox", "Alacritty", "foot"), which is a *different namespace*
//! from the `WM_CLASS` returned by the X11 and gnome-shell backends (e.g.
//! "Firefox"). Because profile lookup is an exact match, a per-app profile
//! created under wlroots will not match under GNOME/X11 and vice versa. We
//! deliberately return the native `app_id` rather than a lossy WM_CLASS
//! approximation (stripping reverse-DNS and capitalizing guesses wrong for many
//! apps); reconciling the two namespaces belongs in a single normalization
//! layer, not here. See the `FrontmostSource` trait doc in `linux.rs`.
//!
//! This protocol is implemented by wlroots-based compositors (sway, Hyprland,
//! river, Wayfire, …). GNOME (Mutter) and KDE (KWin) do not advertise it, so
//! [`connect`](WlrForeignToplevelSource::connect) returns `None` there and the
//! caller falls through to the next backend candidate.
//!
//! The protocol is event-driven, but the [`super::FrontmostSource`] contract is
//! a synchronous poll (~1 Hz from `openlogi-gui::app_watcher`). To bridge that,
//! the connection, event queue, and accumulated state are kept alive in the
//! backend and a `roundtrip` is performed on each poll to drain pending events
//! (focus changes, new / closed windows) before reading the active toplevel.

use std::collections::HashMap;
use std::sync::Mutex;

use tracing::{debug, info, warn};
use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    self, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    self, ZwlrForeignToplevelManagerV1,
};

use super::FrontmostSource;

/// Highest protocol version this backend understands. The events it relies on
/// (`app_id`, `state`, `done`, `closed`) exist since v1, so binding is capped
/// here to stay within what `wayland-protocols-wlr` generates.
const MANAGER_MAX_VERSION: u32 = 3;

/// Accumulated per-toplevel data. wlr sends individual property events and then
/// a `done` marking a consistent snapshot, so updates are staged in `pending_*`
/// and committed on `done`.
#[derive(Default)]
struct Toplevel {
    app_id: Option<String>,
    activated: bool,
    pending_app_id: Option<String>,
    pending_activated: bool,
}

/// Dispatch state: the bound manager plus the toplevels seen so far.
#[derive(Default)]
struct State {
    manager: Option<ZwlrForeignToplevelManagerV1>,
    toplevels: HashMap<ObjectId, Toplevel>,
    /// Set when the compositor sends `finished`; triggers a reconnect on the
    /// next poll instead of permanently disabling the backend.
    finished: bool,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == ZwlrForeignToplevelManagerV1::interface().name {
                let version = version.min(MANAGER_MAX_VERSION);
                let manager =
                    registry.bind::<ZwlrForeignToplevelManagerV1, (), Self>(name, version, qh, ());
                state.manager = Some(manager);
            }
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                state.toplevels.insert(toplevel.id(), Toplevel::default());
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                // The compositor is reloading or restarting. Mark the session
                // finished; the next poll will reconnect automatically.
                warn!(
                    "wlr-foreign-toplevel: compositor sent Finished — \
                     will reconnect on next poll"
                );
                state.finished = true;
                state.manager = None;
            }
            _ => {}
        }
    }

    // The `toplevel` event creates a new handle object; tell the backend to
    // route its events to this same `State` with `()` user data.
    event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;

        let id = handle.id();
        match event {
            Event::AppId { app_id } => {
                if let Some(toplevel) = state.toplevels.get_mut(&id) {
                    toplevel.pending_app_id = Some(app_id);
                }
            }
            Event::State { state: states } => {
                let activated = is_activated(&states);
                if let Some(toplevel) = state.toplevels.get_mut(&id) {
                    toplevel.pending_activated = activated;
                }
            }
            Event::Done => {
                if let Some(toplevel) = state.toplevels.get_mut(&id) {
                    // app_id is sent only when it changes, and a compositor may
                    // emit State + Done before the first AppId. Committing
                    // `pending_app_id` unconditionally would clobber a known id
                    // (or the initial one) with None, so only overwrite when a
                    // value is actually pending. `activated` defaults to false,
                    // which is the correct state for a window that sent none.
                    if toplevel.pending_app_id.is_some() {
                        toplevel.app_id = toplevel.pending_app_id.clone();
                    }
                    toplevel.activated = toplevel.pending_activated;
                }
            }
            Event::Closed => {
                state.toplevels.remove(&id);
                handle.destroy();
            }
            // Title, output enter/leave, and parent are not needed for frontmost.
            _ => {}
        }
    }
}

/// The `state` event carries a `wl_array` of native-endian `u32` state values.
/// A toplevel is frontmost iff the `activated` value is present in that set.
fn is_activated(states: &[u8]) -> bool {
    use zwlr_foreign_toplevel_handle_v1::State;

    states.chunks_exact(4).any(|chunk| {
        let value = u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        State::try_from(value).is_ok_and(|s| s == State::Activated)
    })
}

/// One live Wayland session: connection + event queue + dispatch state.
///
/// Grouping all three behind a single mutex means the whole session can be
/// dropped and rebuilt atomically when the compositor sends `Finished`.
struct Session {
    // Held for RAII — even though `Connection` is Arc-backed, keeping an
    // explicit handle here ensures the connection outlives the queue.
    _conn: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl Session {
    /// Open a fresh connection, bind the manager, and do the initial two
    /// round-trips to populate the toplevel list. Returns `None` when the
    /// compositor doesn't advertise the protocol or the connection fails.
    fn open() -> Option<Self> {
        let conn = Connection::connect_to_env()
            .map_err(|e| debug!("wlr-foreign-toplevel: no Wayland connection: {e}"))
            .ok()?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();

        // Registering the registry triggers `global` events on the next
        // round-trip, where the manager is bound if the compositor advertises
        // it. The registry handle is only needed for that round-trip.
        let _registry = conn.display().get_registry(&qh, ());
        let mut state = State::default();
        queue
            .roundtrip(&mut state)
            .map_err(|e| debug!("wlr-foreign-toplevel: registry roundtrip failed: {e}"))
            .ok()?;
        if state.manager.is_none() {
            debug!("wlr-foreign-toplevel: compositor does not advertise the protocol");
            return None;
        }

        // Second round-trip: receive the initial toplevel list and properties,
        // so the first poll already has the active window.
        queue
            .roundtrip(&mut state)
            .map_err(|e| debug!("wlr-foreign-toplevel: initial roundtrip failed: {e}"))
            .ok()?;

        Some(Self {
            _conn: conn,
            queue,
            state,
        })
    }
}

/// Wayland frontmost backend. Holds the session behind a mutex so the whole
/// connection can be rebuilt on compositor restart without touching callers.
struct WlrForeignToplevelSource {
    // Active session, or `None` when the last reconnect attempt failed.
    // The mutex bridges the event-driven Wayland runtime to the synchronous
    // poll contract; the session is only ever touched here, at ~1 Hz.
    session: Mutex<Option<Session>>,
}

impl WlrForeignToplevelSource {
    fn connect() -> Option<Self> {
        Session::open().map(|s| Self {
            session: Mutex::new(Some(s)),
        })
    }
}

impl FrontmostSource for WlrForeignToplevelSource {
    fn frontmost_bundle_id(&self) -> Option<String> {
        let mut guard = self.session.lock().ok()?;

        // Reconnect when the compositor sent `Finished` (compositor reload /
        // restart) or when a prior reconnect attempt failed.
        let needs_reconnect = guard.as_ref().map_or(true, |s| s.state.finished);
        if needs_reconnect {
            *guard = Session::open();
            match &*guard {
                Some(_) => info!("wlr-foreign-toplevel: reconnected"),
                None => debug!("wlr-foreign-toplevel: reconnect pending, retrying next poll"),
            }
        }

        let Session { queue, state, .. } = guard.as_mut()?;
        queue.roundtrip(state).ok()?;
        if state.finished {
            // `Finished` arrived during this poll; reconnect on the next call.
            return None;
        }

        state
            .toplevels
            .values()
            .find(|toplevel| toplevel.activated)
            .and_then(|toplevel| toplevel.app_id.clone())
    }

    fn name(&self) -> &'static str {
        "wlr-foreign-toplevel"
    }
}

/// Candidate constructor registered in [`super::wayland_candidates`].
pub(super) fn candidate() -> Option<Box<dyn FrontmostSource>> {
    WlrForeignToplevelSource::connect().map(|s| Box::new(s) as Box<dyn FrontmostSource>)
}
