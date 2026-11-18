//! One session bus connection, owned by the Flow worker. Each layout has
//! a token the extension invalidates on lock, monitor or controller changes.
//! An old layout can never move the cursor or produce a new pointer sample.

use std::{
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
    fn place_pointer(
        &self,
        generation: &str,
        x: f64,
        y: f64,
        expires_ms: u64,
    ) -> zbus::Result<bool>;
}

#[derive(Default)]
pub(crate) struct Probe {
    proxy: Option<FlowPointerProxy<'static>>,
    generation: String,
}

impl Probe {
    pub(super) fn generation(&self) -> &str {
        &self.generation
    }

    fn connect() -> zbus::Result<FlowPointerProxy<'static>> {
        let bus: Connection = Builder::session()?
            .name("org.openlogi.OpenLogi.Flow")?
            .method_timeout(Duration::from_secs(1))
            .build()?;
        FlowPointerProxy::new(&bus)
    }

    pub(super) fn displays(&mut self) -> Vec<DisplayBounds> {
        let result = match &self.proxy {
            Some(proxy) => proxy.get_layout(),
            None => Self::connect().and_then(|proxy| self.proxy.insert(proxy).get_layout()),
        };
        match result {
            Ok((generation, monitors)) => {
                self.generation = generation;
                monitors
                    .into_iter()
                    .map(|(x, y, w, h)| DisplayBounds {
                        x: f64::from(x),
                        y: f64::from(y),
                        width: f64::from(w),
                        height: f64::from(h),
                    })
                    .collect()
            }
            Err(error) => {
                if !self.generation.is_empty() {
                    debug!(%error, "GNOME Flow pointer unavailable");
                }
                self.generation.clear();
                if matches!(error, zbus::Error::InputOutput(_)) {
                    self.proxy = None;
                }
                Vec::new()
            }
        }
    }

    pub(super) fn state(&mut self) -> Option<PointerState> {
        if self.generation.is_empty() {
            return None;
        }
        let result = self.proxy.as_ref()?.read_pointer(&self.generation);
        let Ok((x, y, control)) = result else {
            // Retry on the geometry refresh, never on every raw motion.
            self.generation.clear();
            return None;
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
        let proxy = self.proxy.as_ref().ok_or(WarpError::OutsideDisplays)?;
        // Shell may process a queued call after this worker has timed out.
        let expires = SystemTime::now() + Duration::from_millis(500);
        let expires_ms = expires
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok())
            .ok_or_else(|| WarpError::Rejected(io::Error::other("invalid placement deadline")))?;
        if proxy
            .place_pointer(&self.generation, to.x, to.y, expires_ms)
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
