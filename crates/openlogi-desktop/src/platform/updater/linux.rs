//! GPUI glue for the Linux package update path.
//!
//! Runs the [`release_check`] GitHub lookup alongside the engine's own
//! (deliberately asset-less, see `super`) manifest check, and drives the
//! [`package_install`] verified download + install from an explicit
//! "Install Update" click. Both only ever act when the running build targets
//! Linux — a runtime check, not a `cfg`, so the exact same code compiles and
//! is exercised by tests on every host; it simply never fires off Linux.
//! Neither polls, and install never runs unattended: it mirrors the
//! engine's own opt-in, explicit-click shape (see module docs on
//! [`super::install`]).

use std::sync::LazyLock;
use std::time::Duration;

use gpui::{App, Global};
use gpui_updater::Version;

use self::release_check::{LinuxUpdateCheck, ReleaseAsset};
use crate::platform::installation::{Installation, InstallationSource, LinuxPackage};

pub mod package_install;
pub mod release_check;

/// Observable state of the Linux GitHub-release check/install — parallel to,
/// and independent from, the engine's [`gpui_updater::UpdateStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LinuxUpdateStatus {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available {
        version: Version,
        assets: Vec<ReleaseAsset>,
    },
    Installing,
    Staged {
        version: Version,
    },
    Errored(String),
}

impl Global for LinuxUpdateStatus {}

/// Run the GitHub-release check for the channel this launch follows in the
/// background. A no-op off Linux, while a check or install is in flight, and
/// once an install is staged: a later check would compare against the old
/// running version and offer the installed package again.
pub fn check_in_background(cx: &mut App) {
    if !cfg!(target_os = "linux") {
        return;
    }
    if matches!(
        cx.try_global::<LinuxUpdateStatus>(),
        Some(
            LinuxUpdateStatus::Checking
                | LinuxUpdateStatus::Installing
                | LinuxUpdateStatus::Staged { .. }
        )
    ) {
        return;
    }
    cx.set_global(LinuxUpdateStatus::Checking);
    let channel = super::channel_in_use(cx);
    cx.spawn(async move |cx| {
        let current = super::running_version();
        let result = cx
            .background_executor()
            .spawn(async move { release_check::check_latest_release(&HTTP, channel, &current) })
            .await;
        cx.update(|cx| {
            cx.set_global(match result {
                Ok(LinuxUpdateCheck::UpToDate) => LinuxUpdateStatus::UpToDate,
                Ok(LinuxUpdateCheck::Available(release)) => LinuxUpdateStatus::Available {
                    version: release.version,
                    assets: release.assets,
                },
                Err(error) => LinuxUpdateStatus::Errored(error),
            });
        });
    })
    .detach();
}

/// The package family an in-app update installs through, or `None` when
/// this install cannot update in-app. Only a package the detected package
/// manager owns qualifies (not Nix, `Unknown`, or a still-`Detecting` probe),
/// and only in a build that embeds the update signing key: without it every
/// download fails verification. The one gate for the Settings button and
/// [`install`].
pub fn installable_package(installation: Installation) -> Option<LinuxPackage> {
    installable_package_with(installation, super::minisign_public_key())
}

/// [`installable_package`] with the embedded key passed in, so tests cover a
/// build with and without one.
fn installable_package_with(
    installation: Installation,
    update_key: Option<&str>,
) -> Option<LinuxPackage> {
    let Installation::Detected(InstallationSource::LinuxPackage(package)) = installation else {
        return None;
    };
    update_key.is_some().then_some(package)
}

/// Download, verify and install the update [`check_in_background`] found,
/// then report [`LinuxUpdateStatus::Staged`] for the restart prompt. A no-op
/// off Linux, or without an [`LinuxUpdateStatus::Available`] result to act
/// on; an error when [`installable_package`] rejects this install.
pub fn install(cx: &mut App) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Some(LinuxUpdateStatus::Available { version, assets }) =
        cx.try_global::<LinuxUpdateStatus>().cloned()
    else {
        return;
    };
    let Some(source) = installable_package(*cx.global::<Installation>()) else {
        cx.set_global(LinuxUpdateStatus::Errored(
            "this install cannot update in-app: it is not a recognized Linux package, \
             or this build embeds no update signing key"
                .to_string(),
        ));
        return;
    };
    cx.set_global(LinuxUpdateStatus::Installing);
    cx.spawn(async move |cx| {
        let staged_version = version.clone();
        let result = cx
            .background_executor()
            .spawn(async move { download_verify_and_install(&assets, &version, source) })
            .await;
        cx.update(|cx| {
            cx.set_global(match result {
                Ok(()) => LinuxUpdateStatus::Staged {
                    version: staged_version,
                },
                Err(error) => LinuxUpdateStatus::Errored(error),
            });
        });
    })
    .detach();
}

/// Relaunch into the just-installed package. The package manager replaced
/// the binary at this process's own executable path, so a plain restart
/// picks it up — there is no staged-path swap to set up, unlike the engine's
/// own install.
pub fn restart(cx: &mut App) {
    cx.restart();
}

