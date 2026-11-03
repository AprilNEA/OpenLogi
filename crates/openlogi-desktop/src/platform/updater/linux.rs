#[cfg(test)]
use std::path::Path;

use crate::platform::installation::{InstallationSource, LinuxPackage};

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

/// The privileged command to install an already verified Arch package.
#[cfg(test)]
pub(crate) fn arch_install_command(package: &Path) -> (&'static str, Vec<String>) {
    (
        "pacman",
        vec![
            "-U".to_owned(),
            "--".to_owned(),
            package.to_string_lossy().into_owned(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_arch_package_format() {
        assert_eq!(
            package_format(InstallationSource::LinuxPackage(LinuxPackage::Arch)),
            Some("pkg.tar.zst")
        );
    }

    #[test]
    fn rejects_wrong_package_format() {
        let arch = InstallationSource::LinuxPackage(LinuxPackage::Arch);

        assert!(matches_package(
            arch,
            "openlogi-v0.8.21-linux-amd64.pkg.tar.zst"
        ));

        assert!(!matches_package(arch, "openlogi-v0.8.21-linux-amd64.deb"));

        assert!(!matches_package(arch, "untrusted.pkg.tar.zst"));
    }

    #[test]
    fn builds_pacman_command_without_shell_interpolation() {
        let (program, args) = arch_install_command(Path::new("/tmp/update.pkg.tar.zst"));
        assert_eq!(program, "pacman");
        assert_eq!(args, ["-U", "--", "/tmp/update.pkg.tar.zst"]);
    }
}

use std::sync::{Arc, Mutex};

use gpui_updater::{
    DriverEvent, DriverEvents, EngineConfig, Release, StaticManifestSource, UpdateDriver,
    UpdateEngine, Verification, Version,
};

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
            let Some(format) = package_format(InstallationSource::detect())
                .filter(|format| *format == "pkg.tar.zst")
            else {
                events.emit(DriverEvent::Failed(
                    "This Linux installation is not managed by a supported package manager"
                        .to_owned(),
                ));
                events.emit(DriverEvent::Settled);
                return;
            };

            let source = StaticManifestSource::new(url)
                .os("linux")
                .arch(match std::env::consts::ARCH {
                    "aarch64" => "arm64",
                    _ => "x86_64",
                })
                .format(format);

            let mut config = EngineConfig::new(version).verification(Verification::Strict);

            if let Some(key) = key {
                config = config.minisign_public_key(key);
            }

            let engine = UpdateEngine::new(source, config);

            match engine.check() {
                Ok(Some(release)) => {
                    let arch = match std::env::consts::ARCH {
                        "x86_64" => "amd64",
                        "aarch64" => "arm64",
                        _ => {
                            events.emit(DriverEvent::Failed(
                                "Unsupported Linux architecture".to_owned(),
                            ));
                            events.emit(DriverEvent::Settled);
                            return;
                        }
                    };

                    let expected =
                        format!("openlogi-v{}-linux-{arch}.pkg.tar.zst", release.version);

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

        let release = match self.available.lock() {
            Ok(available) => available.clone(),
            Err(_) => {
                events.emit(DriverEvent::Failed("Update state unavailable".into()));
                events.emit(DriverEvent::Settled);
                return;
            }
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
            let result = (|| -> Result<Version, String> {
                let installation = InstallationSource::detect();

                if installation != InstallationSource::LinuxPackage(LinuxPackage::Arch) {
                    return Err(
                        "In-app installation currently supports Arch Linux packages only".into(),
                    );
                }

                let arch = match std::env::consts::ARCH {
                    "x86_64" => "amd64",
                    "aarch64" => "arm64",
                    _ => return Err("Unsupported architecture".into()),
                };

                let expected_name =
                    format!("openlogi-v{}-linux-{arch}.pkg.tar.zst", release.version);

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
                    .arch(match arch {
                        "amd64" => "x86_64",
                        _ => "arm64",
                    })
                    .format("pkg.tar.zst");

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
                let helper = "/usr/libexec/openlogi-update-install";
                if !std::path::Path::new(helper).is_file() {
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
                    .filter(|hash| {
                        hash.len() == 64 && hash.as_bytes().iter().all(u8::is_ascii_hexdigit)
                    })
                    .ok_or("Missing or invalid SHA-256 checksum")?;

                let signature = temporary.path().join(format!("{expected_name}.minisig"));

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
                // minisign and SHA-256 before executing pacman.
                events.emit(DriverEvent::Installing);

                let status = std::process::Command::new("pkexec")
                    .arg(helper)
                    .arg(&artifact)
                    .arg(&signature)
                    .arg(checksum)
                    .arg(&expected_name)
                    .status()
                    .map_err(|error| format!("Could not start polkit: {error}"))?;

                if !status.success() {
                    return Err(format!(
                        "Package installation failed or authorization was denied: {status}"
                    ));
                }

                Ok(release.version)
            })();

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
