//! Official application firmware releases OpenLogi can install.
//!
//! Each entry pins one Logitech release by the SHA-256 of its package, so the
//! updater only ever writes an image someone has checked against Logitech's
//! signature. A release lands here once its package verified against the
//! firmware-signing key Logi Options+ ships and it was installed on hardware.
//!
//! Releases are matched to a device by what HID++ `0x0003` reports for its
//! running application: the firmware prefix and number (`RBM` 27 for the
//! MX Master 4) and the product ID in the entity's transport-specific bytes.

/// One official firmware package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmwareRelease {
    /// Marketed device family this approved package targets.
    pub device_name: &'static str,
    /// Product IDs whose application entity reports this firmware.
    pub product_ids: &'static [u16],
    /// The three-letter firmware prefix of the application entity.
    pub prefix: &'static str,
    /// The firmware number.
    pub number: u8,
    /// The release's revision.
    pub revision: u8,
    /// The release's build.
    pub build: u16,
    /// Where Logitech publishes the package (a ZIP holding `package.json` and
    /// one `.rdfu` image).
    pub package_url: &'static str,
    /// SHA-256 of the package, lowercase hex.
    pub package_sha256: &'static str,
    /// Bytes in the package, so a download can stop early.
    pub package_len: u64,
}

impl FirmwareRelease {
    /// Whether this release is firmware for an application entity reporting
    /// `prefix`, `number` and `product_id`.
    #[must_use]
    pub fn fits(&self, prefix: &str, number: u8, product_id: u16) -> bool {
        self.prefix == prefix && self.number == number && self.product_ids.contains(&product_id)
    }
}

/// MX Master 4 firmware 27.3.19, released 2026-03-30.
const MX_MASTER_4_27_3_19: FirmwareRelease = FirmwareRelease {
    device_name: "MX Master 4",
    product_ids: &[0xb042],
    prefix: "RBM",
    number: 27,
    revision: 3,
    build: 19,
    package_url: "https://d23iz4esrwkib6.cloudfront.net/lu/depot/cdbu/logioptionsplus/dfu/1.11/files.latest/canova/27.3.19/firmware.zip",
    package_sha256: "f386ffd7db18effe03fca930accdfde4ecb7cadf5ae695e86493b135cb449cc7",
    package_len: 895_428,
};

/// Every release OpenLogi can install.
pub const FIRMWARE_RELEASES: &[FirmwareRelease] = &[MX_MASTER_4_27_3_19];

/// The newest release for an application entity reporting `prefix`, `number`
/// and `product_id`, if OpenLogi knows any.
#[must_use]
pub fn latest_release(
    prefix: &str,
    number: u8,
    product_id: u16,
) -> Option<&'static FirmwareRelease> {
    FIRMWARE_RELEASES
        .iter()
        .filter(|release| release.fits(prefix, number, product_id))
        .max_by_key(|release| (release.revision, release.build))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mx_master_4_resolves_to_its_newest_release() {
        let release = latest_release("RBM", 27, 0xb042).expect("MX Master 4 release");
        assert_eq!((release.revision, release.build), (3, 19));
    }

    #[test]
    fn another_product_or_firmware_line_gets_nothing() {
        assert_eq!(latest_release("RBM", 27, 0xb034), None);
        assert_eq!(latest_release("RBM", 26, 0xb042), None);
        assert_eq!(latest_release("MPM", 27, 0xb042), None);
    }

    #[test]
    fn pinned_hashes_are_lowercase_sha256_hex() {
        for release in FIRMWARE_RELEASES {
            assert_eq!(release.package_sha256.len(), 64, "{release:?}");
            assert!(
                release
                    .package_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{release:?}"
            );
        }
    }
}
