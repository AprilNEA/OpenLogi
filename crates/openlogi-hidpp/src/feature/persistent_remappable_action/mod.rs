//! Implements the `PersistentRemappableAction` feature (ID `0x1c00`) that
//! persistently remaps a device control to a different HID action.
//!
//! Controls are identified by the same [`ControlId`]s as
//! [`ReprogControls`](super::reprog_controls) (`0x1b04`); when both features are
//! present and `0x1b04` diverts a control, that takes precedence over a
//! persistent remap here.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{
        CreatableFeature, Feature, FeatureEndpoint, hosts_info::HostIndex,
        reprog_controls::ControlId,
    },
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// What HID outputs a device's persistent remapping can produce, from
    /// [`get_feature_info`](PersistentRemappableActionFeature::get_feature_info).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct RemappableCapabilities: u16 {
        /// Can send keyboard/keypad keys.
        const KEYBOARD_REPORT = 1 << 0;
        /// Can send mouse buttons.
        const MOUSE_BUTTONS = 1 << 1;
        /// Can send mouse X displacement.
        const X_DISPLACEMENT = 1 << 2;
        /// Can send mouse Y displacement.
        const Y_DISPLACEMENT = 1 << 3;
        /// Can send vertical roller increments.
        const VERTICAL_ROLLER = 1 << 4;
        /// Can send horizontal roller (AC pan) increments.
        const HORIZONTAL_ROLLER = 1 << 5;
        /// Can send consumer-control codes.
        const CONSUMER_CONTROL = 1 << 6;
        /// Can execute internal functions.
        const INTERNAL_FUNCTION = 1 << 7;
        /// Can send power keys.
        const POWER_KEY = 1 << 8;
    }
}

bitflags::bitflags! {
    /// Standard keyboard modifier keys for a remapped keyboard action.
    ///
    /// Modifiers only apply to keyboard reports.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ModifierMask: u8 {
        /// Left Control.
        const LEFT_CTRL = 1 << 0;
        /// Left Shift.
        const LEFT_SHIFT = 1 << 1;
        /// Left Alt.
        const LEFT_ALT = 1 << 2;
        /// Left GUI (Win/Command).
        const LEFT_GUI = 1 << 3;
        /// Right Control.
        const RIGHT_CTRL = 1 << 4;
        /// Right Shift.
        const RIGHT_SHIFT = 1 << 5;
        /// Right Alt.
        const RIGHT_ALT = 1 << 6;
        /// Right GUI (Win/Command).
        const RIGHT_GUI = 1 << 7;
    }
}

bitflags::bitflags! {
    /// A set of host slots for
    /// [`reset_to_factory_settings`](PersistentRemappableActionFeature::reset_to_factory_settings).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct HostMask: u8 {
        /// Host 1.
        const HOST_1 = 1 << 0;
        /// Host 2.
        const HOST_2 = 1 << 1;
        /// Host 3.
        const HOST_3 = 1 << 2;
    }
}

/// The action a control performs when triggered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ActionId {
    /// Send a keyboard/keypad report (HID usage page 7).
    SendKeyboard = 0x01,
    /// Send a mouse-button report (usage page 9).
    SendMouseButton = 0x02,
    /// Send mouse X displacement (usage page 1, code 0x30).
    SendXDisplacement = 0x03,
    /// Send mouse Y displacement (usage page 1, code 0x31).
    SendYDisplacement = 0x04,
    /// Send vertical roller/wheel displacement (usage page 1, code 0x38).
    SendVerticalRoller = 0x05,
    /// Send horizontal roller / AC pan displacement (usage page 12, code 0x0238).
    SendHorizontalRoller = 0x06,
    /// Send a consumer-control report (usage page 12).
    SendConsumerControl = 0x07,
    /// Execute an internal function (the value is the function index).
    ExecuteInternalFunction = 0x08,
    /// Send a power-key report (usage page 1).
    SendPowerKey = 0x09,
}

/// Control-table sizing from
/// [`get_count`](PersistentRemappableActionFeature::get_count).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RemappableInfo {
    /// Number of control IDs in the table.
    pub count: u8,
    /// Number of hosts the device supports.
    pub host_count: u8,
}

