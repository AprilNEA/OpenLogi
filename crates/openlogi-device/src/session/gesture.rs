//! Live control capture for one device: divert the device's gesture sources
//! (DPI/ModeShift, the MX dedicated gesture button and/or the MX Master 4
//! haptic panel), and the thumb wheel over HID++ and turn their events
//! into [`CapturedInput`] the GUI can dispatch.
//!
//! [`run_capture_session`] runs on the HID++ channel inventory already holds
//! open for one device, enables diversion on whichever of those controls it
//! exposes, registers one message listener, and restores every control's
//! default mapping on shutdown. Using that one channel matters: a second
//! channel to the same device would split its input-report stream, so all
//! captured controls share this session. The channel lifecycle itself is
//! `session::capture`'s, shared with the keyboard session; this module owns
//! what is armed (`arm`) and what its reports mean (`accum`).
//!
//! The session is transport-only — it has no opinion on what an input *does*.
//! The GUI maps each [`CapturedInput`] to the user's bound action and dispatches
//! it, mirroring how the CGEventTap hook handles the side buttons. The thumb
//! wheel is special: diverting it stops native horizontal scroll, so the GUI
//! re-synthesises scroll from the [`CapturedInput::Scroll`] deltas — the wheel
//! is therefore only diverted when the user's thumbwheel config leaves its
//! defaults (click bound, rotation rebound, or sensitivity changed).

mod accum;
mod arm;

use std::sync::{Arc, Mutex, PoisonError};

use hidpp::protocol::v20;
use openlogi_core::binding::{ButtonId, GestureDirection};
use tokio::sync::mpsc;
use tracing::info;

use crate::SharedChannel;
use crate::channel::route::DeviceRoute;

pub use super::capture::CaptureHost;
use super::capture::{ArmedCapture, Liveness, run_capture};
use accum::CaptureAccum;
pub(crate) use arm::enumerate_controls;
use arm::{ArmedControls, ArmedThumbwheel, arm_controls};

pub use super::capture_restore::{
    CaptureChannelSlot, CaptureError, CaptureSessionFailure, CaptureSessionOutcome,
    PendingCaptureRestore,
};
use crate::reprog_controls::{self, ReprogControlsV4};
use crate::thumbwheel::{self, WheelResolution};

/// One input captured from the active device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturedInput {
    /// A completed swipe (or tap click) from a diverted gesture source,
    /// tagged with the source control so dispatch resolves it against that
    /// button's own direction map.
    Gesture(ButtonId, GestureDirection),
    /// A diverted button's physical down edge.
    ButtonDown(ButtonId),
    /// Thumb-wheel rotation to re-synthesise on the configured scroll axis.
    /// Emitted while the wheel is diverted (click bound, rotation rebound, or
    /// sensitivity changed).
    Scroll {
        /// Rotation in the wheel's diverted increments. Positive is always
        /// physical forward/up: arming normalises the model-specific polarity
        /// reported by `0x2150 default_dir`.
        increments: i16,
        /// What one revolution measures in each mode, so the dispatcher can
        /// scale those increments back to the wheel's native scroll amount
        /// instead of scrolling by however finely this wheel happens to
        /// report.
        resolution: WheelResolution,
    },
    /// The un-inverted polarity learned while arming a thumb wheel. This is a
    /// one-time session fact rather than user input; the agent records it for
    /// native horizontal-wheel events that the Windows hook cannot attribute
    /// to a device.
    ThumbwheelDirection {
        /// Whether a positive native delta is physical forward/up.
        positive_is_forward: bool,
    },
    /// A diverted button's physical up edge.
    ButtonUp(ButtonId),
    /// An instantaneous firmware-reported tap with no observable hold
    /// duration, such as the thumb-wheel touch sensor.
    ButtonPulse(ButtonId),
}

