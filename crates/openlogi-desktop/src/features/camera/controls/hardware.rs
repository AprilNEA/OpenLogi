//! The controls panel's hardware boundary. Tests substitute the UVC transfers,
//! not the panel's load lifecycle, rows, or persistence.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use openlogi_camera::{AutoToggle, CameraControl, CameraState, ControlError};
use tracing::debug;

use crate::ui::commit_slider::SliderRange;

/// The UVC calls the panel makes. `Send + Sync` because the initial read runs
/// on the background executor.
pub(super) trait Uvc: Send + Sync {
    fn read_camera_state(&self, uid: &str) -> Result<CameraState, ControlError>;
    fn apply_settings(
        &self,
        uid: &str,
        autos: &[(AutoToggle, bool)],
        values: &[(CameraControl, i32)],
    ) -> Result<(), ControlError>;
    fn set_control(
        &self,
        uid: &str,
        control: CameraControl,
        value: i32,
    ) -> Result<(), ControlError>;
}

pub(super) struct SystemUvc;

impl Uvc for SystemUvc {
    fn read_camera_state(&self, uid: &str) -> Result<CameraState, ControlError> {
        openlogi_camera::read_camera_state(uid)
    }

    fn apply_settings(
        &self,
        uid: &str,
        autos: &[(AutoToggle, bool)],
        values: &[(CameraControl, i32)],
    ) -> Result<(), ControlError> {
        openlogi_camera::apply_settings(uid, autos, values)
    }

    fn set_control(
        &self,
        uid: &str,
        control: CameraControl,
        value: i32,
    ) -> Result<(), ControlError> {
        openlogi_camera::set_control(uid, control, value)
    }
}

/// The panel's camera access. A background load holds its camera's lock from
/// the first read through the re-apply, so a newer load of the same camera
/// reads only after an older one finished writing. Edits never take a lock:
/// a camera's edits open only once its newest load has landed, by when every
/// older load of it is canceled and will not write.
pub(super) struct Hardware {
    uvc: Arc<dyn Uvc>,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Hardware {
    pub(super) fn new(uvc: Arc<dyn Uvc>) -> Self {
        Self {
            uvc,
            locks: Mutex::new(HashMap::new()),
        }
    }

    fn camera_lock(&self, uid: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(uid.to_string()).or_default())
    }

    pub(super) fn apply_settings(
        &self,
        uid: &str,
        autos: &[(AutoToggle, bool)],
        values: &[(CameraControl, i32)],
    ) -> Result<(), ControlError> {
        self.uvc.apply_settings(uid, autos, values)
    }

    pub(super) fn set_control(
        &self,
        uid: &str,
        control: CameraControl,
        value: i32,
    ) -> Result<(), ControlError> {
        self.uvc.set_control(uid, control, value)
    }
}

/// Set when the panel drops the load that owns it: the camera changed, or the
/// panel went away. The load checks it before writing.
#[derive(Default)]
pub(super) struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub(super) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }
}

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The settings saved for a camera, gathered on the UI thread so the load can
/// run without touching `AppState`.
#[derive(Default)]
pub(super) struct Saved {
    pub(super) autos: Vec<(AutoToggle, bool)>,
    pub(super) values: Vec<(CameraControl, i32)>,
}

impl Saved {
    fn auto(&self, toggle: AutoToggle) -> Option<bool> {
        self.autos
            .iter()
            .find(|(t, _)| *t == toggle)
            .map(|(_, on)| *on)
    }

    fn value(&self, control: CameraControl) -> Option<i32> {
        self.values
            .iter()
            .find(|(c, _)| *c == control)
            .map(|(_, v)| *v)
    }
}

/// What a background load publishes to the panel.
pub(super) struct Loaded {
    /// The state to build rows from — ranges and defaults as the device
    /// reports them, each `current` what the hardware holds after the saved
    /// settings were re-asserted. `None` when the camera is unreachable or a
    /// rejected re-apply left its state unknown.
    pub(super) state: Option<CameraState>,
    /// The saved settings did not stick, so the active profile no longer
    /// describes the hardware.
    pub(super) reapply_failed: bool,
}

/// Read the camera and re-assert the saved auto/value differences in one
/// batched device-open. Blocking: runs on the background executor.
///
/// Saved auto states win over the device's, then saved values win for
/// controls whose auto is off. `apply_settings` isn't atomic — it writes the
/// auto mode, then the value — so a rejected batch can leave the hardware
/// between states and the pre-write snapshot untrustworthy: the device is
/// re-read, and a re-read that also fails yields no state rather than the stale
/// pre-write one.
///
/// A load whose `canceled` flag is set by the time it would write returns
/// without writing; nothing reads its result.
pub(super) fn load(hw: &Hardware, uid: &str, saved: &Saved, canceled: &AtomicBool) -> Loaded {
    let uvc = hw.uvc.as_ref();
    let camera = hw.camera_lock(uid);
    let _loading = camera.lock().unwrap_or_else(PoisonError::into_inner);
    let Ok(mut state) = uvc.read_camera_state(uid) else {
        debug!("camera state read failed");
        return Loaded {
            state: None,
            reapply_failed: false,
        };
    };
    let read = state.clone();

    let mut apply_autos = Vec::new();
    for (toggle, st) in &mut state.autos {
        let on = saved.auto(*toggle).unwrap_or(st.current);
        if on != st.current {
            apply_autos.push((*toggle, on));
        }
        st.current = on;
    }
    let mut apply_values = Vec::new();
    for (control, range) in &mut state.controls {
        let saved_value = saved.value(*control);
        let initial =
            SliderRange::new(range.min, range.max).clamp(saved_value.unwrap_or(range.current));
        let auto_on = control
            .auto_toggle()
            .is_some_and(|toggle| state.autos.iter().any(|(t, st)| *t == toggle && st.current));
        if saved_value.is_some_and(|v| v != range.current) && !auto_on {
            apply_values.push((*control, initial));
        }
        range.current = initial;
    }

    if apply_autos.is_empty() && apply_values.is_empty() {
        return Loaded {
            state: Some(state),
            reapply_failed: false,
        };
    }
    if canceled.load(Ordering::Acquire) {
        return Loaded {
            state: None,
            reapply_failed: false,
        };
    }
    let Err(e) = uvc.apply_settings(uid, &apply_autos, &apply_values) else {
        return Loaded {
            state: Some(state),
            reapply_failed: false,
        };
    };
    debug!(error = %e, "saved camera state reapply failed");
    let live = match uvc.read_camera_state(uid) {
        Ok(live) => live,
        Err(e) => {
            debug!(error = %e, "post-failure camera re-read failed");
            return Loaded {
                state: None,
                reapply_failed: true,
            };
        }
    };
    // Keep the first read's rows; take each `current` from the live read,
    // falling back to the first read for anything the re-read lost.
    for ((toggle, st), (_, first)) in state.autos.iter_mut().zip(&read.autos) {
        st.current = live
            .autos
            .iter()
            .find(|(t, _)| t == toggle)
            .map_or(first.current, |(_, l)| l.current);
    }
    for ((control, range), (_, first)) in state.controls.iter_mut().zip(&read.controls) {
        range.current = live
            .controls
            .iter()
            .find(|(c, _)| c == control)
            .map_or(first.current, |(_, l)| l.current);
    }
    Loaded {
        state: Some(state),
        reapply_failed: true,
    }
}
