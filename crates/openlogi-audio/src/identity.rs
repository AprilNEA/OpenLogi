//! Reading a USB identity out of the host audio stack's device strings.
//!
//! macOS's USB audio driver names a device's model `"<product>:<VID>:<PID>"`,
//! with the IDs in hex, and its device UID `"<driver>:…:<serial>:<n>"`, where
//! `n` numbers the device's audio engines: a Yeti shows up as one input and one
//! output device that share everything up to that last field. G HUB's
//! `LogiGamingAudio` driver, which replaces Apple's for Logitech audio devices
//! when installed, keeps both shapes.

/// The USB vendor and product ID at the end of a model UID, or `None` when the
/// model UID does not end in two 4-digit hex fields.
pub(crate) fn usb_ids_from_model_uid(model_uid: &str) -> Option<(u16, u16)> {
    let mut fields = model_uid.rsplit(':');
    let product = hex_id(fields.next()?)?;
    let vendor = hex_id(fields.next()?)?;
    // A bare "VID:PID" carries no product name; the driver always writes one.
    fields.next()?;
    Some((vendor, product))
}

fn hex_id(field: &str) -> Option<u16> {
    if field.len() != 4 {
        return None;
    }
    u16::from_str_radix(field, 16).ok()
}

/// The part of a device UID that the input and output devices of one
/// microphone share: the UID without a trailing `:<engine number>`.
pub(crate) fn device_stem(uid: &str) -> &str {
    match uid.rsplit_once(':') {
        Some((stem, engine))
            if !stem.is_empty()
                && !engine.is_empty()
                && engine.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            stem
        }
        _ => uid,
    }
}

#[cfg(test)]
mod tests {
    use super::{device_stem, usb_ids_from_model_uid};

    #[test]
    fn a_usb_model_uid_yields_its_ids() {
        assert_eq!(
            usb_ids_from_model_uid("Yeti Stereo Microphone:046D:0AB7"),
            Some((0x046d, 0x0ab7))
        );
        // A product name may itself contain colons.
        assert_eq!(
            usb_ids_from_model_uid("Mic: Pro:046d:0aaf"),
            Some((0x046d, 0x0aaf))
        );
    }

    #[test]
    fn other_model_uids_yield_nothing() {
        for model in [
            "",
            "MacBook Pro Microphone",
            "046D:0AB7",
            "Yeti:46D:0AB7",
            "Yeti:046D:0AB7Z",
            "Yeti:GGGG:0AB7",
            "BlackHole2ch_UID",
        ] {
            assert_eq!(usb_ids_from_model_uid(model), None, "{model:?}");
        }
    }

    #[test]
    fn the_engine_number_is_dropped_from_a_device_uid() {
        let input = "LogiGamingAudio:Generic:Blue Microphones:LT_240615091539AD020037_111000:1";
        let output = "LogiGamingAudio:Generic:Blue Microphones:LT_240615091539AD020037_111000:2";

        assert_eq!(device_stem(input), device_stem(output));
        assert_eq!(
            device_stem(input),
            "LogiGamingAudio:Generic:Blue Microphones:LT_240615091539AD020037_111000"
        );
    }

    #[test]
    fn a_uid_without_an_engine_number_is_its_own_stem() {
        assert_eq!(
            device_stem("BuiltInMicrophoneDevice"),
            "BuiltInMicrophoneDevice"
        );
        assert_eq!(device_stem("AppleUSB:serial:"), "AppleUSB:serial:");
        assert_eq!(device_stem("AppleUSB:serial:1a"), "AppleUSB:serial:1a");
        assert_eq!(device_stem(":1"), ":1");
    }
}
