use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gpui_updater::{
    DriverEvent, DriverEvents, EngineConfig, Release, StaticManifestSource, UpdateDriver,
    UpdateEngine, Verification, Version,
};

use crate::platform::installation::{InstallationSource, LinuxPackage};

/// Privileged helper shipped in release packages.
pub(crate) const UPDATE_INSTALL_HELPER: &str = "/usr/libexec/openlogi-update-install";

/// Package format required by the installation that owns OpenLogi.
pub(crate) fn package_format(source: InstallationSource) -> Option<&'static str> {
    match source {
        InstallationSource::LinuxPackage(LinuxPackage::Arch) => Some("pkg.tar.zst"),
        InstallationSource::LinuxPackage(LinuxPackage::Deb) => Some("deb"),
        InstallationSource::LinuxPackage(LinuxPackage::Rpm) => Some("rpm"),
        _ => None,
    }
}

/// Validate that the selected artifact belongs to the package manager.
pub(crate) fn matches_package(source: InstallationSource, name: &str) -> bool {
    let Some(format) = package_format(source) else {
        return false;
    };

    let expected = format!(".{format}");
    name.starts_with("openlogi-v") && name.contains("-linux-") && name.ends_with(&expected)
}

/// Expected release asset name for this machine and package source.
pub(crate) fn expected_package_name(
    source: InstallationSource,
    version: &Version,
) -> Option<String> {
    let format = package_format(source)?;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return None,
    };
    Some(format!("openlogi-v{version}-linux-{arch}.{format}"))
}

/// Architecture label consumed by [`StaticManifestSource`].
fn manifest_arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

/// Build the `pkexec` argv that production `install()` runs after download.
pub(crate) fn helper_install_command(
    package: &Path,
    signature: &Path,
    checksum: &str,
    expected_name: &str,
) -> (&'static str, Vec<String>) {
    (
        "pkexec",
        vec![
            UPDATE_INSTALL_HELPER.to_owned(),
            package.to_string_lossy().into_owned(),
            signature.to_string_lossy().into_owned(),
            checksum.to_owned(),
            expected_name.to_owned(),
        ],
    )
}

/// Why the Linux package path stopped, rendered through the locale catalog.
///
/// `Other` carries an error raised by `gpui_updater` or the OS, which the
/// macOS and Windows drivers also surface untranslated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinuxUpdateFailure {
    UnsupportedInstallation,
    UnsupportedArchitecture,
    PackageMismatch,
    StateUnavailable,
    NoUpdateSelected,
    MissingSigningKey,
    HelperMissing,
    MissingSignature,
    InvalidChecksum,
    SignatureDownload(String),
    PolkitStart(String),
    InstallFailed(String),
    Relaunch(String),
    Other(String),
}

impl LinuxUpdateFailure {
    /// The message in the active locale, shown inside `updates.update_failed_message`.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::UnsupportedInstallation => tr!("updates.linux_unsupported_installation"),
            Self::UnsupportedArchitecture => tr!("updates.linux_unsupported_architecture"),
            Self::PackageMismatch => tr!("updates.linux_package_mismatch"),
            Self::StateUnavailable => tr!("updates.linux_state_unavailable"),
            Self::NoUpdateSelected => tr!("updates.linux_no_update_selected"),
            Self::MissingSigningKey => tr!("updates.linux_missing_signing_key"),
            Self::HelperMissing => tr!("updates.linux_helper_missing"),
            Self::MissingSignature => tr!("updates.linux_missing_signature"),
            Self::InvalidChecksum => tr!("updates.linux_invalid_checksum"),
            Self::SignatureDownload(error) => {
                tr!("updates.linux_signature_download_failed", error => error)
            }
            Self::PolkitStart(error) => tr!("updates.linux_polkit_start_failed", error => error),
            Self::InstallFailed(error) => tr!("updates.linux_install_failed", error => error),
            Self::Relaunch(error) => tr!("updates.linux_relaunch_failed", error => error),
            Self::Other(error) => return error.clone(),
        }
        .to_string()
    }
}

/// Report `failure` and close the current check or install.
fn fail(events: &DriverEvents, failure: &LinuxUpdateFailure) {
    events.emit(DriverEvent::Failed(failure.message()));
    events.emit(DriverEvent::Settled);
}

