//! Arming for gesture capture: which of one device's controls a session
//! diverts, the firmware state that records, and how it is re-armed and handed
//! back.

use std::sync::Arc;

use hidpp::{channel::HidppChannel, device::Device};
use openlogi_core::binding::ButtonId;
use tracing::{debug, warn};

use super::{CaptureSpec, CapturedInput};
use crate::reprog_controls::{self, ReprogControlsV4};
use crate::session::capture_restore::{
    ArmedReporting, CaptureError, CaptureSessionFailure, PendingCaptureRestore, ReprogRestore,
    divert_change, rollback_capture_start,
};
use crate::thumbwheel::{self, Thumbwheel, ThumbwheelInfo, WheelDirection, WheelResolution};
use crate::{ChannelRegistry, DeviceIoGate, SharedChannel};

/// The set of controls a session has diverted, kept so they can be handed back
/// to the firmware on teardown.
#[derive(Default)]
pub(super) struct ArmedControls {
    /// `0x1b04` accessor, present when the device exposes it.
    pub(super) reprog: Option<ReprogControlsV4>,
    /// The gesture-source CIDs diverted with raw-XY reporting: the
    /// `spec.divert_gesture_sources` members the device exposes.
    pub(super) gesture_cids: Vec<u16>,
    /// Raw-XY-capable additional CIDs diverted as gesture sources (macOS side
    /// buttons and a gesture-mode DPI/ModeShift button).
    pub(super) gesture_button_cids: Vec<(u16, ButtonId)>,
    /// DPI/ModeShift CIDs diverted as plain buttons when gesture mode is off.
    pub(super) dpi_cids: Vec<u16>,
    /// Standard-button CIDs diverted per the session's [`CaptureSpec`], with
    /// the [`ButtonId`] each dispatches as.
    pub(super) button_cids: Vec<(u16, ButtonId)>,
    /// Original reporting state for every diverted `0x1b04` control.
    reporting: Vec<ArmedReporting>,
    /// `0x2150` accessor and the information read while diverting it, present
    /// when the thumb wheel is diverted.
    pub(super) thumb: Option<ArmedThumbwheel>,
}

pub(super) struct ArmedThumbwheel {
    pub(super) wheel: Thumbwheel,
    info: Option<ThumbwheelInfo>,
}

impl ArmedThumbwheel {
    pub(super) fn resolution(&self) -> WheelResolution {
        self.info
            .map_or(WheelResolution::UNKNOWN, |info| info.resolution)
    }

    fn direction(&self) -> WheelDirection {
        if self.info.is_some_and(|info| !info.positive_is_forward()) {
            WheelDirection::Inverted
        } else {
            WheelDirection::Default
        }
    }
}

impl ArmedControls {
    /// Build the one-time polarity fact learned while arming the thumb wheel.
    pub(super) fn thumbwheel_direction(&self) -> Option<CapturedInput> {
        let positive_is_forward = self
            .thumb
            .as_ref()?
            .info
            .map(ThumbwheelInfo::positive_is_forward)?;
        Some(CapturedInput::ThumbwheelDirection {
            positive_is_forward,
        })
    }

    /// Convert all armed firmware state into the one capability that can
    /// release it. Consuming `self` prevents a session and a restore retry from
    /// both claiming ownership at once.
    pub(super) fn into_pending(self, retired: &SharedChannel) -> Option<PendingCaptureRestore> {
        let Self {
            reprog,
            reporting,
            thumb,
            ..
        } = self;
        let reprog =
            reprog.and_then(|controls| ReprogRestore::new(controls.feature_index(), reporting));
        PendingCaptureRestore::new(
            retired,
            reprog,
            thumb.as_ref().map(|thumb| thumb.wheel.feature_index()),
        )
    }

