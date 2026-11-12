//! The Linux package update path.
//!
//! The engine's `latest.json` carries no Linux asset: `.deb`, `.rpm` and
//! `.pkg.tar.zst` installs update through the OS package manager, not the
//! in-app engine (see `xtask/src/commands/release/latest_json.rs`), so on
//! Linux the engine could only ever report that no release asset matched. On
//! Linux [`super::install`] therefore installs this path, and every update
//! check runs here instead: [`release_check`] asks GitHub Releases for the
//! followed channel's newest version, and an explicit "Download & Install"
//! click runs [`package_install`]'s verified download and hands the package to
//! the package manager through `pkexec`. Neither polls, and install never runs
//! unattended: `pkexec` always asks for administrator approval.
//!
//! [`LinuxUpdate`] is the path's state and its one transition authority. It
//! does no I/O: the GPUI glue below runs the network and package-manager work
//! on the background executor and reports each result with the [`Attempt`]
//! that started it, so a result for any attempt but the current one changes
//! nothing.

use std::sync::LazyLock;
use std::time::Duration;

use gpui::{App, Global};
use gpui_updater::Version;

use self::package_install::MatchedAssets;
use self::release_check::{LatestRelease, LinuxUpdateCheck};
use crate::platform::installation::{Installation, InstallationSource, LinuxPackage};

pub mod package_install;
pub mod release_check;

/// The identity of one check or install. [`LinuxUpdate`] hands out a fresh
/// one for each, and accepts a result only with the identity of the attempt
/// still in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt(u64);

/// What the Linux update path reports on Settings → Updates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LinuxUpdateStatus {
    /// No check has run this launch.
    #[default]
    Idle,
    /// A release check is in flight.
    Checking(Attempt),
    /// The followed channel offers nothing newer than this build.
    UpToDate,
    /// The followed channel offers a newer release.
    Available(LatestRelease),
    /// `version` is downloading, verifying or installing.
    Installing { attempt: Attempt, version: Version },
    /// The package manager installed `version`; restarting runs it.
    Staged { version: Version },
    /// The last check or install failed.
    Errored(LinuxUpdateError),
}

/// Why a check or install failed, in the terms Settings → Updates shows. The
/// technical detail goes to the log instead: it is English, and names hosts,
/// paths and exit codes the user cannot act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxUpdateError {
    /// GitHub Releases was unreachable or answered with unreadable metadata.
    Check,
    /// The install needs this tool, and it is not at its fixed path.
    MissingTool(&'static str),
    /// The package, its signature or `SHA256SUMS` did not download.
    Download,
    /// The download failed its checksum or signature check, or is not newer
    /// than this build.
    Verification,
    /// `pkexec` obtained no authorization: the dialog was dismissed, or
    /// authentication failed.
    Authorization,
    /// The package manager failed, or its input could not be prepared.
    Install,
}

/// Whether, and how, this install can install an available release in-app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOffer {
    /// Everything a verified in-app install needs is in place.
    Ready(Box<InstallPlan>),
    /// Installation-source detection has not finished yet; an open Settings
    /// window redraws when it does.
    Detecting,
    /// This install updates in-app, but the release lacks this host's
    /// package, its signature or `SHA256SUMS`.
    NotPublished,
    /// Not an install the app updates: Nix, an unidentified copy, or a build
    /// without the update signing key. Whatever installed it updates it.
    PackageManager,
}

/// The inputs of one verified in-app install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    /// The release to install.
    pub version: Version,
    /// The package family the package manager owns this install as.
    pub package: LinuxPackage,
    /// The package, its `.minisig` and the release's `SHA256SUMS`.
    pub assets: MatchedAssets,
    /// The update public key this build embeds, the only key a package is
    /// verified against.
    pub public_key: &'static str,
}