/// Update driver for Linux installations managed by a package manager.
pub(crate) struct LinuxPackageDriver {
    manifest_url: String,
    public_key: Option<String>,
    current_version: Version,
    available: Arc<Mutex<Option<Release>>>,
    events: Option<DriverEvents>,
}

impl LinuxPackageDriver {
    pub(crate) fn new(
        manifest_url: String,
        public_key: Option<String>,
        current_version: Version,
    ) -> Self {
        Self {
            manifest_url,
            public_key,
            current_version,
            available: Arc::new(Mutex::new(None)),
            events: None,
        }
    }
}

impl UpdateDriver for LinuxPackageDriver {
    fn start(&mut self, events: DriverEvents) {
        self.events = Some(events);
    }

    fn check(&mut self) {
        let Some(events) = self.events.clone() else {
            return;
        };

        let url = self.manifest_url.clone();
        let key = self.public_key.clone();
        let version = self.current_version.clone();
        let available = Arc::clone(&self.available);

        events.emit(DriverEvent::Checking);

        std::thread::spawn(move || {
            let installation = InstallationSource::detect();
            let Some(format) = package_format(installation) else {
                fail(&events, &LinuxUpdateFailure::UnsupportedInstallation);
                return;
            };

            let Some(arch) = manifest_arch() else {
                fail(&events, &LinuxUpdateFailure::UnsupportedArchitecture);
                return;
            };

            let source = StaticManifestSource::new(url)
                .os("linux")
                .arch(arch)
                .format(format);

            let mut config = EngineConfig::new(version).verification(Verification::Strict);

            if let Some(key) = key {
                config = config.minisign_public_key(key);
            }

            let engine = UpdateEngine::new(source, config);

            match engine.check() {
                Ok(Some(release)) => {
                    let Some(expected) = expected_package_name(installation, &release.version)
                    else {
                        fail(&events, &LinuxUpdateFailure::UnsupportedArchitecture);
                        return;
                    };

                    if release.asset.name != expected {
                        fail(&events, &LinuxUpdateFailure::PackageMismatch);
                        return;
                    }

                    if let Ok(mut slot) = available.lock() {
                        *slot = Some(release.clone());
                    }
                    events.emit(DriverEvent::Found(release));
                }
                Ok(None) => {
                    if let Ok(mut slot) = available.lock() {
                        *slot = None;
                    }
                    events.emit(DriverEvent::UpToDate);
                }
                Err(error) => {
                    events.emit(DriverEvent::Failed(error.to_string()));
                }
            }

            events.emit(DriverEvent::Settled);
        });
    }

    fn install(&mut self) {
        let Some(events) = self.events.clone() else {
            return;
        };

        let release = if let Ok(available) = self.available.lock() {
            available.clone()
        } else {
            fail(&events, &LinuxUpdateFailure::StateUnavailable);
            return;
        };

        let Some(release) = release else {
            fail(&events, &LinuxUpdateFailure::NoUpdateSelected);
            return;
        };

        let url = self.manifest_url.clone();
        let key = self.public_key.clone();
        let version = self.current_version.clone();

        std::thread::spawn(move || {
            let result = install_verified_package(release, url, key, version, &events);
            match result {
                Ok(version) => events.emit(DriverEvent::ReadyToRelaunch(version)),
                Err(failure) => events.emit(DriverEvent::Failed(failure.message())),
            }
            events.emit(DriverEvent::Settled);
        });
    }

    fn relaunch(&mut self) {
        use std::os::unix::process::CommandExt;

        let executable = "/usr/bin/openlogi-desktop";

        // exec replaces the current process with the newly installed binary.
        // It avoids opening a second instance while the old one is running.
        let error = std::process::Command::new(executable).exec();

        if let Some(events) = &self.events {
            fail(events, &LinuxUpdateFailure::Relaunch(error.to_string()));
        }
    }
}

