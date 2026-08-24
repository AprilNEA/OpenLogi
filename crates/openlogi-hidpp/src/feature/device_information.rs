//! Implements the `DeviceInformation` feature (ID `0x0003`) that provides some
//! general information about the device.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, nibble::U4, protocol::v20::Hidpp20Error};

/// Decodes a packed binary-coded decimal byte — two decimal digits, high
/// nibble first. `None` when either nibble is outside `0..=9`, which means the
/// device sent something that is not BCD at all.
fn packed_bcd_u8(bcd: u8) -> Option<u8> {
    let digit_0 = U4::from_hi(bcd).to_lo();
    let digit_1 = U4::from_lo(bcd).to_lo();
    (digit_0 <= 9 && digit_1 <= 9).then_some(digit_0 * 10 + digit_1)
}

/// Decodes a packed binary-coded decimal `u16` as four decimal digits, most
/// significant byte first.
fn packed_bcd_u16(bcd: u16) -> Option<u16> {
    let high = u16::from(packed_bcd_u8((bcd >> 8) as u8)?);
    let low = u16::from(packed_bcd_u8((bcd & 0xff) as u8)?);
    Some(high * 100 + low)
}

/// Implements the `DeviceInformation` / `0x0003` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x0003, version = 0)]
pub struct DeviceInformationFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl DeviceInformationFeature {
    /// Retrieves general information about the device and its capabilities.
    pub async fn get_device_info(&self) -> Result<DeviceInformation, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();

        Ok(DeviceInformation {
            entity_count: payload[0],
            unit_id: [payload[1], payload[2], payload[3], payload[4]],
            transport: DeviceTransport::from_bits_retain(payload[6]),
            model_id: [
                u16::from_be_bytes([payload[7], payload[8]]),
                u16::from_be_bytes([payload[9], payload[10]]),
                u16::from_be_bytes([payload[11], payload[12]]),
            ],
            extended_model_id: payload[13],
            capabilities: DeviceInformationCapabilities::from(payload[14]),
        })
    }

    /// Retrieves information about the firmware of a specific entity,
    /// identified by its index bound by the value in
    /// [`DeviceInformation::entity_count`].
    pub async fn get_fw_info(
        &self,
        entity_index: u8,
    ) -> Result<DeviceEntityFirmwareInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [entity_index, 0x00, 0x00])
            .await?
            .extend_payload();

        Ok(DeviceEntityFirmwareInfo {
            entity_type: DeviceEntityType::try_from(payload[0])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            firmware_prefix: String::from_utf8(payload[1..=3].to_vec())
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            firmware_number: packed_bcd_u8(payload[4]).ok_or(Hidpp20Error::UnsupportedResponse)?,
            revision: packed_bcd_u8(payload[5]).ok_or(Hidpp20Error::UnsupportedResponse)?,
            build: packed_bcd_u16(u16::from_be_bytes([payload[6], payload[7]]))
                .ok_or(Hidpp20Error::UnsupportedResponse)?,
            active: payload[8] & 1 != 0,
            transport_pid: u16::from_be_bytes([payload[9], payload[10]]),
            extra_version: [
                payload[11],
                payload[12],
                payload[13],
                payload[14],
                payload[15],
            ],
        })
    }

    /// Retrieves the serial number of the device.
    ///
    /// This function was added in feature version 4 and will likely result in
    /// an [`v20::ErrorType::InvalidFunctionId`](crate::protocol::v20::ErrorType::InvalidFunctionId)
    /// error for older versions, so
    /// [`DeviceInformationCapabilities::serial_number`] should be verified
    /// before calling.
    pub async fn get_serial_number(&self) -> Result<String, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();

        String::from_utf8(payload[..12].to_vec()).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }
}