/// The Linux update path's state: the status Settings → Updates shows, and
/// the counter [`Attempt`]s come from. A GPUI global, installed only on
/// Linux; its presence is what routes update checks and the Settings hero to
/// this path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxUpdate {
    status: LinuxUpdateStatus,
    attempts: u64,
    /// The update public key this build embeds. Without one every download
    /// would fail verification, so no install is offered.
    public_key: Option<&'static str>,
}

impl Global for LinuxUpdate {}

impl LinuxUpdate {
    /// The path at launch, verifying packages against `public_key`.
    #[must_use]
    pub fn new(public_key: Option<&'static str>) -> Self {
        Self {
            status: LinuxUpdateStatus::Idle,
            attempts: 0,
            public_key,
        }
    }

    /// What Settings → Updates shows.
    #[must_use]
    pub fn status(&self) -> &LinuxUpdateStatus {
        &self.status
    }

    fn next_attempt(&mut self) -> Attempt {
        self.attempts += 1;
        Attempt(self.attempts)
    }

    /// Start a release check. `None` while a check or install is in flight,
    /// and once an install is staged: a later check would compare against
    /// the old running version and offer the installed package again.
    pub fn begin_check(&mut self) -> Option<Attempt> {
        if matches!(
            self.status,
            LinuxUpdateStatus::Checking(_)
                | LinuxUpdateStatus::Installing { .. }
                | LinuxUpdateStatus::Staged { .. }
        ) {
            return None;
        }
        let attempt = self.next_attempt();
        self.status = LinuxUpdateStatus::Checking(attempt);
        Some(attempt)
    }

    /// Apply the result of the check `attempt` started. Returns whether it
    /// applied: unless `attempt` is the check in flight, the result is stale
    /// and changes nothing.
    pub fn finish_check(
        &mut self,
        attempt: Attempt,
        result: Result<LinuxUpdateCheck, LinuxUpdateError>,
    ) -> bool {
        if self.status != LinuxUpdateStatus::Checking(attempt) {
            return false;
        }
        self.status = match result {
            Ok(LinuxUpdateCheck::UpToDate) => LinuxUpdateStatus::UpToDate,
            Ok(LinuxUpdateCheck::Available(release)) => LinuxUpdateStatus::Available(release),
            Err(error) => LinuxUpdateStatus::Errored(error),
        };
        true
    }

    /// How `installation` can install the available release; `None` when no
    /// release is available. The one gate for the Settings action and
    /// [`Self::begin_install`].
    #[must_use]
    pub fn offer(&self, installation: Installation) -> Option<InstallOffer> {
        let LinuxUpdateStatus::Available(release) = &self.status else {
            return None;
        };
        let arch = openlogi_core::linux_package::arch_tag(std::env::consts::ARCH);
        Some(install_offer(release, installation, self.public_key, arch))
    }

    /// Start installing the available release, when [`Self::offer`] is
    /// [`InstallOffer::Ready`]; `None` otherwise, changing nothing.
    pub fn begin_install(&mut self, installation: Installation) -> Option<(Attempt, InstallPlan)> {
        let Some(InstallOffer::Ready(plan)) = self.offer(installation) else {
            return None;
        };
        let attempt = self.next_attempt();
        self.status = LinuxUpdateStatus::Installing {
            attempt,
            version: plan.version.clone(),
        };
        Some((attempt, *plan))
    }

    /// Apply the result of the install `attempt` started. Returns whether it
    /// applied: unless `attempt` is the install in flight, the result is
    /// stale and changes nothing.
    pub fn finish_install(
        &mut self,
        attempt: Attempt,
        result: Result<(), LinuxUpdateError>,
    ) -> bool {
        let LinuxUpdateStatus::Installing {
            attempt: current,
            version,
        } = &self.status
        else {
            return false;
        };
        if *current != attempt {
            return false;
        }
        self.status = match result {
            Ok(()) => LinuxUpdateStatus::Staged {
                version: version.clone(),
            },
            Err(error) => LinuxUpdateStatus::Errored(error),
        };
        true
    }
}

