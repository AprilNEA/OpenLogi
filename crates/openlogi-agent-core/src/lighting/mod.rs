//! Host lighting session: firmware apply, renderer loop, and capture backends.

mod audio;
mod render;
mod screen;
mod session;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use openlogi_core::config::Lighting;
use openlogi_hid::DeviceRoute;

use crate::hardware::DeviceOp;
use crate::orchestrator::SharedRuntime;

pub use audio::available as audio_available;
pub use screen::available as screen_available;

static LAST_PRESS_MS: AtomicU64 = AtomicU64::new(0);

/// Record an input press for [`openlogi_core::hid::LightingEffect::EchoPress`].
pub fn notify_press() {
    LAST_PRESS_MS.store(unix_ms(), Ordering::Relaxed);
}

pub(crate) fn millis_since_press() -> u64 {
    unix_ms().saturating_sub(LAST_PRESS_MS.load(Ordering::Relaxed))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Per-route host lighting sessions owned by the agent runtime.
#[derive(Clone, Default)]
pub struct LightingHost {
    inner: Arc<session::Sessions>,
}

impl LightingHost {
    /// Stop any renderer for `route`.
    pub fn stop(&self, route: &DeviceRoute) {
        self.inner.stop(route);
    }

    /// Apply firmware lighting, or start the host renderer when required.
    pub fn apply(&self, shared: &SharedRuntime, route: DeviceRoute, lighting: Lighting) {
        self.inner.apply(shared, route, lighting);
    }

    /// Same as [`Self::apply`] from a bound [`DeviceOp`].
    pub fn apply_op(&self, op: &DeviceOp<'_>, lighting: Lighting) {
        self.inner.apply_op(op, lighting);
    }
}
