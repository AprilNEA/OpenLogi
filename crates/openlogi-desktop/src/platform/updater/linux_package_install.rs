//! Verified download + package-manager install for a Linux package update
//! found by [`super::linux_release_check`].
//!
//! Mirrors `packaging/linux/install.sh`'s `install_release` /
//! `install_release_package`: download the package plus its `.minisig` and
//! the release's `SHA256SUMS`, verify both, then hand the verified file to
//! the OS package manager through `pkexec` (the script uses `sudo`; the GUI
//! has no TTY for a password prompt, so `pkexec` is the GUI equivalent). This
//! never runs on its own — only from an explicit "Install Update" click.

use std::fmt::Write as _;
use std::path::Path;

use gpui_updater::Version;
use sha2::{Digest as _, Sha256};

use super::linux_release_check::ReleaseAsset;
use crate::platform::installation::LinuxPackage;

/// Public trust anchor for release package signatures. Must stay in sync with
/// `MINISIGN_PUBLIC_KEY` in `packaging/linux/install.sh` and
/// `OPENLOGI_UPDATE_MINISIGN_PUBLIC_KEY` in the release publishing configuration.
pub const MINISIGN_PUBLIC_KEY: &str = "RWRRkFtw+rqkvTlCTGKUszSE5dX9CK1teaQD45jO4P9rYlWLO4/nHVUF";

/// The three release assets a verified Linux install needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedAssets {
    pub package: ReleaseAsset,
    pub signature: ReleaseAsset,
    pub checksums: ReleaseAsset,
}

/// `std::env::consts::ARCH`, translated to the arch tag
/// `packaging/linux/install.sh`'s `detect_architecture` uses in package names.
fn package_arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

fn package_extension(source: LinuxPackage) -> &'static str {
    match source {
        LinuxPackage::Deb => "deb",
        LinuxPackage::Rpm => "rpm",
        LinuxPackage::Arch => "pkg.tar.zst",
    }
}

/// The exact asset filename `xtask release` publishes for `version`, matching
/// `install.sh`'s `openlogi-${RELEASE_TAG}-linux-${PACKAGE_ARCH}.${PACKAGE_EXTENSION}`.
fn expected_package_name(version: &Version, source: LinuxPackage, arch: &str) -> String {
    format!(
        "openlogi-v{version}-linux-{arch}.{}",
        package_extension(source)
    )
}

/// Pick the package, `.minisig` and `SHA256SUMS` assets matching this host out
/// of a release's asset list.
pub fn match_assets(
    assets: &[ReleaseAsset],
    version: &Version,
    source: LinuxPackage,
) -> Result<MatchedAssets, String> {
    let arch = package_arch().ok_or("unsupported architecture for a Linux package install")?;
    match_assets_for_arch(assets, version, source, arch)
}

/// [`match_assets`] with the arch tag passed in rather than read from the
/// host, so tests can exercise every arch/package-family combination without
/// depending on the arch of the machine running them.
fn match_assets_for_arch(
    assets: &[ReleaseAsset],
    version: &Version,
    source: LinuxPackage,
    arch: &str,
) -> Result<MatchedAssets, String> {
    let package_name = expected_package_name(version, source, arch);
    let find = |name: &str| {
        assets
            .iter()
            .find(|asset| asset.name == name)
            .cloned()
            .ok_or_else(|| format!("no release asset named {name}"))
    };
    Ok(MatchedAssets {
        package: find(&package_name)?,
        signature: find(&format!("{package_name}.minisig"))?,
        checksums: find("SHA256SUMS")?,
    })
}

/// Whether `bytes` hashes to `expected_hex` (case-insensitive), the same
/// SHA-256 check `install.sh` runs via `sha256sum -c`.
pub fn verify_checksum(bytes: &[u8], expected_hex: &str) -> bool {
    let digest = Sha256::digest(bytes);
    let actual = digest.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    });
    actual.eq_ignore_ascii_case(expected_hex.trim())
}

