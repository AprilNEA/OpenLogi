//! HID++ `ReprogControlsV4` (feature `0x1b04`) — temporary control diversion
//! and raw-XY reporting, the mechanism behind MX-line reprogrammable controls.
//!
//! The full protocol wrapper lives in `openlogi-hidpp`; this module keeps the
//! OpenLogi-facing compatibility API used by gesture/button orchestration:
//! `getCount` / `getCtrlIdInfo` (locate a control and confirm it can divert raw
//! XY) and `setCidReporting` (turn diversion on or off). While a control is
//! diverted with raw-XY reporting, the device emits two unsolicited events,
//! decoded by [`decode_event`]:
//!
//! - function `0` `divertedButtonsEvent` — up to four currently-pressed CIDs.
//! - function `1` `rawXYEvent` — signed `dx`/`dy` while a raw-XY control is held.
//!
//! Wire formats cross-checked against Solaar's `hidpp20.py` and
//! `notifications.py`.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    feature::{CreatableFeature, reprog_controls as hidpp_reprog},
    protocol::v20::Hidpp20Error,
};

mod event;

pub use event::{RawControlEvent, decode_event};
pub use hidpp_reprog::{
    AnalyticsKeyEvent, CidFlags, CidInfo, CidReporting, CidReportingChange, CidReportingChangeEcho,
    ControlId, GroupMask, RawWheelResolution, ReprogControlsCapabilities, ReprogControlsEvent,
    TaskId, decode_event as decode_full_event,
};

/// `ReprogControlsV4` HID++ feature ID.
pub const FEATURE_ID: u16 = 0x1b04;

/// Control ID of the MX-line dedicated gesture button (`Mouse_Gesture_Button`,
/// Logitech "App_Switch_Gesture").
///
/// MX Master 4 also has a separate Haptic Sense Panel in the thumb area. That
/// panel is not this CID; it must be discovered from the device's `0x1b04`
/// control table and supported explicitly before OpenLogi treats it as a
/// bindable/capturable input.
pub const GESTURE_BUTTON_CID: u16 = 0x00c3;

/// Control IDs of the "DPI / ModeShift" button family. Whichever a device
/// exposes (and can divert) is captured and mapped to
/// [`ButtonId::DpiToggle`](openlogi_core::binding::ButtonId::DpiToggle): the MX
/// wheel-mode "Smart Shift" button, plus the dedicated "DPI Change" / "DPI
/// Switch" buttons on other models. Values from the `0x1b04` control-ID list,
/// cross-checked against Solaar `special_keys.py`.
pub const DPI_MODE_SHIFT_CIDS: [u16; 3] = [0x00c4, 0x00ed, 0x00fd];

/// Identity and capabilities of one reprogrammable control, as returned by
/// `getCtrlIdInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtrlIdInfo {
    /// Control ID — stable across firmware (e.g. [`GESTURE_BUTTON_CID`]).
    pub cid: u16,
    /// Task ID — the control's default on-device action.
    pub task_id: u16,
    /// `KeyFlag` capability bitfield (response bytes 4 and 8 combined).
    pub flags: u16,
}

impl CtrlIdInfo {
    /// Typed view of the legacy raw [`Self::flags`] field.
    #[must_use]
    pub fn typed_flags(self) -> CidFlags {
        CidFlags::from_bits_retain(self.flags)
    }

    /// Whether the control can be temporarily diverted to HID++ events.
    #[must_use]
    pub fn is_divertable(self) -> bool {
        self.typed_flags().is_divertable()
    }

    /// Whether the control can report raw XY movement while held — required to
    /// decode a swipe into a direction.
    #[must_use]
    pub fn supports_raw_xy(self) -> bool {
        self.typed_flags().supports_raw_xy()
    }

    /// Whether the control can report force raw-XY data while held.
    #[must_use]
    pub fn supports_force_raw_xy(self) -> bool {
        self.typed_flags().supports_force_raw_xy()
    }

    /// Whether the control can report analytics key events.
    #[must_use]
    pub fn supports_analytics_events(self) -> bool {
        self.typed_flags().supports_analytics_key_events()
    }

    /// Whether the control can report raw wheel data.
    #[must_use]
    pub fn supports_raw_wheel(self) -> bool {
        self.typed_flags().supports_raw_wheel()
    }
}