/// [`LinuxUpdate::offer`] with the host's release arch tag passed in, so
/// tests cover every arch without depending on the machine running them.
fn install_offer(
    release: &LatestRelease,
    installation: Installation,
    public_key: Option<&'static str>,
    arch: Option<&str>,
) -> InstallOffer {
    let package = match installation {
        Installation::Detecting => return InstallOffer::Detecting,
        Installation::Detected(InstallationSource::LinuxPackage(package)) => package,
        Installation::Detected(_) => return InstallOffer::PackageManager,
    };
    // A host arch with no release packages cannot have installed one; and
    // a build without the key cannot verify the download.
    let (Some(public_key), Some(arch)) = (public_key, arch) else {
        return InstallOffer::PackageManager;
    };
    match package_install::match_assets(&release.assets, &release.version, package, arch) {
        Ok(assets) => InstallOffer::Ready(Box::new(InstallPlan {
            version: release.version.clone(),
            package,
            assets,
            public_key,
        })),
        Err(_) => InstallOffer::NotPublished,
    }
}

/// Install the Linux update path as this app's update path. Only on Linux —
/// a runtime check rather than a `cfg`, so the same code compiles and is
/// tested on every host. Call once, from [`super::install`].
pub fn install_path(cx: &mut App) {
    if cfg!(target_os = "linux") {
        cx.set_global(LinuxUpdate::new(super::minisign_public_key()));
    }
}

/// Whether the Linux update path owns updates in this app.
pub fn is_active(cx: &App) -> bool {
    cx.has_global::<LinuxUpdate>()
}

/// Check GitHub Releases for the channel this launch follows, in the
/// background. A no-op without the Linux path, and whenever
/// [`LinuxUpdate::begin_check`] declines.
pub fn check(cx: &mut App) {
    if !is_active(cx) {
        return;
    }
    let Some(attempt) = cx.global_mut::<LinuxUpdate>().begin_check() else {
        return;
    };
    let channel = super::channel_in_use(cx);
    cx.spawn(async move |cx| {
        let current = super::running_version();
        let result = cx
            .background_executor()
            .spawn(async move { release_check::check_latest_release(&HTTP, channel, &current) })
            .await
            .map_err(|detail| {
                tracing::warn!(%detail, "Linux update check failed");
                LinuxUpdateError::Check
            });
        cx.update(|cx| {
            if !cx.global_mut::<LinuxUpdate>().finish_check(attempt, result) {
                tracing::debug!(?attempt, "dropped a stale Linux update check result");
            }
        });
    })
    .detach();
}