    /// Reapply volatile diversion after a wireless reconnect broadcast. The
    /// broadcast can precede the device accepting feature writes, so allow a
    /// short settling window like the keyboard capture path does.
    pub(super) async fn rearm(&self, device_io: &DeviceIoGate) {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if !device_io.allows_io() {
            return;
        }
        if let Some(rc) = self.reprog.as_ref() {
            for &reporting in &self.reporting {
                let raw_xy = self.gesture_cids.contains(&reporting.cid)
                    || self
                        .gesture_button_cids
                        .iter()
                        .any(|&(cid, _)| cid == reporting.cid);
                let change = divert_change(reporting.original, raw_xy);
                if let Err(error) = rc.set_cid_reporting_full(reporting.cid, change).await {
                    warn!(
                        cid = format_args!("{:#06x}", reporting.cid),
                        ?error,
                        "re-divert after wake failed"
                    );
                }
            }
        }
        if let Some(thumb) = self.thumb.as_ref()
            && let Err(error) = thumb.wheel.divert(thumb.direction()).await
        {
            warn!(?error, "thumb-wheel re-divert after wake failed");
        }
    }
}

/// Resolve features off the device's root and divert the controls `spec`
/// selects: the gesture sources (raw-XY), DPI/ModeShift buttons and rebindable
/// standard buttons over `0x1b04`, and the thumb wheel over `0x2150`. The
/// root-feature lookup mirrors `write::open_feature`,
/// since hidpp 0.2's registry doesn't carry the features OpenLogi reimplements.
///
/// A failure mid-way tries to hand every possibly-diverted control back to the
/// firmware. If compensation is incomplete, the returned failure carries an
/// opaque restore capability for the manager to retain and retry.
pub(super) async fn arm_controls(
    chan: &Arc<HidppChannel>,
    slot: u8,
    spec: &CaptureSpec,
    shared: &SharedChannel,
    registry: &ChannelRegistry,
) -> Result<ArmedControls, CaptureSessionFailure> {
    let device = Device::new(Arc::clone(chan), slot)
        .await
        .map_err(|_| CaptureError::DeviceUnreachable(slot))?;
    let mut armed = ArmedControls::default();
    if let Err(error) = arm_controls_into(&device, chan, slot, spec, &mut armed).await {
        let pending = armed.into_pending(shared);
        return Err(rollback_capture_start(error, pending, registry).await);
    }
    if armed.gesture_cids.is_empty()
        && armed.gesture_button_cids.is_empty()
        && armed.dpi_cids.is_empty()
        && armed.button_cids.is_empty()
        && armed.thumb.is_none()
    {
        debug!(slot, "no capturable controls — idle session");
    }
    Ok(armed)
}

