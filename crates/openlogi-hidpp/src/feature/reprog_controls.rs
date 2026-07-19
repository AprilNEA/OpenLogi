//! Implements `SpecialKeysMseButtons` / `ReprogControlsV4` (feature `0x1b04`).
//!
//! Logitech's v6 document names this feature `SpecialKeysMseButtons`: it
//! enumerates physical and virtual controls, lets host software divert or remap
//! them, and emits notifications for diverted buttons, raw XY, analytics key
//! events, and raw wheel movement.

use std::sync::Arc;

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

pub mod control_ids;
mod event;
pub mod task_ids;

use event::decode_event_payload;
pub use event::{AnalyticsKeyEvent, RawWheelResolution, ReprogControlsEvent, decode_event};

/// Implements the `SpecialKeysMseButtons` / `0x1b04` feature.
pub struct ReprogControlsFeature {
    endpoint: FeatureEndpoint,
    emitter: Arc<EventEmitter<ReprogControlsEvent>>,
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for ReprogControlsFeature {
    const ID: u16 = 0x1b04;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        let emitter = Arc::new(EventEmitter::new());

        let listener = chan.add_msg_listener_guarded({
            let emitter = Arc::clone(&emitter);

            move |raw, matched| {
                let Some((func, payload)) =
                    event_payload(raw, matched, device_index, feature_index)
                else {
                    return;
                };
                let Some(event) = decode_event_payload(func.to_lo(), &payload) else {
                    return;
                };
                emitter.emit(event);
            }
        });

        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
            emitter,
            _msg_listener: listener,
        }
    }
}

impl Feature for ReprogControlsFeature {}

impl EmittingFeature<ReprogControlsEvent> for ReprogControlsFeature {
    fn listen(&self) -> async_channel::Receiver<ReprogControlsEvent> {
        self.emitter.create_receiver()
    }
}

impl ReprogControlsFeature {
    /// Returns the number of rows in the control ID table.
    pub async fn get_count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Returns one row from the control ID table.
    pub async fn get_cid_info(&self, index: u8) -> Result<CidInfo, Hidpp20Error> {
        let mut params = [0u8; 16];
        params[0] = index;
        let payload = self.endpoint.call_long(1, params).await?.extend_payload();
        Ok(CidInfo::from_payload(payload))
    }

    /// Returns the current reporting/remapping state for `cid`.
    pub async fn get_cid_reporting(&self, cid: ControlId) -> Result<CidReporting, Hidpp20Error> {
        let [cid_hi, cid_lo] = cid.0.to_be_bytes();
        let payload = self
            .endpoint
            .call(2, [cid_hi, cid_lo, 0])
            .await?
            .extend_payload();
        Ok(CidReporting::from_payload(payload))
    }

    /// Applies reporting/remapping changes for `cid`.
    ///
    /// Optional boolean fields in [`CidReportingChange`] map to the corresponding
    /// `*-valid` bit in Logitech's packet. Fields set to `None` are left
    /// unchanged by the device. Remapping is carried as a value field rather
    /// than a valid/value pair; `None` sends the documented `0` value.
    pub async fn set_cid_reporting(
        &self,
        cid: ControlId,
        change: CidReportingChange,
    ) -> Result<CidReportingChangeEcho, Hidpp20Error> {
        let payload = self
            .endpoint
            .call_long(3, change.to_payload(cid))
            .await?
            .extend_payload();
        Ok(CidReportingChangeEcho::from_payload(payload))
    }

    /// Returns feature-level capabilities.
    ///
    /// This function exists on v6 devices. Older firmware may return
    /// `InvalidFunctionId`.
    pub async fn get_capabilities(&self) -> Result<ReprogControlsCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(4, [0; 3]).await?.extend_payload();
        Ok(ReprogControlsCapabilities {
            reset_all_cid_report_settings: payload[0] & 1 != 0,
        })
    }

    /// Resets all diverted or remapped control settings.
    ///
    /// This function exists on v6 devices that report
    /// [`ReprogControlsCapabilities::reset_all_cid_report_settings`].
    pub async fn reset_all_cid_report_settings(&self) -> Result<(), Hidpp20Error> {
        self.endpoint.call(5, [0; 3]).await?;
        Ok(())
    }
}

/// A HID++ control ID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ControlId(pub u16);

impl ControlId {
    fn from_payload(bytes: &[u8]) -> Self {
        Self(u16_from_be_payload(bytes))
    }
}