/// HID++-divertable standard buttons: the `0x1b04` control ID and the
/// [`ButtonId`] its press dispatches as. A button is diverted per device only
/// when its binding leaves the default, so an unbound button keeps its native
/// HID behavior (no re-synthesis needed). The Haptic Sense Panel is a gesture
/// source ([`GESTURE_SOURCE_BUTTONS`]), not a member of this table.
///
/// The two wheel-tilt CIDs are the classic "Left/Right Scroll" controls that
/// MX-line mice with a tilting main wheel (MX Anywhere 2S and friends) expose
/// as divertable — the same mechanism Options+ uses to rebind a tilt. Arming
/// only ever diverts what a device's own `getCtrlIdInfo` reports, so listing
/// them here is inert on a mouse whose wheel does not tilt.
pub const DIVERTABLE_STANDARD_BUTTONS: [(u16, ButtonId); 9] = {
    // Destructured rather than indexed: a family that gains a CID stops
    // compiling here instead of silently staying out of the table.
    let [
        back,
        back_multiplatform,
        back_multiplatform_alt,
        back_generic,
    ] = reprog_controls::BACK_CIDS;
    let [forward, forward_multiplatform] = reprog_controls::FORWARD_CIDS;
    [
        (0x0052, ButtonId::MiddleClick),
        (back, ButtonId::Back),
        (back_multiplatform, ButtonId::Back),
        (back_multiplatform_alt, ButtonId::Back),
        (back_generic, ButtonId::Back),
        (forward, ButtonId::Forward),
        (forward_multiplatform, ButtonId::Forward),
        (0x005b, ButtonId::WheelTiltLeft),
        (0x005d, ButtonId::WheelTiltRight),
    ]
};

/// HID++ gesture sources: the `0x1b04` control ID and the [`ButtonId`] it
/// delivers — the dedicated gesture button on most MX mice, and the Haptic
/// Sense Panel on MX Master 4 (two distinct physical controls). Each source in
/// gesture mode is diverted with raw-XY; one with a non-default single binding
/// instead is plain-diverted like a standard button.
pub const GESTURE_SOURCE_BUTTONS: [(u16, ButtonId); 2] = [
    (reprog_controls::GESTURE_BUTTON_CID, ButtonId::GestureButton),
    (reprog_controls::HAPTIC_PANEL_CID, ButtonId::HapticPanel),
];

/// Which of one device's controls a capture session should divert.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureSpec {
    /// Divert the thumb wheel over `0x2150` (rotation rebind / sensitivity /
    /// click bound).
    pub capture_thumbwheel: bool,
    /// Gesture-source CIDs ([`GESTURE_SOURCE_BUTTONS`] members) to divert
    /// with raw-XY — one per source in gesture mode; empty when no HID++
    /// control gestures.
    pub divert_gesture_sources: Vec<u16>,
    /// Standard-button CIDs requested as raw-XY gesture sources. A control is
    /// armed only when its HID++ capability flags advertise raw-XY support.
    pub divert_gesture_buttons: Vec<(u16, ButtonId)>,
    /// Buttons to divert as plain presses (no raw-XY): the
    /// [`DIVERTABLE_STANDARD_BUTTONS`] and non-gesturing
    /// [`GESTURE_SOURCE_BUTTONS`] whose binding leaves the default.
    pub divert_buttons: Vec<(u16, ButtonId)>,
}

/// Capture the controls selected by `spec` on `route` until `host.shutdown`
/// resolves, forwarding each event to `host.sink`.
///
/// Each gesture source in `spec.divert_gesture_sources` is diverted with
/// raw-XY. A source not in gesture mode keeps its native behavior — unless a
/// non-default single binding puts it in `spec.divert_buttons`, in which case
/// it is diverted as a plain button (the OS hook never sees a gesture-source
/// CID, so this is the binding's only delivery path). The DPI/ModeShift
/// capture and the channel-reuse slot are independent of this.
///
/// Runs on the inventory-owned channel `host.registry` currently publishes
/// for `route`: sharing that connection avoids splitting HID++ replies and
/// input reports across two readers, and a registry miss
/// ([`CaptureError::DeviceNotFound`]) is retried by the caller after a later
/// inventory publication. Diverts whichever of those controls the device
/// exposes, and listens. Returns once `host.shutdown` fires (or its sender is
/// dropped). A normal stop restores every diverted control before returning;
/// transport replacement or loss may return
/// [`CaptureSessionOutcome::RestorePending`] for the caller to retry on the
/// current inventory channel.
pub async fn run_capture_session(
    route: DeviceRoute,
    spec: CaptureSpec,
    host: CaptureHost<'_>,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    let shared = host.channel_for(&route)?;
    let armed = arm_controls(&shared, &spec, host.registry).await?;
    if let Some(direction) = armed.thumbwheel_direction() {
        let _ = host.sink.send(direction);
    }
    Ok(run_capture(shared, GestureCapture::new(armed), host).await)
}

