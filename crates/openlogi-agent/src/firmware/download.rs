//! Bounded download and authenticated extraction of catalog-pinned packages.

use std::io::Cursor;
use std::time::Duration;

use openlogi_core::hid::FirmwareUpdateFailure;
use openlogi_device::firmware::FirmwarePackage;
use openlogi_device_registry::firmware::FirmwareRelease;
use sha2::{Digest as _, Sha256};
use ureq::tls::{RootCerts, TlsConfig};

pub(super) fn download(
    release: &FirmwareRelease,
) -> Result<FirmwarePackage, FirmwareUpdateFailure> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into();
    let mut response = agent
        .get(release.package_url)
        .call()
        .map_err(|error| FirmwareUpdateFailure::Download(error.to_string()))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(release.package_len + 1)
        .read_to_vec()
        .map_err(|error| FirmwareUpdateFailure::Download(error.to_string()))?;
    unpack(release, &bytes)
}

fn unpack(
    release: &FirmwareRelease,
    bytes: &[u8],
) -> Result<FirmwarePackage, FirmwareUpdateFailure> {
    let mut archive = verified_archive(release, bytes)?;
    let mut image_index = None;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| FirmwareUpdateFailure::ImageRejected)?;
        if file
            .name()
            .map_err(|_| FirmwareUpdateFailure::ImageRejected)?
            .ends_with(".rdfu")
            && image_index.replace(index).is_some()
        {
            return Err(FirmwareUpdateFailure::ImageRejected);
        }
    }
    let file = archive
        .by_index(image_index.ok_or(FirmwareUpdateFailure::ImageRejected)?)
        .map_err(|_| FirmwareUpdateFailure::ImageRejected)?;
    // Stream in memory, never extract archive paths to the filesystem.
    FirmwarePackage::read_from(file).map_err(|_| FirmwareUpdateFailure::ImageRejected)
}

fn verified_archive<'a>(
    release: &FirmwareRelease,
    bytes: &'a [u8],
) -> Result<zip::ZipArchive<Cursor<&'a [u8]>>, FirmwareUpdateFailure> {
    // Authenticate the complete archive before interpreting any of its contents.
    if bytes.len() as u64 != release.package_len
        || format!("{:x}", Sha256::digest(bytes)) != release.package_sha256
    {
        return Err(FirmwareUpdateFailure::ImageRejected);
    }
    zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| FirmwareUpdateFailure::ImageRejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in files {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn release(bytes: &[u8]) -> FirmwareRelease {
        FirmwareRelease {
            device_name: "Test device",
            product_ids: &[0xb042],
            prefix: "RBM",
            number: 27,
            revision: 3,
            build: 19,
            package_url: "https://example.invalid/firmware.zip",
            package_sha256: Box::leak(format!("{:x}", Sha256::digest(bytes)).into_boxed_str()),
            package_len: bytes.len() as u64,
        }
    }

    #[test]
    #[ignore = "downloads the catalog-pinned official firmware; never opens a device"]
    fn official_release_download_and_parse() {
        let release =
            openlogi_device_registry::firmware::latest_release("RBM", 27, 0xb042).unwrap();
        download(release).expect("pinned official ZIP downloads, authenticates and parses");
    }

    #[test]
    fn archive_authentication_keeps_entries_in_memory() {
        let bytes = archive(&[
            ("package.json", b"{}"),
            ("../image.rdfu", b"exact firmware bytes"),
        ]);
        let mut verified = verified_archive(&release(&bytes), &bytes).unwrap();
        assert_eq!(verified.len(), 2);
        assert_eq!(verified.by_name("../image.rdfu").unwrap().size(), 20);
    }

    #[test]
    fn altered_or_truncated_package_is_rejected_before_extraction() {
        let bytes = archive(&[("image.rdfu", b"{}")]);
        let pinned = release(&bytes);
        let mut altered = bytes.clone();
        altered[0] ^= 1;
        assert_eq!(
            unpack(&pinned, &altered).unwrap_err(),
            FirmwareUpdateFailure::ImageRejected
        );
        assert_eq!(
            unpack(&pinned, &bytes[..bytes.len() - 1]).unwrap_err(),
            FirmwareUpdateFailure::ImageRejected
        );
    }

    #[test]
    fn missing_ambiguous_or_invalid_image_is_rejected() {
        for files in [
            vec![("package.json", &b"{}"[..])],
            vec![("a.rdfu", &b"{}"[..]), ("b.rdfu", &b"{}"[..])],
            vec![("a.rdfu", &b"invalid"[..])],
        ] {
            let bytes = archive(&files);
            assert_eq!(
                unpack(&release(&bytes), &bytes).unwrap_err(),
                FirmwareUpdateFailure::ImageRejected
            );
        }
    }
}