impl From<u16> for ControlId {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

impl From<ControlId> for u16 {
    fn from(value: ControlId) -> Self {
        value.0
    }
}

fn u16_from_be_payload(bytes: &[u8]) -> u16 {
    u16::from_be_bytes(bytes.try_into().unwrap())
}

/// A HID++ task ID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TaskId(pub u16);

impl From<u16> for TaskId {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

impl From<TaskId> for u16 {
    fn from(value: TaskId) -> Self {
        value.0
    }
}

/// One `getCidInfo` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CidInfo {
    /// Control ID.
    pub cid: ControlId,
    /// Default task ID currently associated with the control.
    pub task_id: TaskId,
    /// Capability and classification flags.
    pub flags: CidFlags,
    /// Physical position value reported by the device.
    pub position: u8,
    /// Control group number.
    pub group: u8,
    /// Bit mask of groups this control belongs to.
    pub group_mask: GroupMask,
}

impl CidInfo {
    fn from_payload(payload: [u8; 16]) -> Self {
        Self {
            cid: ControlId::from_payload(&payload[0..=1]),
            task_id: TaskId(u16_from_be_payload(&payload[2..=3])),
            flags: CidFlags::from_bytes(payload[4], payload[8]),
            position: payload[5],
            group: payload[6],
            group_mask: GroupMask(payload[7]),
        }
    }
}

bitflags::bitflags! {
    /// Capability and classification flags for one control ID.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct CidFlags: u16 {
        /// Control belongs to a mouse/pointer device.
        const MOUSE = 1 << 0;
        /// Control is a keyboard function key.
        const FUNCTION_KEY = 1 << 1;
        /// Control is a hotkey.
        const HOTKEY = 1 << 2;
        /// Control toggles Fn behavior.
        const FN_TOGGLE = 1 << 3;
        /// Control can be reprogrammed.
        const REPROGRAMMABLE = 1 << 4;
        /// Control can be temporarily diverted to HID++ events.
        const DIVERTABLE = 1 << 5;
        /// Control can be persistently diverted.
        const PERSISTENTLY_DIVERTABLE = 1 << 6;
        /// Control is virtual rather than a physical input.
        const VIRTUAL_CONTROL = 1 << 7;
        /// Control supports raw XY reporting.
        const RAW_XY = 1 << 8;
        /// Control supports force raw XY reporting.
        const FORCE_RAW_XY = 1 << 9;
        /// Control supports analytics key events.
        const ANALYTICS_KEY_EVENTS = 1 << 10;
        /// Control supports raw wheel events.
        const RAW_WHEEL = 1 << 11;
    }
}

impl CidFlags {
    fn from_bytes(primary: u8, additional: u8) -> Self {
        Self::from_bits_retain(u16::from(primary) | (u16::from(additional) << 8))
    }

    /// Raw `flags` value used by older OpenLogi diagnostics: primary flags in
    /// the low byte, additional flags in the high byte.
    #[must_use]
    pub fn raw(self) -> u16 {
        self.bits()
    }

    /// Whether this is a mouse control.
    #[must_use]
    pub fn is_mouse(self) -> bool {
        self.contains(Self::MOUSE)
    }

    /// Whether this control can be temporarily diverted to HID++ events.
    #[must_use]
    pub fn is_divertable(self) -> bool {
        self.contains(Self::DIVERTABLE)
    }

    /// Whether this control can be persistently diverted.
    #[must_use]
    pub fn is_persistently_divertable(self) -> bool {
        self.contains(Self::PERSISTENTLY_DIVERTABLE)
    }

    /// Whether this is a virtual control.
    #[must_use]
    pub fn is_virtual_control(self) -> bool {
        self.contains(Self::VIRTUAL_CONTROL)
    }

    /// Whether this control can report raw XY movement while held.
    #[must_use]
    pub fn supports_raw_xy(self) -> bool {
        self.contains(Self::RAW_XY)
    }

    /// Whether this control can report force raw XY movement while held.
    #[must_use]
    pub fn supports_force_raw_xy(self) -> bool {
        self.contains(Self::FORCE_RAW_XY)
    }

    /// Whether this control can report analytics key events.
    #[must_use]
    pub fn supports_analytics_key_events(self) -> bool {
        self.contains(Self::ANALYTICS_KEY_EVENTS)
    }

    /// Whether this control can report raw wheel events.
    #[must_use]
    pub fn supports_raw_wheel(self) -> bool {
        self.contains(Self::RAW_WHEEL)
    }
}

/// Group mask `g1..g8` from `getCidInfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct GroupMask(pub u8);

/// Current reporting/remapping state returned by `getCidReporting`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CidReporting {
    /// Control ID whose reporting state was read.
    pub cid: ControlId,
    /// Whether temporary diversion is enabled.
    pub diverted: bool,
    /// Whether persistent diversion is enabled.
    pub persistently_diverted: bool,
    /// Whether force raw XY reporting is enabled.
    pub force_raw_xy: bool,
    /// Whether raw XY reporting is enabled.
    pub raw_xy: bool,
    /// Optional remapping target control ID.
    pub remap: Option<ControlId>,
    /// Whether analytics key events are enabled.
    pub analytics_key_events: bool,
    /// Whether raw wheel reporting is enabled.
    pub raw_wheel: bool,
}

impl CidReporting {
    fn from_payload(payload: [u8; 16]) -> Self {
        let remap = ControlId::from_payload(&payload[3..=4]);
        Self {
            cid: ControlId::from_payload(&payload[0..=1]),
            diverted: payload[2] & (1 << 0) != 0,
            persistently_diverted: payload[2] & (1 << 2) != 0,
            raw_xy: payload[2] & (1 << 4) != 0,
            force_raw_xy: payload[2] & (1 << 6) != 0,
            remap: (remap.0 != 0).then_some(remap),
            analytics_key_events: payload[5] & (1 << 0) != 0,
            raw_wheel: payload[5] & (1 << 2) != 0,
        }
    }
}

