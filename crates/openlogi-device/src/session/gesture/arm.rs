//! Arming for gesture capture: which of one device's controls a session
//! diverts, the firmware state that records, and how it is re-armed and handed
//! back.

use std::num::NonZeroU8;
use std::sync::Arc;

use hidpp::feature::{CreatableFeature as _, hires_wheel::HiResWheelFeature};
use hidpp::protocol::v20::Hidpp20Error;
use hidpp::{channel::HidppChannel, device::Device};
use openlogi_core::binding::ButtonId;
use tracing::{debug, warn};

use super::main_wheel::{self, ArmedMainWheel, MainWheelDivert};
use super::{CaptureSpec, CapturedInput};
use crate::reprog_controls::{self, ReprogControlsV4};
use crate::session::capture::open_device;
use crate::session::capture_restore::{
    ArmedReporting, CaptureError, CaptureSessionFailure, HiResWheelRestore, PendingCaptureRestore,
    PointerDivert, ReprogRestore, ThumbwheelRestore, restore_now,
};
use crate::session::restore::rollback_start;
use crate::thumbwheel::{
    self, ReportingMode, Thumbwheel, ThumbwheelInfo, WheelDirection, WheelResolution,
};
use crate::{ChannelRegistry, SharedChannel};

/// The set of controls a session has diverted, kept so they can be handed back
/// to the firmware on teardown.
#[derive(Default)]
pub(super) struct ArmedControls {
    /// `0x1b04` accessor, present when the device exposes it.
    pub(super) reprog: Option<ReprogControlsV4>,
    /// The gesture-source CIDs diverted with raw-XY reporting: the
    /// `spec.divert_gesture_sources` members the device exposes.
    pub(super) gesture_cids: Vec<u16>,
    /// Raw-XY-capable additional CIDs diverted as gesture sources (side
    /// buttons on supported desktops and a gesture-mode DPI/ModeShift button).
    pub(super) gesture_button_cids: Vec<(u16, ButtonId)>,
    /// DPI/ModeShift CIDs diverted as plain buttons when gesture mode is off.
    pub(super) dpi_cids: Vec<u16>,
    /// Standard-button CIDs diverted per the session's [`CaptureSpec`], with
    /// the [`ButtonId`] each dispatches as.
    pub(super) button_cids: Vec<(u16, ButtonId)>,
    /// Wheel-hold CIDs diverted with raw-wheel reporting, with the
    /// [`ButtonId`] each dispatches as. A wheel-hold control without raw-wheel
    /// support is plain-diverted into `button_cids` instead.
    pub(super) wheel_button_cids: Vec<(u16, ButtonId)>,
    /// The main wheel's high-resolution multiplier, read from `0x2121` when a
    /// wheel-hold control is armed; `None` on a wheel without that feature,
    /// which cannot report in high resolution.
    pub(super) hires_per_notch: Option<NonZeroU8>,
    /// Original reporting state for every diverted `0x1b04` control.
    reporting: Vec<ArmedReporting>,
    /// `0x2150` accessor and the information read while diverting it, present
    /// when the thumb wheel is diverted.
    pub(super) thumb: Option<ArmedThumbwheel>,
    /// `0x2121` main wheel, present when its movement reports are diverted.
    pub(super) main_wheel: Option<ArmedMainWheel>,
    /// What the session owned before a live reconfiguration and the new spec
    /// has neither re-claimed nor handed back yet.
    superseded: Superseded,
}

/// Firmware ownership carried across a live reconfiguration. Arming re-claims
/// what the new spec still diverts; the rest is handed back afterwards.
#[derive(Default)]
struct Superseded {
    reporting: Vec<ArmedReporting>,
    thumb: Option<ArmedThumbwheel>,
    main_wheel: Option<ArmedMainWheel>,
}