/// Find the hex digest for `package_name` in a `SHA256SUMS`-formatted file
/// (`<hex>  <filename>` per line, as `sha256sum` writes and reads it).
pub fn find_checksum<'a>(sha256sums: &'a str, package_name: &str) -> Option<&'a str> {
    sha256sums.lines().find_map(|line| {
        let (hex, name) = line.split_once(char::is_whitespace)?;
        (name.trim() == package_name).then_some(hex)
    })
}

/// Runs an external command and reports whether it exited successfully,
/// injectable so verification logic is testable without spawning real
/// processes (`minisign` is not guaranteed to be installed, and the real
/// package-manager commands must never run outside a real install).
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<bool, String>;
}

/// The real runner, used outside tests.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<bool, String> {
        std::process::Command::new(program)
            .args(args)
            .status()
            .map(|status| status.success())
            .map_err(|error| format!("failed to run {program}: {error}"))
    }
}

/// Verify `package_path`'s minisign signature against the embedded public key,
/// exactly as `install.sh` runs `minisign -V -P "$MINISIGN_PUBLIC_KEY" -m
/// "$PACKAGE_PATH" -x "$SIGNATURE_PATH"`.
pub fn verify_minisign(
    runner: &dyn CommandRunner,
    package_path: &Path,
    signature_path: &Path,
) -> Result<(), String> {
    let args = vec![
        "-V".to_string(),
        "-P".to_string(),
        MINISIGN_PUBLIC_KEY.to_string(),
        "-m".to_string(),
        package_path.display().to_string(),
        "-x".to_string(),
        signature_path.display().to_string(),
    ];
    if runner.run("minisign", &args)? {
        Ok(())
    } else {
        Err("minisign signature verification failed".to_string())
    }
}

/// The literal per-package-manager install command `install.sh`'s
/// `install_release_package` runs, without the `pkexec`/`sudo` wrapper.
///
/// `LinuxPackage` only distinguishes the package family, not which frontend
/// manages it (`dnf` vs `zypper` vs plain `rpm`), so the RPM case uses
/// `rpm -Uvh` — the lowest-common-denominator command `install.sh` itself
/// falls back to, valid on every RPM-based host since `rpm` always underlies
/// `dnf`/`zypper`/`yum`.
fn install_command(source: LinuxPackage, package_path: &Path) -> Vec<String> {
    let path = package_path.display().to_string();
    match source {
        LinuxPackage::Deb => vec!["apt-get".into(), "install".into(), "-y".into(), path],
        LinuxPackage::Rpm => vec!["rpm".into(), "-Uvh".into(), path],
        LinuxPackage::Arch => vec!["pacman".into(), "-U".into(), "--noconfirm".into(), path],
    }
}