/// The fallible arming steps of [`arm_controls`], recording ownership before
/// each write. A transport failure cannot prove whether firmware applied that
/// write, so rollback deliberately includes the uncertain current control.
pub(super) async fn arm_controls_into(
    device: &Device,
    chan: &Arc<HidppChannel>,
    slot: u8,
    spec: &CaptureSpec,
    armed: &mut ArmedControls,
) -> Result<(), CaptureError> {
    if let Some(info) = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await?
    {
        let rc = ReprogControlsV4::new(Arc::clone(chan), slot, info.index);
        let controls = enumerate_controls(&rc).await?;
        // Register an accessor before the first divert, so a failure on any
        // divert (including the first) can become a restore capability.
        armed.reprog = Some(rc.clone());

        // Divert each gesture-mode source; a source not listed stays native
        // (an idle HID++ control must not be captured-and-dropped).
        for &cid in &spec.divert_gesture_sources {
            if controls.iter().any(|c| c.cid == cid && c.supports_raw_xy()) {
                arm_reprog_control(&rc, cid, true, &mut armed.reporting).await?;
                armed.gesture_cids.push(cid);
            }
        }
        for &(cid, button) in &spec.divert_gesture_buttons {
            if let Some(control) = controls.iter().find(|c| c.cid == cid)
                && control.is_divertable()
                && control.supports_raw_xy()
            {
                arm_reprog_control(&rc, cid, true, &mut armed.reporting).await?;
                armed.gesture_button_cids.push((cid, button));
            }
        }
        for &cid in &reprog_controls::DPI_MODE_SHIFT_CIDS {
            let gesture_requested = spec
                .divert_gesture_buttons
                .iter()
                .any(|&(gesture_cid, button)| gesture_cid == cid && button == ButtonId::DpiToggle);
            if gesture_requested {
                // The raw-XY loop above owns a supported control. An
                // unsupported one must stay native: plain-diverting it while
                // dispatch still expects gesture events would swallow both
                // the configured click and the firmware action.
                continue;
            }
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                arm_reprog_control(&rc, cid, false, &mut armed.reporting).await?;
                armed.dpi_cids.push(cid);
            }
        }
        for &(cid, button) in &spec.divert_buttons {
            // The plan never lists a raw-XY-diverted gesture source, but
            // guard anyway: a plain (divert, no raw-XY) write here would strip
            // the raw-XY reporting armed above.
            if armed.gesture_cids.contains(&cid)
                || armed
                    .gesture_button_cids
                    .iter()
                    .any(|&(gesture_cid, _)| gesture_cid == cid)
            {
                continue;
            }
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                arm_reprog_control(&rc, cid, false, &mut armed.reporting).await?;
                armed.button_cids.push((cid, button));
            }
        }
    }

    if spec.capture_thumbwheel
        && let Some(info) = device.root().get_feature(thumbwheel::FEATURE_ID).await?
    {
        let tw = Thumbwheel::new(Arc::clone(chan), slot, info.index);
        let wheel_info = match tw.get_info().await {
            Ok(twinfo) => Some(twinfo),
            Err(e) => {
                warn!(error = ?e, "thumb wheel getInfo failed");
                None
            }
        };
        // Divert whenever capture was requested: rotation rebinds and the
        // sensitivity multiplier need the diverted event stream even on wheels
        // that report no single-tap capability (e.g. MX Master 4) — lacking the
        // tap only means a bound click can never fire.
        if wheel_info.is_some_and(|info| !info.supports_single_tap) {
            debug!("thumb wheel reports no single tap — click not capturable");
        }
        // Store ownership before the write: a transport error cannot prove
        // whether firmware applied diversion, so rollback must cover it too.
        armed.thumb = Some(ArmedThumbwheel {
            wheel: tw,
            info: wheel_info,
        });
        if let Some(thumb) = armed.thumb.as_ref() {
            thumb.wheel.divert(thumb.direction()).await?;
        }
    }
    Ok(())
}

async fn arm_reprog_control(
    rc: &ReprogControlsV4,
    cid: u16,
    raw_xy: bool,
    reporting: &mut Vec<ArmedReporting>,
) -> Result<(), CaptureError> {
    let original = rc.get_cid_reporting(cid).await?;
    if original.diverted {
        // Left over from a session that never tore down (agent killed, or
        // another Logitech app). Worth a line: it is the state that used to be
        // replayed on restore, leaving the button dead.
        debug!(cid, "control was already diverted before arming");
    }
    let change = divert_change(original, raw_xy);
    // Record ownership before the write: a transport error does not prove the
    // firmware rejected the command, so rollback must cover this CID too.
    reporting.push(ArmedReporting { cid, original });
    rc.set_cid_reporting_full(cid, change).await?;
    Ok(())
}

/// Read the device's full reprogrammable-control table in one pass, so we can
/// test several CIDs without rescanning per control.
pub(crate) async fn enumerate_controls(
    rc: &ReprogControlsV4,
) -> Result<Vec<reprog_controls::CtrlIdInfo>, CaptureError> {
    let count = rc.get_count().await?;
    let mut controls = Vec::with_capacity(usize::from(count));
    for index in 0..count {
        controls.push(rc.get_ctrl_id_info(index).await?);
    }
    Ok(controls)
}