/// Download, verify and install the available release, when
/// [`LinuxUpdate::offer`] is [`InstallOffer::Ready`] for this installation.
/// A no-op otherwise.
pub fn install(cx: &mut App) {
    if !is_active(cx) {
        return;
    }
    let installation = cx
        .try_global::<Installation>()
        .copied()
        .unwrap_or(Installation::Detecting);
    let Some((attempt, plan)) = cx.global_mut::<LinuxUpdate>().begin_install(installation) else {
        return;
    };
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { download_verify_and_install(&plan) })
            .await
            .map_err(|failure| {
                tracing::warn!(detail = %failure.detail, "Linux package update failed");
                failure.error
            });
        cx.update(|cx| {
            if !cx
                .global_mut::<LinuxUpdate>()
                .finish_install(attempt, result)
            {
                tracing::debug!(?attempt, "dropped a stale Linux update install result");
            }
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

/// A failed install step: what Settings → Updates shows, and the detail the
/// log keeps.
#[derive(Debug)]
struct Failure {
    error: LinuxUpdateError,
    detail: String,
}

impl Failure {
    fn new(error: LinuxUpdateError, detail: impl Into<String>) -> Self {
        Self {
            error,
            detail: detail.into(),
        }
    }
}

/// What a failed `pkexec` exit code means. pkexec(1) exits 126 when the
/// user dismissed the authentication dialog and 127 when authorization
/// failed or could not be obtained; any other code is the package manager's
/// own.
fn exit_failure(code: Option<i32>) -> LinuxUpdateError {
    match code {
        Some(126 | 127) => LinuxUpdateError::Authorization,
        _ => LinuxUpdateError::Install,
    }
}

/// A fresh, randomly-named temporary directory, owner-only on Unix (`0o700`)
/// so no other local account can read or write into it while the downloaded
/// package sits there awaiting verification and install.
#[cfg(unix)]
fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt as _;

    tempfile::Builder::new()
        .prefix("openlogi-update-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
}

#[cfg(not(unix))]
fn private_tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("openlogi-update-")
        .tempdir()
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

fn download_bytes(url: &str, limit: u64, timeout: Duration) -> Result<Vec<u8>, Failure> {
    HTTP.get(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call()
        .map_err(|error| {
            Failure::new(
                LinuxUpdateError::Download,
                format!("download of {url} failed: {error}"),
            )
        })?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|error| {
            Failure::new(
                LinuxUpdateError::Download,
                format!("failed to read downloaded bytes from {url}: {error}"),
            )
        })
}

/// A small text download: a `.minisig` or `SHA256SUMS`. Bytes that are not
/// UTF-8 are not a file the release pipeline wrote.
fn download_text(url: &str) -> Result<String, Failure> {
    String::from_utf8(download_bytes(url, METADATA_LIMIT, METADATA_TIMEOUT)?).map_err(|error| {
        Failure::new(
            LinuxUpdateError::Verification,
            format!("{url} is not valid UTF-8: {error}"),
        )
    })
}

/// The verified install path `install.sh`'s `install_release` runs: download
/// the planned package, `.minisig` and `SHA256SUMS`, verify both, then hand
/// the verified file to the package manager through `pkexec`. Blocking —
/// callers run it off the UI thread. Not unit-tested: it is real network I/O
/// plus a real privileged package-manager invocation, which must never run
/// outside a genuine, user-initiated install (see [`package_install`] for the
/// unit-tested pure logic this calls into).
fn download_verify_and_install(plan: &InstallPlan) -> Result<(), Failure> {
    use LinuxUpdateError::{Install, MissingTool, Verification};

    let package = &plan.assets.package;
    package_install::ensure_newer(&plan.version, &super::running_version())
        .map_err(|detail| Failure::new(Verification, detail))?;

    // A private, randomly-named, owner-only directory — not the shared,
    // world-writable `std::env::temp_dir()` with a predictable filename
    // (the asset's own name, known in advance from the release). A
    // predictable path there would let another local process race to swap
    // the file between this verification and the `pkexec` install below.
    // Dropped (and removed) when this function returns either way.
    let download_dir = private_tempdir().map_err(|error| {
        Failure::new(
            Install,
            format!("failed to create a private download directory: {error}"),
        )
    })?;
    let package_path = download_dir.path().join(&package.name);
    let mut command =
        package_install::install_command(plan.package, &package_path).map_err(|program| {
            Failure::new(MissingTool(program), format!("{program} was not found"))
        })?;

    let package_bytes = download_bytes(
        &package.browser_download_url,
        PACKAGE_LIMIT,
        PACKAGE_TIMEOUT,
    )?;
    let signature = download_text(&plan.assets.signature.browser_download_url)?;
    let checksums = download_text(&plan.assets.checksums.browser_download_url)?;

    let expected_hex =
        package_install::find_checksum(&checksums, &package.name).ok_or_else(|| {
            Failure::new(
                Verification,
                format!("SHA256SUMS has no entry for {}", package.name),
            )
        })?;
    if !package_install::verify_checksum(&package_bytes, expected_hex) {
        return Err(Failure::new(
            Verification,
            format!("checksum verification failed for {}", package.name),
        ));
    }
    package_install::verify_minisign(plan.public_key, &package_bytes, &signature, &package.name)
        .map_err(|detail| Failure::new(Verification, detail))?;

    std::fs::write(&package_path, &package_bytes).map_err(|error| {
        Failure::new(
            Install,
            format!("failed to write {}: {error}", package_path.display()),
        )
    })?;
    let status = command
        .status()
        .map_err(|error| Failure::new(Install, format!("failed to run pkexec: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Failure::new(
            exit_failure(status.code()),
            format!("the package install exited with {status}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::release_check::ReleaseAsset;
    use super::*;

    const KEY: Option<&str> = Some("RWQfixture");

    fn rpm() -> Installation {
        Installation::Detected(InstallationSource::LinuxPackage(LinuxPackage::Rpm))
    }

    fn host_arch() -> &'static str {
        openlogi_core::linux_package::arch_tag(std::env::consts::ARCH).unwrap_or("amd64")
    }

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.test/{name}"),
        }
    }

    /// A release of `version` publishing `names` (with `{arch}` standing for
    /// `arch`).
    fn release(version: &str, arch: &str, names: &[&str]) -> LatestRelease {
        LatestRelease {
            version: Version::parse(version).unwrap(),
            assets: names
                .iter()
                .map(|name| asset(&name.replace("{arch}", arch)))
                .collect(),
        }
    }

    /// Every asset an RPM install needs, for `arch`.
    fn full_rpm_release(arch: &str) -> LatestRelease {
        release(
            "99.0.0",
            arch,
            &[
                "openlogi-v99.0.0-linux-{arch}.rpm",
                "openlogi-v99.0.0-linux-{arch}.rpm.minisig",
                "SHA256SUMS",
            ],
        )
    }

    /// A path whose check found `release`.
    fn available(key: Option<&'static str>, release: LatestRelease) -> LinuxUpdate {
        let mut update = LinuxUpdate::new(key);
        let attempt = update.begin_check().unwrap();
        assert!(update.finish_check(attempt, Ok(LinuxUpdateCheck::Available(release))));
        update
    }

    #[test]
    fn only_a_detected_linux_package_in_a_keyed_build_is_offered_an_install() {
        let release = full_rpm_release("amd64");
        let InstallOffer::Ready(plan) = install_offer(&release, rpm(), KEY, Some("amd64")) else {
            panic!("a keyed RPM install with every asset published installs in-app");
        };
        assert_eq!(plan.package, LinuxPackage::Rpm);
        assert_eq!(plan.version, Version::parse("99.0.0").unwrap());
        assert_eq!(plan.assets.package.name, "openlogi-v99.0.0-linux-amd64.rpm");
        assert_eq!(plan.public_key, "RWQfixture");

        assert_eq!(
            install_offer(&release, rpm(), None, Some("amd64")),
            InstallOffer::PackageManager,
            "a build without the update key cannot verify a download"
        );
        assert_eq!(
            install_offer(&release, rpm(), KEY, None),
            InstallOffer::PackageManager,
            "no release package exists for this arch"
        );
        assert_eq!(
            install_offer(&release, Installation::Detecting, KEY, Some("amd64")),
            InstallOffer::Detecting
        );
        for source in [InstallationSource::Nix, InstallationSource::Unknown] {
            assert_eq!(
                install_offer(&release, Installation::Detected(source), KEY, Some("amd64")),
                InstallOffer::PackageManager,
                "{source:?}"
            );
        }
    }

    #[test]
    fn partial_release_is_not_installable() {
        let partial = [
            // The Linux build failed: only SHA256SUMS (and other platforms).
            &["SHA256SUMS", "OpenLogi-v99.0.0-macos-arm64.dmg"][..],
            // The package without its signature.
            &["openlogi-v99.0.0-linux-{arch}.rpm", "SHA256SUMS"][..],
            // The package and signature without SHA256SUMS.
            &[
                "openlogi-v99.0.0-linux-{arch}.rpm",
                "openlogi-v99.0.0-linux-{arch}.rpm.minisig",
            ][..],
            // Only another family's package.
            &[
                "openlogi-v99.0.0-linux-{arch}.deb",
                "openlogi-v99.0.0-linux-{arch}.deb.minisig",
                "SHA256SUMS",
            ][..],
        ];
        for names in partial {
            let release = release("99.0.0", host_arch(), names);
            assert_eq!(
                install_offer(&release, rpm(), KEY, Some(host_arch())),
                InstallOffer::NotPublished,
                "{names:?}"
            );
            let mut update = available(KEY, release);
            let before = update.clone();
            assert_eq!(update.begin_install(rpm()), None, "{names:?}");
            assert_eq!(update, before, "a declined install changes nothing");
        }
        // Another arch's complete set is not this host's package.
        let other = if host_arch() == "amd64" {
            "arm64"
        } else {
            "amd64"
        };
        assert_eq!(
            install_offer(&full_rpm_release(other), rpm(), KEY, Some(host_arch())),
            InstallOffer::NotPublished
        );
    }

    #[test]
    fn stale_update_completion_is_ignored() {
        let mut update = LinuxUpdate::new(KEY);
        let first = update.begin_check().unwrap();
        assert_eq!(update.begin_check(), None, "one check at a time");
        assert!(update.finish_check(first, Err(LinuxUpdateError::Check)));
        assert_eq!(
            update.status(),
            &LinuxUpdateStatus::Errored(LinuxUpdateError::Check)
        );

        // A retry is a new attempt; the first one's result can no longer land.
        let retry = update.begin_check().unwrap();
        assert_ne!(retry, first);
        assert!(!update.finish_check(first, Ok(LinuxUpdateCheck::UpToDate)));
        assert_eq!(update.status(), &LinuxUpdateStatus::Checking(retry));
        assert!(update.finish_check(
            retry,
            Ok(LinuxUpdateCheck::Available(full_rpm_release(host_arch())))
        ));
        assert!(!update.finish_check(retry, Ok(LinuxUpdateCheck::UpToDate)));

        let (install, plan) = update.begin_install(rpm()).unwrap();
        assert_eq!(plan.version, Version::parse("99.0.0").unwrap());
        let installing = update.status().clone();
        assert_eq!(update.begin_check(), None, "no check while installing");
        assert_eq!(update.begin_install(rpm()), None, "one install at a time");
        // Neither a check's identity nor an earlier attempt's applies.
        assert!(!update.finish_check(install, Ok(LinuxUpdateCheck::UpToDate)));
        assert!(!update.finish_install(retry, Err(LinuxUpdateError::Install)));
        assert_eq!(update.status(), &installing);

        assert!(update.finish_install(install, Ok(())));
        assert_eq!(
            update.status(),
            &LinuxUpdateStatus::Staged {
                version: Version::parse("99.0.0").unwrap()
            }
        );
        assert!(!update.finish_install(install, Err(LinuxUpdateError::Install)));
        assert_eq!(update.begin_check(), None, "a staged install is final");
    }

    #[test]
    fn a_failed_install_reports_its_error_and_allows_a_new_check() {
        let mut update = available(KEY, full_rpm_release(host_arch()));
        let (attempt, _) = update.begin_install(rpm()).unwrap();
        assert!(update.finish_install(attempt, Err(LinuxUpdateError::Authorization)));
        assert_eq!(
            update.status(),
            &LinuxUpdateStatus::Errored(LinuxUpdateError::Authorization)
        );
        assert_eq!(
            update.offer(rpm()),
            None,
            "nothing is offered after a failure"
        );
        assert!(update.begin_check().is_some());
    }

    #[test]
    fn pkexec_exit_codes_distinguish_authorization_from_install_failures() {
        assert_eq!(exit_failure(Some(126)), LinuxUpdateError::Authorization);
        assert_eq!(exit_failure(Some(127)), LinuxUpdateError::Authorization);
        assert_eq!(exit_failure(Some(1)), LinuxUpdateError::Install);
        assert_eq!(exit_failure(Some(100)), LinuxUpdateError::Install);
        assert_eq!(
            exit_failure(None),
            LinuxUpdateError::Install,
            "killed by a signal"
        );
    }
}