/// [`install_command`], wrapped in `pkexec` for the privilege escalation a GUI
/// has no TTY to do with `sudo`.
pub fn pkexec_install_command(source: LinuxPackage, package_path: &Path) -> Vec<String> {
    let mut argv = vec!["pkexec".to_string()];
    argv.extend(install_command(source, package_path));
    argv
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn assets() -> Vec<ReleaseAsset> {
        vec![
            ReleaseAsset {
                name: "openlogi-v0.8.15-linux-amd64.deb".into(),
                browser_download_url: "https://example.test/openlogi-v0.8.15-linux-amd64.deb"
                    .into(),
            },
            ReleaseAsset {
                name: "openlogi-v0.8.15-linux-amd64.deb.minisig".into(),
                browser_download_url:
                    "https://example.test/openlogi-v0.8.15-linux-amd64.deb.minisig".into(),
            },
            ReleaseAsset {
                name: "openlogi-v0.8.15-linux-arm64.rpm".into(),
                browser_download_url: "https://example.test/openlogi-v0.8.15-linux-arm64.rpm"
                    .into(),
            },
            ReleaseAsset {
                name: "openlogi-v0.8.15-linux-arm64.rpm.minisig".into(),
                browser_download_url:
                    "https://example.test/openlogi-v0.8.15-linux-arm64.rpm.minisig".into(),
            },
            ReleaseAsset {
                name: "SHA256SUMS".into(),
                browser_download_url: "https://example.test/SHA256SUMS".into(),
            },
        ]
    }

    #[test]
    fn matches_the_asset_for_an_explicit_arch_and_package_family() {
        let version = Version::parse("0.8.15").unwrap();
        let matched =
            match_assets_for_arch(&assets(), &version, LinuxPackage::Deb, "amd64").unwrap();
        assert_eq!(matched.package.name, "openlogi-v0.8.15-linux-amd64.deb");
        assert_eq!(
            matched.signature.name,
            "openlogi-v0.8.15-linux-amd64.deb.minisig"
        );
        assert_eq!(matched.checksums.name, "SHA256SUMS");

        let matched =
            match_assets_for_arch(&assets(), &version, LinuxPackage::Rpm, "arm64").unwrap();
        assert_eq!(matched.package.name, "openlogi-v0.8.15-linux-arm64.rpm");
    }

    #[test]
    fn missing_asset_is_an_error_not_a_panic() {
        let version = Version::parse("0.8.15").unwrap();
        match_assets_for_arch(&assets(), &version, LinuxPackage::Arch, "amd64").unwrap_err();
        match_assets_for_arch(&assets(), &version, LinuxPackage::Deb, "arm64").unwrap_err();
    }

    #[test]
    fn checksum_matches_case_insensitively_and_rejects_tampering() {
        let bytes = b"fixture package bytes";
        let digest = Sha256::digest(bytes);
        let hex = digest.iter().fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
        assert!(verify_checksum(bytes, &hex));
        assert!(verify_checksum(bytes, &hex.to_uppercase()));
        assert!(!verify_checksum(b"tampered bytes", &hex));
    }

    #[test]
    fn finds_the_matching_line_in_a_sha256sums_file() {
        let sums = "aaaa  openlogi-v0.8.15-linux-amd64.deb\nbbbb  SHA256SUMS\n";
        assert_eq!(
            find_checksum(sums, "openlogi-v0.8.15-linux-amd64.deb"),
            Some("aaaa")
        );
        assert_eq!(find_checksum(sums, "nonexistent.deb"), None);
    }

    struct FakeRunner {
        succeeds: bool,
        seen: RefCell<Option<(String, Vec<String>)>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<bool, String> {
            *self.seen.borrow_mut() = Some((program.to_string(), args.to_vec()));
            Ok(self.succeeds)
        }
    }

    #[test]
    fn minisign_verification_reports_the_fake_runners_outcome() {
        let package_path = Path::new("/tmp/openlogi.deb");
        let signature_path = Path::new("/tmp/openlogi.deb.minisig");

        let ok_runner = FakeRunner {
            succeeds: true,
            seen: RefCell::new(None),
        };
        verify_minisign(&ok_runner, package_path, signature_path).unwrap();
        let (program, args) = ok_runner.seen.into_inner().unwrap();
        assert_eq!(program, "minisign");
        assert_eq!(
            args,
            vec![
                "-V",
                "-P",
                MINISIGN_PUBLIC_KEY,
                "-m",
                "/tmp/openlogi.deb",
                "-x",
                "/tmp/openlogi.deb.minisig",
            ]
        );

        let failing_runner = FakeRunner {
            succeeds: false,
            seen: RefCell::new(None),
        };
        assert!(verify_minisign(&failing_runner, package_path, signature_path).is_err());
    }

    #[test]
    fn pkexec_install_command_matches_install_sh_per_package_manager() {
        let path = Path::new("/tmp/openlogi.rpm");
        assert_eq!(
            pkexec_install_command(LinuxPackage::Rpm, path),
            vec!["pkexec", "rpm", "-Uvh", "/tmp/openlogi.rpm"]
        );
        let path = Path::new("/tmp/openlogi.deb");
        assert_eq!(
            pkexec_install_command(LinuxPackage::Deb, path),
            vec!["pkexec", "apt-get", "install", "-y", "/tmp/openlogi.deb"]
        );
        let path = Path::new("/tmp/openlogi.pkg.tar.zst");
        assert_eq!(
            pkexec_install_command(LinuxPackage::Arch, path),
            vec![
                "pkexec",
                "pacman",
                "-U",
                "--noconfirm",
                "/tmp/openlogi.pkg.tar.zst",
            ]
        );
    }
}
