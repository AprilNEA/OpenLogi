//! GPUI glue for the Linux package update path.
//!
//! Runs the [`linux_release_check`] GitHub lookup alongside the engine's own
//! (deliberately asset-less, see `super`) manifest check, and drives the
//! [`linux_package_install`] verified download + install from an explicit
//! "Install Update" click. Both only ever act when the running build targets
//! Linux — a runtime check, not a `cfg`, so the exact same code compiles and
//! is exercised by tests on every host; it simply never fires off Linux.
//! Neither polls, and install never runs unattended: it mirrors the
//! engine's own opt-in, explicit-click shape (see module docs on
//! [`super::install`]).

use gpui::{App, Global};
use gpui_updater::Version;

use super::linux_package_install;
use super::linux_release_check::{self, LinuxUpdateCheck, ReleaseAsset};
use crate::platform::installation::{Installation, InstallationSource};

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

/// Run the GitHub-release check in the background. A no-op off Linux, and
/// while a check or install is already in flight.
pub fn check_in_background(cx: &mut App) {
    if !cfg!(target_os = "linux") {
        return;
    }
    if matches!(
        cx.try_global::<LinuxUpdateStatus>(),
        Some(LinuxUpdateStatus::Checking | LinuxUpdateStatus::Installing)
    ) {
        return;
    }
    cx.set_global(LinuxUpdateStatus::Checking);
    cx.spawn(async move |cx| {
        #[expect(
            clippy::expect_used,
            reason = "CARGO_PKG_VERSION is cargo-provided and always valid semver"
        )]
        let current = Version::parse(env!("CARGO_PKG_VERSION")).expect("valid embedded version");
        let result = cx
            .background_executor()
            .spawn(async move { linux_release_check::check_latest_release(&current) })
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

/// Download, verify and install the update [`check_in_background`] found,
/// then report [`LinuxUpdateStatus::Staged`] for the restart prompt. A no-op
/// off Linux, without a matched package installation, or without an
/// [`LinuxUpdateStatus::Available`] result to act on.
pub fn install(cx: &mut App) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Some(LinuxUpdateStatus::Available { version, assets }) =
        cx.try_global::<LinuxUpdateStatus>().cloned()
    else {
        return;
    };
    let Installation::Detected(InstallationSource::LinuxPackage(source)) =
        *cx.global::<Installation>()
    else {
        cx.set_global(LinuxUpdateStatus::Errored(
            "installation source is not a recognized Linux package".to_string(),
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

fn download_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    ureq::get(url)
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
/// outside a genuine, user-initiated install (see [`linux_package_install`]
/// for the unit-tested pure logic this calls into).
fn download_verify_and_install(
    assets: &[ReleaseAsset],
    version: &Version,
    source: crate::platform::installation::LinuxPackage,
) -> Result<(), String> {
    #[expect(
        clippy::expect_used,
        reason = "CARGO_PKG_VERSION is cargo-provided and always valid semver"
    )]
    let current = Version::parse(env!("CARGO_PKG_VERSION")).expect("valid embedded version");
    linux_package_install::ensure_newer(version, &current)?;
    linux_package_install::require_install_tools(
        source,
        &std::env::var_os("PATH").unwrap_or_default(),
    )?;
    let matched = linux_package_install::match_assets(assets, version, source)?;

    let package_bytes = download_bytes(&matched.package.browser_download_url, PACKAGE_LIMIT)?;
    let signature_text = String::from_utf8(download_bytes(
        &matched.signature.browser_download_url,
        METADATA_LIMIT,
    )?)
    .map_err(|error| format!("{} is not valid UTF-8: {error}", matched.signature.name))?;
    let checksums_text = String::from_utf8(download_bytes(
        &matched.checksums.browser_download_url,
        METADATA_LIMIT,
    )?)
    .map_err(|error| format!("SHA256SUMS is not valid UTF-8: {error}"))?;

    let expected_hex = linux_package_install::find_checksum(&checksums_text, &matched.package.name)
        .ok_or_else(|| format!("SHA256SUMS has no entry for {}", matched.package.name))?;
    if !linux_package_install::verify_checksum(&package_bytes, expected_hex) {
        return Err(format!(
            "checksum verification failed for {}",
            matched.package.name
        ));
    }

    linux_package_install::verify_minisign(&package_bytes, &signature_text, &matched.package.name)?;

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

    let command = linux_package_install::pkexec_install_command(source, &package_path);
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
