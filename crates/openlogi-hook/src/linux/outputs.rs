//! The screen layout of a Wayland session, in the compositor's logical
//! coordinates.
//!
//! `xdg-output` reports where each output sits in the space the compositor
//! places windows and the cursor in. A KWin script's `screen.geometry` and
//! `workspace.cursorPos` use the same space, which is why a position the
//! script reports can be matched against this layout.
//!
//! The layout is read on demand over a short-lived connection: one bind of
//! every output and of the `xdg-output` manager, then one round trip to
//! collect the answers. Nothing here keeps a connection open. The read runs on
//! a helper thread so that a compositor that stops answering costs the caller
//! [`TIMEOUT`], not its own life.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tracing::debug;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::{
    self, ZxdgOutputManagerV1,
};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};

use crate::DisplayBounds;

/// How long a read of the layout may take before the caller gives up.
const TIMEOUT: Duration = Duration::from_secs(2);

/// The logical position and size of one output as the compositor last sent it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Logical {
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
}

impl Logical {
    /// The output's bounds, once both its position and its size are known and
    /// the size is positive.
    fn bounds(self) -> Option<DisplayBounds> {
        let ((x, y), (width, height)) = self.position.zip(self.size)?;
        (width > 0 && height > 0).then_some(DisplayBounds {
            x: f64::from(x),
            y: f64::from(y),
            width: f64::from(width),
            height: f64::from(height),
        })
    }
}

#[derive(Default)]
struct State {
    manager: Option<ZxdgOutputManagerV1>,
    outputs: Vec<WlOutput>,
    /// One entry per bound output, in the order of `outputs`.
    logical: Vec<Logical>,
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
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        if interface == "wl_output" {
            let index = state.outputs.len();
            state
                .outputs
                .push(registry.bind::<WlOutput, _, _>(name, version.min(4), qh, index));
            state.logical.push(Logical::default());
        } else if interface == "zxdg_output_manager_v1" {
            // Version 1 carries everything used here.
            state.manager = Some(registry.bind::<ZxdgOutputManagerV1, _, _>(name, 1, qh, ()));
        }
    }
}

impl Dispatch<WlOutput, usize> for State {
    fn event(
        _: &mut Self,
        _: &WlOutput,
        _: wl_output::Event,
        _: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZxdgOutputManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZxdgOutputV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(logical) = state.logical.get_mut(*index) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => logical.position = Some((x, y)),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                logical.size = Some((width, height));
            }
            _ => {}
        }
    }
}

/// The bounds of every output that reports a usable logical rectangle, in the
/// compositor's logical coordinates.
///
/// `None` when there is no Wayland connection, the compositor lacks
/// `xdg-output`, or it does not answer within [`TIMEOUT`].
pub(super) fn logical_displays() -> Option<Vec<DisplayBounds>> {
    let (sender, receiver) = mpsc::channel();
    // A helper that outlives the timeout ends when the compositor answers.
    thread::Builder::new()
        .name("openlogi-xdg-output".to_owned())
        .spawn(move || {
            let _ = sender.send(read_layout());
        })
        .map_err(|error| debug!(%error, "could not start the screen layout read"))
        .ok()?;
    receiver
        .recv_timeout(TIMEOUT)
        .map_err(|_| debug!("the compositor did not answer the screen layout read in time"))
        .ok()
        .flatten()
}

fn read_layout() -> Option<Vec<DisplayBounds>> {
    let connection = Connection::connect_to_env()
        .map_err(|error| debug!(%error, "no Wayland connection for the screen layout"))
        .ok()?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let mut state = State::default();
    connection.display().get_registry(&qh, ());

    // First round trip: the registry's globals. Second: the xdg-output events
    // that asking the manager about each output provokes.
    queue.roundtrip(&mut state).ok()?;
    let Some(manager) = state.manager.clone() else {
        debug!("the compositor has no xdg-output, so the screen layout is unknown");
        return None;
    };
    for (index, output) in state.outputs.iter().enumerate() {
        manager.get_xdg_output(output, &qh, index);
    }
    queue.roundtrip(&mut state).ok()?;

    let displays: Vec<_> = state.logical.iter().filter_map(|l| l.bounds()).collect();
    (!displays.is_empty()).then_some(displays)
}

#[cfg(test)]
mod tests;
