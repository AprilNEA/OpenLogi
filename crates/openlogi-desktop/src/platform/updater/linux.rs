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

/// Backend de actualizaciones para instalaciones Linux gestionadas por paquetes.
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
                events.emit(DriverEvent::Failed(
                    "This Linux installation is not managed by a supported package manager"
                        .to_owned(),
                ));
                events.emit(DriverEvent::Settled);
                return;
            };

            let Some(arch) = manifest_arch() else {
                events.emit(DriverEvent::Failed(
                    "Unsupported Linux architecture".to_owned(),
                ));
                events.emit(DriverEvent::Settled);
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
                        events.emit(DriverEvent::Failed(
                            "Unsupported Linux architecture".to_owned(),
                        ));
                        events.emit(DriverEvent::Settled);
                        return;
                    };

                    if release.asset.name != expected {
                        events.emit(DriverEvent::Failed(
                            "Update package name or architecture does not match".to_owned(),
                        ));
                        events.emit(DriverEvent::Settled);
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
            events.emit(DriverEvent::Failed("Update state unavailable".into()));
            events.emit(DriverEvent::Settled);
            return;
        };

        let Some(release) = release else {
            events.emit(DriverEvent::Failed("No update selected".into()));
            events.emit(DriverEvent::Settled);
            return;
        };

        let url = self.manifest_url.clone();
        let key = self.public_key.clone();
        let version = self.current_version.clone();

        std::thread::spawn(move || {
            let result = install_verified_package(release, url, key, version, &events);
            match result {
                Ok(version) => events.emit(DriverEvent::ReadyToRelaunch(version)),
                Err(error) => events.emit(DriverEvent::Failed(error)),
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
            events.emit(DriverEvent::Failed(format!(
                "Could not relaunch updated OpenLogi: {error}"
            )));
            events.emit(DriverEvent::Settled);
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
) -> Result<Version, String> {
    let installation = InstallationSource::detect();
    let Some(format) = package_format(installation) else {
        return Err(
            "In-app installation supports Debian, RPM, and Arch Linux packages only".into(),
        );
    };

    let Some(arch) = manifest_arch() else {
        return Err("Unsupported architecture".into());
    };

    let expected_name =
        expected_package_name(installation, &release.version).ok_or("Unsupported architecture")?;

    if release.asset.name != expected_name {
        return Err("Unexpected package name, version or architecture".into());
    }

    if !matches_package(installation, &release.asset.name) {
        return Err("Package format mismatch".into());
    }

    let key = key.ok_or("No trusted update signing key embedded")?;
    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;

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
        .map_err(|error| error.to_string())?;

    events.emit(DriverEvent::Installing);

    // A privileged helper is shipped with newer OpenLogi packages.
    // Older installations must be upgraded manually once.
    if !Path::new(UPDATE_INSTALL_HELPER).is_file() {
        return Err(
            "The secure update helper is not installed. Update OpenLogi manually once to enable in-app installation."
                .to_owned(),
        );
    }

    let signature_url = release
        .signature_url
        .as_deref()
        .filter(|url| url.starts_with("https://"))
        .ok_or("No HTTPS minisign signature URL in the release")?;

    let checksum = release
        .sha256
        .as_deref()
        .filter(|hash| hash.len() == 64 && hash.as_bytes().iter().all(u8::is_ascii_hexdigit))
        .ok_or("Missing or invalid SHA-256 checksum")?;

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
        .map_err(|error| format!("Could not download signature: {error}"))?;

    if !download_status.success() {
        return Err(format!(
            "Could not download package signature: {download_status}"
        ));
    }

    // The helper copies into a root-owned directory, then verifies
    // minisign and SHA-256 before invoking the package manager.
    events.emit(DriverEvent::Installing);

    let (program, args) = helper_install_command(&artifact, &signature, checksum, &expected_name);
    let status = std::process::Command::new(program)
        .args(&args)
        .status()
        .map_err(|error| format!("Could not start polkit: {error}"))?;

    if !status.success() {
        return Err(format!(
            "Package installation failed or authorization was denied: {status}"
        ));
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