/// Changes for `setCidReporting`.
///
/// For boolean fields, `None` means "leave unchanged". Remapping is encoded as
/// the packet's value field and defaults to the documented `0` value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CidReportingChange {
    /// New temporary diversion state, or `None` to leave unchanged.
    pub diverted: Option<bool>,
    /// New persistent diversion state, or `None` to leave unchanged.
    pub persistently_diverted: Option<bool>,
    /// New force raw XY state, or `None` to leave unchanged.
    pub force_raw_xy: Option<bool>,
    /// New raw XY state, or `None` to leave unchanged.
    pub raw_xy: Option<bool>,
    /// Remaps to another control ID. `None` sends the documented `0` value,
    /// which represents no persistent remapping.
    pub remap: Option<ControlId>,
    /// New analytics key event state, or `None` to leave unchanged.
    pub analytics_key_events: Option<bool>,
    /// New raw wheel state, or `None` to leave unchanged.
    pub raw_wheel: Option<bool>,
}

impl CidReportingChange {
    /// Change only the temporary diverted/raw-XY bits.
    #[must_use]
    pub fn temporary_diversion(diverted: bool, raw_xy: bool) -> Self {
        Self {
            diverted: Some(diverted),
            raw_xy: Some(raw_xy),
            ..Self::default()
        }
    }

    fn to_payload(self, cid: ControlId) -> [u8; 16] {
        let mut payload = [0u8; 16];
        let [cid_hi, cid_lo] = cid.0.to_be_bytes();
        payload[0] = cid_hi;
        payload[1] = cid_lo;

        if let Some(value) = self.diverted {
            payload[2] |= 1 << 1;
            payload[2] |= u8::from(value);
        }
        if let Some(value) = self.persistently_diverted {
            payload[2] |= 1 << 3;
            payload[2] |= u8::from(value) << 2;
        }
        if let Some(value) = self.raw_xy {
            payload[2] |= 1 << 5;
            payload[2] |= u8::from(value) << 4;
        }
        if let Some(value) = self.force_raw_xy {
            payload[2] |= 1 << 7;
            payload[2] |= u8::from(value) << 6;
        }
        if let Some(remap) = self.remap {
            let [remap_hi, remap_lo] = remap.0.to_be_bytes();
            payload[3] = remap_hi;
            payload[4] = remap_lo;
        }
        if let Some(value) = self.analytics_key_events {
            payload[5] |= 1 << 1;
            payload[5] |= u8::from(value);
        }
        if let Some(value) = self.raw_wheel {
            payload[5] |= 1 << 3;
            payload[5] |= u8::from(value) << 2;
        }

        payload
    }
}

/// Echo returned by `setCidReporting`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CidReportingChangeEcho {
    /// Control ID whose reporting state was changed.
    pub cid: ControlId,
    /// Echoed temporary diversion state when changed.
    pub diverted: Option<bool>,
    /// Echoed persistent diversion state when changed.
    pub persistently_diverted: Option<bool>,
    /// Echoed force raw XY state when changed.
    pub force_raw_xy: Option<bool>,
    /// Echoed raw XY state when changed.
    pub raw_xy: Option<bool>,
    /// Echoed remapping target when present.
    pub remap: Option<ControlId>,
    /// Echoed analytics key event state when changed.
    pub analytics_key_events: Option<bool>,
    /// Echoed raw wheel state when changed.
    pub raw_wheel: Option<bool>,
}

impl CidReportingChangeEcho {
    fn from_payload(payload: [u8; 16]) -> Self {
        let remap = ControlId::from_payload(&payload[3..=4]);
        Self {
            cid: ControlId::from_payload(&payload[0..=1]),
            diverted: (payload[2] & (1 << 1) != 0).then_some(payload[2] & (1 << 0) != 0),
            persistently_diverted: (payload[2] & (1 << 3) != 0)
                .then_some(payload[2] & (1 << 2) != 0),
            raw_xy: (payload[2] & (1 << 5) != 0).then_some(payload[2] & (1 << 4) != 0),
            force_raw_xy: (payload[2] & (1 << 7) != 0).then_some(payload[2] & (1 << 6) != 0),
            remap: (remap.0 != 0).then_some(remap),
            analytics_key_events: (payload[5] & (1 << 1) != 0)
                .then_some(payload[5] & (1 << 0) != 0),
            raw_wheel: (payload[5] & (1 << 3) != 0).then_some(payload[5] & (1 << 2) != 0),
        }
    }
}

/// Feature-level capabilities returned by `getCapabilities` on v6 devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ReprogControlsCapabilities {
    /// Whether `resetAllCidReportSettings` is supported.
    pub reset_all_cid_report_settings: bool,
}

#[cfg(test)]
mod tests;
