//! Logitech (Blue) USB microphone identities.
//!
//! These are USB Audio Class devices, not HID++. Their settings — mic gain,
//! mute, headphone volume, mic monitoring — are UAC feature-unit controls the
//! host's audio stack already exposes, so the agent drives them through that
//! stack rather than through a HID channel. A HID interface may be present
//! (the Yeti has an input-only Consumer/Telephony collection for its mute
//! button), but it carries no settings and is not part of this identity.

use crate::LOGITECH_VENDOR_ID;

/// A known USB microphone identity and its static product metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicrophoneDescriptor {
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// Marketed product name.
    pub name: &'static str,
    /// Model ID the openlogi-org/assets index lists for this microphone, used
    /// to resolve its device image.
    pub registry_model_id: &'static str,
}

impl MicrophoneDescriptor {
    const fn logitech(
        product_id: u16,
        name: &'static str,
        registry_model_id: &'static str,
    ) -> Self {
        Self {
            vendor_id: LOGITECH_VENDOR_ID,
            product_id,
            name,
            registry_model_id,
        }
    }
}

/// All USB microphones OpenLogi recognizes.
pub const MICROPHONES: &[MicrophoneDescriptor] = &[
    // Yeti (Logitech-era revision, 046D:0AB7). UAC 1.0: FU 0x13 holds the mic
    // gain and mute, FU 0x23 the monitoring (sidetone) level and mute, and
    // FU 0x16 the headphone volume and mute.
    MicrophoneDescriptor::logitech(0x0ab7, "Yeti", "0ab7"),
];

/// Finds a microphone descriptor by its USB identity.
#[must_use]
pub fn find_microphone(vendor_id: u16, product_id: u16) -> Option<&'static MicrophoneDescriptor> {
    MICROPHONES
        .iter()
        .find(|device| device.vendor_id == vendor_id && device.product_id == product_id)
}

#[cfg(test)]
mod tests {
    use super::{MICROPHONES, find_microphone};
    use crate::LOGITECH_VENDOR_ID;

    #[test]
    fn microphone_identities_are_unique() {
        for (index, device) in MICROPHONES.iter().enumerate() {
            assert!(
                MICROPHONES[..index].iter().all(|other| {
                    (other.vendor_id, other.product_id) != (device.vendor_id, device.product_id)
                }),
                "duplicate microphone identity {:04x}:{:04x}",
                device.vendor_id,
                device.product_id
            );
        }
    }

    #[test]
    fn product_id_selects_the_yeti() {
        let yeti = find_microphone(LOGITECH_VENDOR_ID, 0x0ab7).expect("Yeti identity");

        assert_eq!(yeti.name, "Yeti");
        assert_eq!(yeti.registry_model_id, "0ab7");
    }

    #[test]
    fn lookup_requires_the_complete_identity() {
        assert!(find_microphone(0xffff, 0x0ab7).is_none());
        assert!(find_microphone(LOGITECH_VENDOR_ID, 0x0ab8).is_none());
    }
}
