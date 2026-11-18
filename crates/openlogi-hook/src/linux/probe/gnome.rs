//! One session bus connection, owned by the Flow worker. Each layout has
//! a token the extension invalidates on lock, monitor or controller changes.
//! An old layout can never move the cursor or produce a new pointer sample.

use std::{io, time::Duration};

use rustix::time::{ClockId, clock_gettime};

use tracing::debug;
use zbus::blocking::{Connection, connection::Builder};
use zbus::proxy;

use crate::{DesktopPoint, DisplayBounds, KeyModifiers, PointerState, WarpError};

type Rect = (i32, i32, i32, i32);

#[proxy(
    interface = "org.openlogi.FlowPointer",
    default_service = "org.openlogi.Frontmost",
    default_path = "/org/openlogi/FlowPointer",
    gen_async = false
)]
trait FlowPointer {
    fn get_layout(&self) -> zbus::Result<(String, Vec<Rect>)>;
    fn read_pointer(&self, generation: &str) -> zbus::Result<(i32, i32, bool)>;
    fn place_pointer_monotonic(
        &self,
        generation: &str,
        x: f64,
        y: f64,
        deadline_us: u64,
    ) -> zbus::Result<bool>;
}

#[derive(Default)]
pub(crate) struct Probe {
    phase: Phase,
}

#[derive(Default)]
enum Phase {
    #[default]
    Disconnected,
    AwaitingLayout {
        proxy: FlowPointerProxy<'static>,
    },
    Ready {
        proxy: FlowPointerProxy<'static>,
        generation: String,
    },
}

impl Probe {
    pub(super) fn generation(&self) -> &str {
        match &self.phase {
            Phase::Ready { generation, .. } => generation,
            Phase::Disconnected | Phase::AwaitingLayout { .. } => "",
        }
    }

    fn connect() -> zbus::Result<FlowPointerProxy<'static>> {
        let bus: Connection = Builder::session()?
            .name("org.openlogi.OpenLogi.Flow")?
            .method_timeout(Duration::from_secs(1))
            .build()?;
        FlowPointerProxy::new(&bus)
    }

    pub(super) fn displays(&mut self) -> Vec<DisplayBounds> {
        let proxy = match &self.phase {
            Phase::Disconnected => match Self::connect() {
                Ok(proxy) => proxy,
                Err(_) => return Vec::new(),
            },
            Phase::AwaitingLayout { proxy } | Phase::Ready { proxy, .. } => proxy.clone(),
        };
        let result = proxy.get_layout();
        self.update_layout(proxy, result)
            .into_iter()
            .map(|(x, y, w, h)| DisplayBounds {
                x: f64::from(x),
                y: f64::from(y),
                width: f64::from(w),
                height: f64::from(h),
            })
            .collect()
    }

    /// Retain the connection across Shell/layout failures. Only a transport
    /// error needs a new connection at the next geometry refresh.
    fn update_layout(
        &mut self,
        proxy: FlowPointerProxy<'static>,
        layout: zbus::Result<(String, Vec<Rect>)>,
    ) -> Vec<Rect> {
        let (phase, monitors) = match layout {
            Ok((generation, monitors)) if !generation.is_empty() => {
                (Phase::Ready { proxy, generation }, monitors)
            }
            Ok(_) => (Phase::AwaitingLayout { proxy }, Vec::new()),
            Err(error) => {
                if matches!(self.phase, Phase::Ready { .. }) {
                    debug!(%error, "GNOME Flow pointer unavailable");
                }
                let phase = if matches!(error, zbus::Error::InputOutput(_)) {
                    Phase::Disconnected
                } else {
                    Phase::AwaitingLayout { proxy }
                };
                (phase, Vec::new())
            }
        };
        self.phase = phase;
        monitors
    }

    pub(super) fn state(&mut self) -> Option<PointerState> {
        let Phase::Ready { proxy, generation } = &self.phase else {
            return None;
        };
        let (x, y, control) = match proxy.read_pointer(generation) {
            Ok(sample) => sample,
            Err(error) => {
                // Retry on the geometry refresh, never on every raw motion.
                self.update_layout(proxy.clone(), Err(error));
                return None;
            }
        };
        Some(PointerState {
            position: DesktopPoint {
                x: f64::from(x),
                y: f64::from(y),
            },
            modifiers: KeyModifiers {
                control,
                ..KeyModifiers::default()
            },
        })
    }

    pub(super) fn warp(&self, to: DesktopPoint) -> Result<(), WarpError> {
        let Phase::Ready { proxy, generation } = &self.phase else {
            return Err(WarpError::OutsideDisplays);
        };
        // GLib.get_monotonic_time() in Linux Shell uses CLOCK_MONOTONIC.
        // The absolute deadline includes time queued on the session bus.
        let now = Duration::try_from(clock_gettime(ClockId::Monotonic))
            .map_err(|error| WarpError::Rejected(io::Error::other(error)))?;
        let deadline_us = u64::try_from((now + Duration::from_millis(500)).as_micros())
            .map_err(|error| WarpError::Rejected(io::Error::other(error)))?;
        if proxy
            .place_pointer_monotonic(generation, to.x, to.y, deadline_us)
            .map_err(|e| WarpError::Rejected(io::Error::other(e)))?
        {
            Ok(())
        } else {
            Err(WarpError::Rejected(io::Error::other(
                "GNOME did not confirm cursor placement",
            )))
        }
    }
}