/// Gesture capture as [`run_capture`] drives it: the armed controls, and the
/// accumulator their reports feed.
struct GestureCapture {
    armed: ArmedControls,
    /// Behind a `Mutex` because the channel's read thread invokes the report
    /// handler by shared reference.
    accum: Arc<Mutex<CaptureAccum>>,
}

impl GestureCapture {
    fn new(armed: ArmedControls) -> Self {
        Self {
            armed,
            accum: Arc::default(),
        }
    }
}

impl ArmedCapture for GestureCapture {
    const NAME: &'static str = "control";
    const LIVENESS: Liveness = Liveness::Watched;

    fn log_active(&self, device_index: u8, wake_rearm: bool) {
        let armed = &self.armed;
        info!(
            index = device_index,
            gesture_sources = armed.gesture_cids.len(),
            gesture_buttons = armed.gesture_button_cids.len(),
            dpi_buttons = armed.dpi_cids.len(),
            buttons = armed.button_cids.len(),
            thumbwheel = armed.thumb.is_some(),
            wake_rearm,
            "control capture active"
        );
    }

    fn report_handler(
        &self,
        device_index: u8,
        sink: mpsc::UnboundedSender<CapturedInput>,
    ) -> impl Fn(&v20::Message) + Send + Sync + 'static {
        let armed = &self.armed;
        let accum = Arc::clone(&self.accum);
        let reprog_index = armed.reprog.as_ref().map(ReprogControlsV4::feature_index);
        let gesture_cids = armed.gesture_cids.clone();
        let gesture_button_set = armed.gesture_button_cids.clone();
        let thumb_index = armed
            .thumb
            .as_ref()
            .map(|thumb| thumb.wheel.feature_index());
        let thumb_resolution = armed
            .thumb
            .as_ref()
            .map_or(WheelResolution::UNKNOWN, ArmedThumbwheel::resolution);
        let dpi_set = armed.dpi_cids.clone();
        let button_set = armed.button_cids.clone();
        move |msg| {
            if let Some(idx) = reprog_index
                && let Some(event) = reprog_controls::decode_event(msg, device_index, idx)
            {
                // Recover the guard even if a prior holder panicked — the
                // critical section is panic-free, so the data is consistent.
                let mut acc = accum.lock().unwrap_or_else(PoisonError::into_inner);
                acc.on_event(
                    event,
                    &gesture_cids,
                    &dpi_set,
                    &gesture_button_set,
                    &button_set,
                    &sink,
                );
                return;
            }
            if let Some(idx) = thumb_index
                && let Some(event) = thumbwheel::decode_event(msg, device_index, idx)
                && let Some(input) = thumbwheel_input(event, thumb_resolution)
            {
                let _ = sink.send(input);
            }
        }
    }

    fn reset_input_state(&self) {
        *self.accum.lock().unwrap_or_else(PoisonError::into_inner) = CaptureAccum::default();
    }

    async fn rearm(&self) {
        self.armed.rearm().await;
    }

    fn into_pending(self, retired: &SharedChannel) -> Option<PendingCaptureRestore> {
        self.armed.into_pending(retired)
    }
}

/// The single input one diverted thumb-wheel report stands for, if any.
///
/// A report is a roll *or* a tap, never both, and `0x2150` says which: the
/// wheel's touch sensor sets `single_tap` for the finger that turned the
/// wheel, so every report from `Start` through `Stop` carries a tap bit that
/// belongs to the roll rather than to the user. `Stop` is the one that needs
/// the status field — it is the release, so it reports no rotation of its own
/// and is otherwise indistinguishable from a tap on a settled wheel.
///
/// A report's own rotation is checked alongside the status rather than
/// through it: both are direct statements that this report is part of a roll,
/// and taking either keeps the roll recognised on a wheel whose firmware
/// leaves byte 4 at zero.
fn thumbwheel_input(
    event: thumbwheel::ThumbwheelEvent,
    resolution: WheelResolution,
) -> Option<CapturedInput> {
    if event.rotation != 0 {
        return Some(CapturedInput::Scroll {
            increments: event.rotation,
            resolution,
        });
    }
    if event.rotation_status.is_rolling() {
        return None;
    }
    event
        .single_tap
        .then_some(CapturedInput::ButtonPulse(ButtonId::Thumbwheel))
}

#[cfg(test)]
mod tests;