pub(super) struct ArmedThumbwheel {
    pub(super) wheel: Thumbwheel,
    info: Option<ThumbwheelInfo>,
    restore: ThumbwheelRestore,
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
            mut reporting,
            thumb,
            main_wheel,
            superseded,
            ..
        } = self;
        reporting.extend(superseded.reporting);
        let reprog =
            reprog.and_then(|controls| ReprogRestore::new(controls.feature_index(), reporting));
        PendingCaptureRestore::new(
            retired,
            reprog,
            thumb
                .or(superseded.thumb)
                .as_ref()
                .map(|thumb| thumb.restore),
            main_wheel
                .or(superseded.main_wheel)
                .as_ref()
                .map(|wheel| HiResWheelRestore::new(wheel.feature_index)),
        )
    }

    /// Move a live session to `spec` on its own channel. A control `spec`
    /// still diverts the same way is not written again, one it diverts
    /// differently is switched in place with the reporting read before the
    /// session first diverted it, and one it drops is handed back. Returns
    /// whether firmware now matches `spec`; either way every control the
    /// session may still have diverted stays owned for teardown.
    pub(super) async fn reconfigure(&mut self, shared: &SharedChannel, spec: &CaptureSpec) -> bool {
        let device = match open_device(shared).await {
            Ok(device) => device,
            Err(error) => {
                warn!(%error, "control capture reload could not reach the device");
                return false;
            }
        };
        let previous = std::mem::take(self);
        let mut superseded = previous.superseded;
        superseded.reporting.extend(previous.reporting);
        superseded.thumb = previous.thumb.or(superseded.thumb);
        superseded.main_wheel = previous.main_wheel.or(superseded.main_wheel);
        self.reprog = previous.reprog;
        self.superseded = superseded;
        if let Err(error) =
            arm_controls_into(&device, shared.channel(), shared.device_index(), spec, self).await
        {
            warn!(%error, "control capture reload failed");
            return false;
        }
        let superseded = &self.superseded;
        let reprog = self.reprog.as_ref().and_then(|controls| {
            ReprogRestore::new(controls.feature_index(), superseded.reporting.clone())
        });
        let released = restore_now(
            shared,
            reprog,
            superseded.thumb.as_ref().map(|thumb| thumb.restore),
            superseded
                .main_wheel
                .as_ref()
                .map(|wheel| HiResWheelRestore::new(wheel.feature_index)),
        )
        .await;
        if released {
            self.superseded = Superseded::default();
        }
        released
    }

    /// Reapply volatile diversion after a wireless reconnect broadcast.
    pub(super) async fn rearm(&self) {
        if let Some(rc) = self.reprog.as_ref() {
            for &reporting in &self.reporting {
                if let Err(error) = rc
                    .set_cid_reporting_full(reporting.cid, reporting.divert_change())
                    .await
                {
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
        if let Some(wheel) = self.main_wheel.as_ref()
            && let Err(error) = wheel.divert().await
        {
            warn!(?error, "main-wheel re-divert after wake failed");
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
    shared: &SharedChannel,
    spec: &CaptureSpec,
    registry: &ChannelRegistry,
) -> Result<ArmedControls, CaptureSessionFailure> {
    let device = open_device(shared).await?;
    let chan = shared.channel();
    let slot = shared.device_index();
    let mut armed = ArmedControls::default();
    if let Err(error) = arm_controls_into(&device, chan, slot, spec, &mut armed).await {
        let pending = armed.into_pending(shared);
        return Err(rollback_start(error, pending, registry).await);
    }
    if armed.gesture_cids.is_empty()
        && armed.gesture_button_cids.is_empty()
        && armed.dpi_cids.is_empty()
        && armed.button_cids.is_empty()
        && armed.wheel_button_cids.is_empty()
        && armed.thumb.is_none()
        && armed.main_wheel.is_none()
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
            if controls
                .iter()
                .any(|c| c.cid == cid && c.is_divertable() && c.supports_raw_xy())
            {
                arm_reprog_control(&rc, cid, PointerDivert::RawXy, armed).await?;
                armed.gesture_cids.push(cid);
            }
        }
        for &(cid, button) in &spec.divert_gesture_buttons {
            if let Some(control) = controls.iter().find(|c| c.cid == cid)
                && control.is_divertable()
                && control.supports_raw_xy()
            {
                arm_reprog_control(&rc, cid, PointerDivert::RawXy, armed).await?;
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
                arm_reprog_control(&rc, cid, PointerDivert::Plain, armed).await?;
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
                arm_reprog_control(&rc, cid, PointerDivert::Plain, armed).await?;
                armed.button_cids.push((cid, button));
            }
        }
        arm_wheel_hold_controls(device, chan, slot, &rc, &controls, spec, armed).await?;
    }

    if spec.capture_thumbwheel
        && let Some(thumb) = armed.superseded.thumb.take()
    {
        armed.thumb = Some(thumb);
    } else if spec.capture_thumbwheel
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
        // Read before the first divert: teardown hands back the native
        // inversion found here (see `ThumbwheelRestore::new`).
        let original = match tw.get_status().await {
            Ok(status) => Some(status),
            Err(e) => {
                warn!(error = ?e, "thumb wheel getStatus failed; restore will use the default sign");
                None
            }
        };
        if original.is_some_and(|status| status.mode == ReportingMode::Diverted) {
            debug!("thumb wheel was already diverted before arming");
        }
        // Store ownership before the write: a transport error cannot prove
        // whether firmware applied diversion, so rollback must cover it too.
        armed.thumb = Some(ArmedThumbwheel {
            restore: ThumbwheelRestore::new(tw.feature_index(), original),
            wheel: tw,
            info: wheel_info,
        });
        if let Some(thumb) = armed.thumb.as_ref() {
            thumb.wheel.divert(thumb.direction()).await?;
        }
    }

    if let Some(requested) = spec.divert_main_wheel
        && let Some(info) = device.root().get_feature(HiResWheelFeature::ID).await?
    {
        arm_main_wheel(chan, slot, info.index, requested, armed).await?;
    }
    Ok(())
}

/// Divert the main wheel for smooth scrolling.
///
/// A wheel that refuses — a feature error, or an echo showing another mode —
/// is handed back to native reporting and the rest of the session arms
/// anyway, so only smoothing is lost rather than every capture on the
/// device. A transport failure still fails arming, which rolls the wheel
/// back with everything else.
async fn arm_main_wheel(
    chan: &Arc<HidppChannel>,
    slot: u8,
    feature_index: u8,
    requested: MainWheelDivert,
    armed: &mut ArmedControls,
) -> Result<(), CaptureError> {
    let wheel = match ArmedMainWheel::open(chan, slot, feature_index, requested).await {
        Ok(wheel) => wheel,
        Err(error @ Hidpp20Error::Channel(_)) => return Err(error.into()),
        Err(error) => {
            warn!(?error, "main wheel unreadable — left native");
            return Ok(());
        }
    };
    // Same ownership rule as the thumb wheel: recorded before the write,
    // which a transport error leaves uncertain.
    let wheel = armed.main_wheel.insert(wheel);
    // This divert rewrites any mode a live session armed before, so that
    // earlier ownership passes to it instead of being handed back.
    armed.superseded.main_wheel = None;
    match wheel.divert().await {
        Ok(()) => Ok(()),
        Err(error @ Hidpp20Error::Channel(_)) => Err(error.into()),
        Err(error) => {
            // A refused write may still have moved the target, and an echo
            // mismatch says it did something else: hand the wheel back.
            warn!(?error, "main wheel refused the divert — handing it back");
            main_wheel::undivert(&wheel.feature).await?;
            armed.main_wheel = None;
            Ok(())
        }
    }
}

async fn arm_reprog_control(
    rc: &ReprogControlsV4,
    cid: u16,
    divert: PointerDivert,
    armed: &mut ArmedControls,
) -> Result<(), CaptureError> {
    if let Some(index) = armed
        .superseded
        .reporting
        .iter()
        .position(|previous| previous.cid == cid)
    {
        // Re-reading would record this session's own diversion as the
        // original, so keep the one read before the session first armed it.
        let previous = armed.superseded.reporting.swap_remove(index);
        let next = ArmedReporting::pointer(cid, previous.original, divert);
        armed.reporting.push(next);
        if next != previous {
            rc.set_cid_reporting_full(cid, next.rediverted_from(previous))
                .await?;
        }
        return Ok(());
    }
    let reporting = &mut armed.reporting;
    let original = rc.get_cid_reporting(cid).await?;
    if original.diverted {
        // Left over from a session that never tore down (agent killed, or
        // another Logitech app). Worth a line: it is the state that used to be
        // replayed on restore, leaving the button dead.
        debug!(cid, "control was already diverted before arming");
    }
    let armed = ArmedReporting::pointer(cid, original, divert);
    // Record ownership before the write: a transport error does not prove the
    // firmware rejected the command, so rollback must cover this CID too.
    reporting.push(armed);
    rc.set_cid_reporting_full(cid, armed.divert_change())
        .await?;
    Ok(())
}

/// Divert the wheel-hold buttons `spec` lists, with raw-wheel reporting where
/// the control offers it, then read the main wheel's multiplier so a
/// high-resolution report can be scaled to notches. Like every step of
/// [`arm_controls_into`], ownership is recorded before each write.
async fn arm_wheel_hold_controls(
    device: &Device,
    chan: &Arc<HidppChannel>,
    slot: u8,
    rc: &ReprogControlsV4,
    controls: &[reprog_controls::CtrlIdInfo],
    spec: &CaptureSpec,
    armed: &mut ArmedControls,
) -> Result<(), CaptureError> {
    for &(cid, button) in &spec.divert_wheel_buttons {
        // A control armed earlier already has an owner; arming it twice
        // would record a second restore for the same CID.
        if armed.reporting.iter().any(|reporting| reporting.cid == cid) {
            continue;
        }
        let Some(control) = controls.iter().find(|c| c.cid == cid && c.is_divertable()) else {
            continue;
        };
        if control.supports_raw_wheel() {
            arm_reprog_control(rc, cid, PointerDivert::RawWheel, armed).await?;
            armed.wheel_button_cids.push((cid, button));
        } else {
            // Without raw-wheel reporting the button cannot scroll, but its
            // click is still the user's: deliver it as a plain press.
            debug!(
                cid,
                "control lacks raw-wheel reporting — wheel-hold armed as a click"
            );
            arm_reprog_control(rc, cid, PointerDivert::Plain, armed).await?;
            armed.button_cids.push((cid, button));
        }
    }
    if !armed.wheel_button_cids.is_empty() {
        armed.hires_per_notch = read_hires_per_notch(device, chan, slot).await?;
    }
    Ok(())
}

/// The main wheel's high-resolution multiplier: how many units a
/// high-resolution report carries per notch. `None` when the device has no
/// `0x2121`, so its wheel cannot report in high resolution, or reports a
/// multiplier of zero.
async fn read_hires_per_notch(
    device: &Device,
    chan: &Arc<HidppChannel>,
    slot: u8,
) -> Result<Option<NonZeroU8>, CaptureError> {
    let Some(info) = device.root().get_feature(HiResWheelFeature::ID).await? else {
        return Ok(None);
    };
    let wheel = HiResWheelFeature::new(Arc::clone(chan), slot, info.index);
    let capabilities = wheel.get_wheel_capabilities().await?;
    Ok(NonZeroU8::new(capabilities.multiplier))
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