/// A fresh, randomly-named temporary directory, owner-only on Unix (`0o700`)
/// so no other local account can read or write into it while the downloaded
/// package sits there awaiting verification and install.
#[cfg(unix)]
fn private_tempdir() -> Result<tempfile::TempDir, String> {
    use std::os::unix::fs::PermissionsExt as _;

    tempfile::Builder::new()
        .prefix("openlogi-update-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .map_err(|error| format!("failed to create a private download directory: {error}"))
}

#[cfg(not(unix))]
fn private_tempdir() -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix("openlogi-update-")
        .tempdir()
        .map_err(|error| format!("failed to create a private download directory: {error}"))
}

/// Read limit for the signature and `SHA256SUMS` downloads, which are tiny.
const METADATA_LIMIT: u64 = 1024 * 1024;

/// Read limit for the package itself; release packages are ~40 MB, well past
/// ureq's 10 MB default.
const PACKAGE_LIMIT: u64 = 512 * 1024 * 1024;

/// Whole-request deadline for the release lookup and the small downloads.
const METADATA_TIMEOUT: Duration = Duration::from_secs(60);

/// Whole-request deadline for the package download, sized for a ~40 MB
/// package on a slow link.
const PACKAGE_TIMEOUT: Duration = Duration::from_mins(15);

/// The one HTTP agent for the release lookup and every download. ureq sets no
/// timeouts by default, and a stalled request would leave the status in
/// `Checking` or `Installing` — which blocks every later check — until the
/// app is relaunched.
static HTTP: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_global(Some(METADATA_TIMEOUT))
        .build()
        .into()
});

fn download_bytes(url: &str, limit: u64, timeout: Duration) -> Result<Vec<u8>, String> {
    HTTP.get(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call()
        .map_err(|error| format!("download of {url} failed: {error}"))?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|error| format!("failed to read downloaded bytes from {url}: {error}"))
}

/// The verified install path `install.sh`'s `install_release` runs: download
/// the matched package, `.minisig` and `SHA256SUMS`, verify both, then hand
/// the verified file to the package manager through `pkexec`. Blocking —
/// callers run it off the UI thread. Not unit-tested: it is real network I/O
/// plus a real privileged package-manager invocation, which must never run
/// outside a genuine, user-initiated install (see [`package_install`]
/// for the unit-tested pure logic this calls into).
fn download_verify_and_install(
    assets: &[ReleaseAsset],
    version: &Version,
    source: LinuxPackage,
) -> Result<(), String> {
    package_install::ensure_newer(version, &super::running_version())?;
    package_install::require_install_tools(source)?;
    let matched = package_install::match_assets(assets, version, source)?;

    let package_bytes = download_bytes(
        &matched.package.browser_download_url,
        PACKAGE_LIMIT,
        PACKAGE_TIMEOUT,
    )?;
    let signature_text = String::from_utf8(download_bytes(
        &matched.signature.browser_download_url,
        METADATA_LIMIT,
        METADATA_TIMEOUT,
    )?)
    .map_err(|error| format!("{} is not valid UTF-8: {error}", matched.signature.name))?;
    let checksums_text = String::from_utf8(download_bytes(
        &matched.checksums.browser_download_url,
        METADATA_LIMIT,
        METADATA_TIMEOUT,
    )?)
    .map_err(|error| format!("SHA256SUMS is not valid UTF-8: {error}"))?;

    let expected_hex = package_install::find_checksum(&checksums_text, &matched.package.name)
        .ok_or_else(|| format!("SHA256SUMS has no entry for {}", matched.package.name))?;
    if !package_install::verify_checksum(&package_bytes, expected_hex) {
        return Err(format!(
            "checksum verification failed for {}",
            matched.package.name
        ));
    }

    package_install::verify_minisign(&package_bytes, &signature_text, &matched.package.name)?;

    // A private, randomly-named, owner-only directory — not the shared,
    // world-writable `std::env::temp_dir()` with a predictable filename
    // (the asset's own name, known in advance from the release). A
    // predictable path there would let another local process race to swap
    // the file between this verification and the `pkexec` install below.
    // Dropped (and removed) when this function returns either way.
    let download_dir = private_tempdir()?;
    let package_path = download_dir.path().join(&matched.package.name);
    std::fs::write(&package_path, &package_bytes)
        .map_err(|error| format!("failed to write {}: {error}", package_path.display()))?;

    let command = package_install::pkexec_install_command(source, &package_path);
    let [program, rest @ ..] = command.as_slice() else {
        return Err("empty install command".to_string());
    };
    let status = std::process::Command::new(program)
        .args(rest)
        .status()
        .map_err(|error| format!("failed to run {program}: {error}"))?;
    if !status.success() {
        return Err("package-manager install failed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_detected_linux_package_in_a_keyed_build_installs_in_app() {
        let rpm = Installation::Detected(InstallationSource::LinuxPackage(LinuxPackage::Rpm));
        assert_eq!(
            installable_package_with(rpm, Some("key")),
            Some(LinuxPackage::Rpm)
        );
        assert_eq!(installable_package_with(rpm, None), None);
        for installation in [
            Installation::Detecting,
            Installation::Detected(InstallationSource::Nix),
            Installation::Detected(InstallationSource::Unknown),
        ] {
            assert_eq!(
                installable_package_with(installation, Some("key")),
                None,
                "{installation:?}"
            );
        }
    }
}