/// The action mapped to a control, from
/// [`get_persistent_action`](PersistentRemappableActionFeature::get_persistent_action).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct PersistentAction {
    /// The control the action belongs to.
    pub cid: ControlId,
    /// The host slot the mapping applies to.
    pub host: HostIndex,
    /// The action performed when triggered.
    pub action_id: ActionId,
    /// The HID usage code, displacement, or internal-function index sent.
    pub value: u16,
    /// Keyboard modifiers applied (keyboard actions only).
    pub modifier_mask: ModifierMask,
    /// Whether the control is remapped away from its default behaviour.
    pub remapped: bool,
}

impl PersistentAction {
    fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            cid: ControlId::from(u16::from_be_bytes([payload[0], payload[1]])),
            host: HostIndex::from(payload[2]),
            action_id: ActionId::try_from(payload[3])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            value: u16::from_be_bytes([payload[4], payload[5]]),
            modifier_mask: ModifierMask::from_bits_retain(payload[6]),
            remapped: payload[7] & 1 != 0,
        })
    }
}

/// The action to assign with
/// [`set_persistent_action`](PersistentRemappableActionFeature::set_persistent_action).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PersistentActionConfig {
    /// The action to perform when triggered.
    pub action_id: ActionId,
    /// The HID usage code, displacement, or internal-function index to send.
    pub value: u16,
    /// Keyboard modifiers to apply (keyboard actions only).
    pub modifier_mask: ModifierMask,
}

/// Implements the `PersistentRemappableAction` / `0x1c00` feature.
#[derive(Clone)]
pub struct PersistentRemappableActionFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for PersistentRemappableActionFeature {
    const ID: u16 = 0x1c00;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for PersistentRemappableActionFeature {}

impl PersistentRemappableActionFeature {
    /// Retrieves which HID outputs the device's remapping can produce.
    pub async fn get_feature_info(&self) -> Result<RemappableCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(RemappableCapabilities::from_bits_retain(
            u16::from_be_bytes([payload[0], payload[1]]),
        ))
    }

    /// Retrieves the control-ID count and host count.
    pub async fn get_count(&self) -> Result<RemappableInfo, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        Ok(RemappableInfo {
            count: payload[0],
            host_count: payload[1],
        })
    }

    /// Retrieves the control ID at table `index` for `host`.
    pub async fn get_cid_info(
        &self,
        index: u8,
        host: HostIndex,
    ) -> Result<ControlId, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [index, u8::from(host), 0])
            .await?
            .extend_payload();
        Ok(ControlId::from(u16::from_be_bytes([
            payload[0], payload[1],
        ])))
    }

    /// Retrieves the persistent action mapped to `cid` on `host`.
    pub async fn get_persistent_action(
        &self,
        cid: ControlId,
        host: HostIndex,
    ) -> Result<PersistentAction, Hidpp20Error> {
        let [cid_hi, cid_lo] = u16::from(cid).to_be_bytes();
        let payload = self
            .endpoint
            .call(3, [cid_hi, cid_lo, u8::from(host)])
            .await?
            .extend_payload();
        PersistentAction::from_payload(&payload)
    }

    /// Persistently remaps `cid` on `host` to `config`.
    ///
    /// This writes to the device's non-volatile memory and changes the control's
    /// behaviour until reset (see [`Self::reset_persistent_action`]).
    pub async fn set_persistent_action(
        &self,
        cid: ControlId,
        host: HostIndex,
        config: PersistentActionConfig,
    ) -> Result<(), Hidpp20Error> {
        let [cid_hi, cid_lo] = u16::from(cid).to_be_bytes();
        let [value_hi, value_lo] = config.value.to_be_bytes();
        let mut args = [0; 16];
        args[..7].copy_from_slice(&[
            cid_hi,
            cid_lo,
            u8::from(host),
            config.action_id.into(),
            value_hi,
            value_lo,
            config.modifier_mask.bits(),
        ]);
        self.endpoint.call_long(4, args).await?;
        Ok(())
    }

    /// Resets `cid` on `host` to its factory default action.
    pub async fn reset_persistent_action(
        &self,
        cid: ControlId,
        host: HostIndex,
    ) -> Result<(), Hidpp20Error> {
        let [cid_hi, cid_lo] = u16::from(cid).to_be_bytes();
        self.endpoint
            .call(5, [cid_hi, cid_lo, u8::from(host)])
            .await?;
        Ok(())
    }

    /// Resets every control to its factory default for the hosts in `hosts`.
    pub async fn reset_to_factory_settings(&self, hosts: HostMask) -> Result<(), Hidpp20Error> {
        self.endpoint.call(6, [hosts.bits(), 0, 0]).await?;
        Ok(())
    }
}