/// Download, verify, and elevate-install one signed Linux package release.
fn install_verified_package(
    release: Release,
    url: String,
    key: Option<String>,
    version: Version,
    events: &DriverEvents,
) -> Result<Version, LinuxUpdateFailure> {
    let installation = InstallationSource::detect();
    let Some(format) = package_format(installation) else {
        return Err(LinuxUpdateFailure::UnsupportedInstallation);
    };

    let Some(arch) = manifest_arch() else {
        return Err(LinuxUpdateFailure::UnsupportedArchitecture);
    };

    let expected_name = expected_package_name(installation, &release.version)
        .ok_or(LinuxUpdateFailure::UnsupportedArchitecture)?;

    if release.asset.name != expected_name || !matches_package(installation, &release.asset.name) {
        return Err(LinuxUpdateFailure::PackageMismatch);
    }

    let key = key.ok_or(LinuxUpdateFailure::MissingSigningKey)?;
    let temporary =
        tempfile::tempdir().map_err(|error| LinuxUpdateFailure::Other(error.to_string()))?;

    let source = StaticManifestSource::new(url)
        .os("linux")
        .arch(arch)
        .format(format);

    let config = EngineConfig::new(version)
        .verification(Verification::Strict)
        .minisign_public_key(key)
        .install_root(temporary.path().join("openlogi"))
        .download_dir(temporary.path().join("downloads"));

    let engine = UpdateEngine::new(source, config);

    events.emit(DriverEvent::Downloading {
        downloaded: 0,
        total: None,
    });

    let artifact = engine
        .download(&release, |downloaded, total| {
            events.emit(DriverEvent::Downloading { downloaded, total });
        })
        .map_err(|error| LinuxUpdateFailure::Other(error.to_string()))?;

    events.emit(DriverEvent::Installing);

    // A privileged helper is shipped with newer OpenLogi packages.
    // Older installations must be upgraded manually once.
    if !Path::new(UPDATE_INSTALL_HELPER).is_file() {
        return Err(LinuxUpdateFailure::HelperMissing);
    }

    let signature_url = release
        .signature_url
        .as_deref()
        .filter(|url| url.starts_with("https://"))
        .ok_or(LinuxUpdateFailure::MissingSignature)?;

    let checksum = release
        .sha256
        .as_deref()
        .filter(|hash| hash.len() == 64 && hash.as_bytes().iter().all(u8::is_ascii_hexdigit))
        .ok_or(LinuxUpdateFailure::InvalidChecksum)?;

    let signature: PathBuf = temporary.path().join(format!("{expected_name}.minisig"));

    let download_status = std::process::Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--output",
        ])
        .arg(&signature)
        .arg("--")
        .arg(signature_url)
        .status()
        .map_err(|error| LinuxUpdateFailure::SignatureDownload(error.to_string()))?;

    if !download_status.success() {
        return Err(LinuxUpdateFailure::SignatureDownload(
            download_status.to_string(),
        ));
    }

    // The helper copies into a root-owned directory, then verifies
    // minisign and SHA-256 before invoking the package manager.
    events.emit(DriverEvent::Installing);

    let (program, args) = helper_install_command(&artifact, &signature, checksum, &expected_name);
    let status = std::process::Command::new(program)
        .args(&args)
        .status()
        .map_err(|error| LinuxUpdateFailure::PolkitStart(error.to_string()))?;

    if !status.success() {
        return Err(LinuxUpdateFailure::InstallFailed(status.to_string()));
    }

    Ok(release.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_package_formats_for_supported_sources() {
        assert_eq!(
            package_format(InstallationSource::LinuxPackage(LinuxPackage::Arch)),
            Some("pkg.tar.zst")
        );
        assert_eq!(
            package_format(InstallationSource::LinuxPackage(LinuxPackage::Deb)),
            Some("deb")
        );
        assert_eq!(
            package_format(InstallationSource::LinuxPackage(LinuxPackage::Rpm)),
            Some("rpm")
        );
        assert_eq!(package_format(InstallationSource::Unknown), None);
    }

    #[test]
    fn rejects_wrong_package_format() {
        let arch = InstallationSource::LinuxPackage(LinuxPackage::Arch);
        let deb = InstallationSource::LinuxPackage(LinuxPackage::Deb);
        let rpm = InstallationSource::LinuxPackage(LinuxPackage::Rpm);

        assert!(matches_package(
            arch,
            "openlogi-v0.8.21-linux-amd64.pkg.tar.zst"
        ));
        assert!(matches_package(deb, "openlogi-v0.8.21-linux-amd64.deb"));
        assert!(matches_package(rpm, "openlogi-v0.8.21-linux-amd64.rpm"));

        assert!(!matches_package(arch, "openlogi-v0.8.21-linux-amd64.deb"));
        assert!(!matches_package(deb, "openlogi-v0.8.21-linux-amd64.rpm"));
        assert!(!matches_package(rpm, "untrusted.rpm"));
    }

    #[test]
    fn builds_expected_package_names() {
        let version = Version::parse("0.9.1").unwrap();
        let name = expected_package_name(
            InstallationSource::LinuxPackage(LinuxPackage::Deb),
            &version,
        )
        .unwrap();
        assert!(
            name == "openlogi-v0.9.1-linux-amd64.deb" || name == "openlogi-v0.9.1-linux-arm64.deb"
        );
    }

    #[test]
    fn failures_render_in_the_active_locale() {
        let detail = "exit status: 126";
        let failures = [
            LinuxUpdateFailure::UnsupportedInstallation,
            LinuxUpdateFailure::UnsupportedArchitecture,
            LinuxUpdateFailure::PackageMismatch,
            LinuxUpdateFailure::StateUnavailable,
            LinuxUpdateFailure::NoUpdateSelected,
            LinuxUpdateFailure::MissingSigningKey,
            LinuxUpdateFailure::HelperMissing,
            LinuxUpdateFailure::MissingSignature,
            LinuxUpdateFailure::InvalidChecksum,
            LinuxUpdateFailure::SignatureDownload(detail.to_owned()),
            LinuxUpdateFailure::PolkitStart(detail.to_owned()),
            LinuxUpdateFailure::InstallFailed(detail.to_owned()),
            LinuxUpdateFailure::Relaunch(detail.to_owned()),
        ];

        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let english: Vec<String> = failures.iter().map(LinuxUpdateFailure::message).collect();
        rust_i18n::set_locale("de");
        let german: Vec<String> = failures.iter().map(LinuxUpdateFailure::message).collect();
        rust_i18n::set_locale("en");

        for ((failure, en), de) in failures.iter().zip(&english).zip(&german) {
            // A missing key renders as its own path.
            assert!(
                !en.contains("updates.linux_"),
                "{failure:?} has no English entry"
            );
            assert!(
                !de.contains("updates.linux_"),
                "{failure:?} has no German entry"
            );
            assert_ne!(en, de, "{failure:?} is not translated");
            if matches!(
                failure,
                LinuxUpdateFailure::SignatureDownload(_)
                    | LinuxUpdateFailure::PolkitStart(_)
                    | LinuxUpdateFailure::InstallFailed(_)
                    | LinuxUpdateFailure::Relaunch(_)
            ) {
                assert!(de.contains(detail), "{failure:?} drops its detail: {de}");
            }
        }
    }

    #[test]
    fn engine_failures_pass_through_unchanged() {
        let failure = LinuxUpdateFailure::Other("no release asset matched".to_owned());
        assert_eq!(failure.message(), "no release asset matched");
    }

    #[test]
    fn builds_pkexec_helper_command_without_shell_interpolation() {
        // Avoid the contiguous 0-f alphabet that fixture-report-hex-owner flags.
        let checksum = "deadbeef".repeat(8);
        let (program, args) = helper_install_command(
            Path::new("/tmp/openlogi-v0.9.1-linux-amd64.deb"),
            Path::new("/tmp/openlogi-v0.9.1-linux-amd64.deb.minisig"),
            &checksum,
            "openlogi-v0.9.1-linux-amd64.deb",
        );
        assert_eq!(program, "pkexec");
        assert_eq!(
            args,
            [
                UPDATE_INSTALL_HELPER,
                "/tmp/openlogi-v0.9.1-linux-amd64.deb",
                "/tmp/openlogi-v0.9.1-linux-amd64.deb.minisig",
                checksum.as_str(),
                "openlogi-v0.9.1-linux-amd64.deb",
            ]
        );
    }
}