impl From<CidInfo> for CtrlIdInfo {
    fn from(info: CidInfo) -> Self {
        Self {
            cid: info.cid.into(),
            task_id: info.task_id.0,
            flags: info.flags.raw(),
        }
    }
}

/// `ReprogControlsV4` accessor bound to one device + resolved feature index.
///
/// Construct with the feature index obtained from the device's root feature
/// (`get_feature(`[`FEATURE_ID`]`)`), then call the functions below. Cheap to
/// clone (an `Arc` plus two indices).
#[derive(Clone)]
pub struct ReprogControlsV4 {
    inner: Arc<hidpp_reprog::ReprogControlsFeature>,
    device_index: u8,
    feature_index: u8,
}

impl ReprogControlsV4 {
    /// Bind the feature to `(device_index, feature_index)` on `chan`.
    #[must_use]
    pub fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            inner: Arc::new(hidpp_reprog::ReprogControlsFeature::new(
                chan,
                device_index,
                feature_index,
            )),
            device_index,
            feature_index,
        }
    }

    /// The feature index this accessor talks to — used to match unsolicited
    /// events in [`decode_event`].
    #[must_use]
    pub fn feature_index(&self) -> u8 {
        self.feature_index
    }

    /// The device index this accessor talks to.
    #[must_use]
    pub fn device_index(&self) -> u8 {
        self.device_index
    }

    /// Number of reprogrammable controls the device exposes.
    pub async fn get_count(&self) -> Result<u8, Hidpp20Error> {
        self.inner.get_count().await
    }

    /// Identity + capabilities of the control at `index` (`0..get_count`).
    pub async fn get_cid_info(&self, index: u8) -> Result<CidInfo, Hidpp20Error> {
        self.inner.get_cid_info(index).await
    }

    /// Compatibility projection of [`Self::get_cid_info`].
    pub async fn get_ctrl_id_info(&self, index: u8) -> Result<CtrlIdInfo, Hidpp20Error> {
        Ok(self.get_cid_info(index).await?.into())
    }

    /// Scan the control table for the control with `cid`. `None` if the device
    /// doesn't expose it.
    pub async fn find_cid_info(&self, cid: ControlId) -> Result<Option<CidInfo>, Hidpp20Error> {
        let count = self.get_count().await?;
        for index in 0..count {
            let info = self.get_cid_info(index).await?;
            if info.cid == cid {
                return Ok(Some(info));
            }
        }
        Ok(None)
    }

    /// Compatibility projection of [`Self::find_cid_info`].
    pub async fn find_control(&self, cid: u16) -> Result<Option<CtrlIdInfo>, Hidpp20Error> {
        Ok(self.find_cid_info(ControlId(cid)).await?.map(Into::into))
    }

    /// Current reporting/remapping state for `cid`.
    pub async fn get_cid_reporting(&self, cid: u16) -> Result<CidReporting, Hidpp20Error> {
        self.inner.get_cid_reporting(ControlId(cid)).await
    }

    /// Apply the full `setCidReporting` packet.
    pub async fn set_cid_reporting_full(
        &self,
        cid: u16,
        change: CidReportingChange,
    ) -> Result<CidReportingChangeEcho, Hidpp20Error> {
        self.inner.set_cid_reporting(ControlId(cid), change).await
    }

    /// Feature-level v6 capabilities.
    pub async fn get_capabilities(&self) -> Result<ReprogControlsCapabilities, Hidpp20Error> {
        self.inner.get_capabilities().await
    }

    /// Reset all diverted/remapped control report settings on v6 devices that
    /// advertise this capability.
    pub async fn reset_all_cid_report_settings(&self) -> Result<(), Hidpp20Error> {
        self.inner.reset_all_cid_report_settings().await
    }

    /// Set (or clear) temporary diversion and raw-XY reporting for `cid`.
    ///
    /// `remap` is left at `0` (no persistent remapping). After enabling, the
    /// device emits [`RawControlEvent`]s on this feature index; clear both flags
    /// on teardown to hand the control back to the firmware.
    pub async fn set_cid_reporting(
        &self,
        cid: u16,
        diverted: bool,
        raw_xy: bool,
    ) -> Result<(), Hidpp20Error> {
        self.set_cid_reporting_full(
            cid,
            hidpp_reprog::CidReportingChange::temporary_diversion(diverted, raw_xy),
        )
        .await?;
        Ok(())
    }
}