/// Represents information about the device as reported by
/// [`DeviceInformationFeature::get_device_info`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub struct DeviceInformation {
    /// The amount of entities in the device from which version information can
    /// be retrieved using [`DeviceInformationFeature::get_fw_info`].
    pub entity_count: u8,

    /// A 4-byte random value serving as a unique identifier (among all devices
    /// with the same [`Self::model_id`]) for the unit.
    ///
    /// This field was added in feature version 1 and will always be `0` for
    /// older versions.
    pub unit_id: [u8; 4],

    /// A bitfield about which transport protocols the device supports.
    ///
    /// This field was added in feature version 1 and will always be `0` for
    /// older versions.
    pub transport: DeviceTransport,

    /// A 6-byte array serving as the identifier for the device model.
    ///
    /// This array will consist of the application PIDs of the different
    /// transport protocols supported by the device, as stated in
    /// [`Self::transport`].
    /// The 16-bit PID for every supported transport protocol will be appended
    /// into this array, limiting the total amount of supported transport
    /// protocols to three.
    ///
    /// This field was added in feature version 1 and will always be `0` for
    /// older versions.
    pub model_id: [u16; 3],

    /// An 8-bit value representing an additional configurable attribute for a
    /// given [`Self::model_id`], set on the production line. This could be the
    /// color of the device.
    ///
    /// This field was added in feature version 2 and will always be `0` for
    /// older versions.
    pub extended_model_id: u8,

    /// Additional capability flags of this feature.
    ///
    /// This field was added in feature version 4 together with the serial
    /// number retrieval function. All capabilities will be flagged as
    /// unsupported for older versions.
    pub capabilities: DeviceInformationCapabilities,
}

bitflags::bitflags! {
    /// Represents the bitfield stating which transport protocols a device
    /// supports.
    ///
    /// One given device can only support up to three transport protocols at a
    /// time.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct DeviceTransport: u8 {
        /// The device supports USB.
        const USB = 1 << 3;

        /// The device supports eQuad, the protocol used by the Unifying
        /// Receiver.
        const E_QUAD = 1 << 2;

        /// The device supports Bluetooth Low Energy as used by the Bolt
        /// Receiver.
        const BTLE = 1 << 1;

        /// The device supports Bluetooth.
        const BLUETOOTH = 1 << 0;
    }
}

/// Represents the bitfield stating which additional capabilities this feature
/// supports.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DeviceInformationCapabilities {
    /// Whether serial number retrieval is supported.
    ///
    /// This field was added in feature version 4 and will always be `false` for
    /// older versions.
    pub serial_number: bool,
}

impl From<u8> for DeviceInformationCapabilities {
    fn from(value: u8) -> Self {
        Self {
            serial_number: value & 1 != 0,
        }
    }
}

/// Represents information about the firmware of a specific device entity as
/// obtained via [`DeviceInformationFeature::get_fw_info`].
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DeviceEntityFirmwareInfo {
    /// The type of the described entity.
    pub entity_type: DeviceEntityType,

    /// A 3-letter prefix for the firmware name.
    pub firmware_prefix: String,

    /// The firmware number.
    ///
    /// This is represented in packed BCD format in the protocol itself, but
    /// decoding is handled by this implementation automatically.
    pub firmware_number: u8,

    /// The firmware revision.
    ///
    /// This is represented in packed BCD format in the protocol itself, but
    /// decoding is handled by this implementation automatically.
    pub revision: u8,

    /// The firmware build.
    ///
    /// This is represented in packed BCD format in the protocol itself, but
    /// decoding is handled by this implementation automatically.
    pub build: u16,

    /// Whether the entity is the responding and active one.
    ///
    /// Exactly one entity will be active at any given time.
    pub active: bool,

    /// The transport protocol PID.
    ///
    /// If this entity is the active one (see [`Self::active`]), this will be
    /// set to the actual PID. If it is not, this field COULD be all-zero.
    pub transport_pid: u16,

    /// Optional extra versioning information.
    pub extra_version: [u8; 5],
}

/// Represents the type of a device entity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum DeviceEntityType {
    /// Main application firmware entity.
    MainApplication = 0,
    /// Bootloader firmware entity.
    Bootloader = 1,
    /// Hardware entity.
    Hardware = 2,
    /// Touchpad firmware/entity.
    Touchpad = 3,
    /// Optical sensor entity.
    OpticalSensor = 4,
    /// Bluetooth SoftDevice entity.
    Softdevice = 5,
    /// RF companion MCU entity.
    RfCompanionMcu = 6,
    /// Factory application firmware entity.
    FactoryApplication = 7,
    /// RGB custom effect entity.
    RgbCustomEffect = 8,
    /// Motor drive entity.
    MotorDrive = 9,
}
