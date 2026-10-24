//! Verified download + package-manager install for a Linux package update
//! found by [`super::linux_release_check`].
//!
//! Mirrors `packaging/linux/install.sh`'s `install_release` /
//! `install_release_package`: download the package plus its `.minisig` and
//! the release's `SHA256SUMS`, verify both, then hand the verified file to
//! the OS package manager through `pkexec` (the script uses `sudo`; the GUI
//! has no TTY for a password prompt, so `pkexec` is the GUI equivalent). This
//! never runs on its own — only from an explicit "Install Update" click.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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

/// Refuse `version` unless it is strictly newer than `current`, so a validly
/// signed but older release can never be installed over this one.
pub fn ensure_newer(version: &Version, current: &Version) -> Result<(), String> {
    if version > current {
        Ok(())
    } else {
        Err(format!(
            "refusing to install {version}: not newer than the running {current}"
        ))
    }
}

/// Verify `package_bytes` against the `.minisig` text with the embedded public
/// key, as `install.sh`'s `minisign -V` does, and require the signed trusted
/// comment to name `package_name`. The release pipeline signs with minisign's
/// default trusted comment (`timestamp:…\tfile:<name>\thashed`), so this binds
/// the signature to that exact, versioned filename.
pub fn verify_minisign(
    package_bytes: &[u8],
    signature: &str,
    package_name: &str,
) -> Result<(), String> {
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|error| format!("invalid minisign signature: {error}"))?;
    let public_key = minisign_verify::PublicKey::from_base64(MINISIGN_PUBLIC_KEY)
        .map_err(|error| format!("invalid minisign public key: {error}"))?;
    public_key
        .verify(package_bytes, &signature, false)
        .map_err(|error| format!("minisign signature verification failed: {error}"))?;
    if !trusted_comment_names(signature.trusted_comment(), package_name) {
        return Err(format!("minisign signature is not for {package_name}"));
    }
    Ok(())
}

/// Whether a minisign trusted comment carries a `file:<package_name>` field.
fn trusted_comment_names(trusted_comment: &str, package_name: &str) -> bool {
    trusted_comment
        .split('\t')
        .any(|field| field.strip_prefix("file:") == Some(package_name))
}

/// The first executable named `program` in the `PATH`-style list `path`.
fn find_on_path(program: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Check, before anything is downloaded, that `pkexec` and the package
/// manager the install runs are on `path` (normally `$PATH`), so a missing
/// tool is an actionable message instead of a failure after the download.
pub fn require_install_tools(source: LinuxPackage, path: &OsStr) -> Result<(), String> {
    for program in ["pkexec", install_program(source)] {
        if find_on_path(program, path).is_none() {
            return Err(format!(
                "{program} was not found on PATH; install it to update from the app \
                 (pkexec is provided by polkit)"
            ));
        }
    }
    Ok(())
}

fn install_program(source: LinuxPackage) -> &'static str {
    match source {
        LinuxPackage::Deb => "apt-get",
        LinuxPackage::Rpm => "rpm",
        LinuxPackage::Arch => "pacman",
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
    let args: &[&str] = match source {
        LinuxPackage::Deb => &["install", "-y"],
        LinuxPackage::Rpm => &["-Uvh"],
        LinuxPackage::Arch => &["-U", "--noconfirm"],
    };
    std::iter::once(install_program(source))
        .chain(args.iter().copied())
        .map(String::from)
        .chain([package_path.display().to_string()])
        .collect()
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

    /// A real release signature, for `openlogi-v0.8.17-linux-amd64.deb`.
    const V0_8_17_DEB_MINISIG: &str = "untrusted comment: signature from minisign secret key
RURRkFtw+rqkvbEQzNdKwxbny5Z1fl9AFFrQx14DUIwV8T8oJcVn4xHQKn2lgbJalOoCwR3oZ2bnLN9lNQNgXRA2+MbO+8Dehwg=
trusted comment: timestamp:1792704533\tfile:openlogi-v0.8.17-linux-amd64.deb\thashed
fpTEZT3hEgq9Dt80RpQtQU7Kff+YNjzVBoMwk+p2H11ji5iYFgTNrNdjVH9RPoDeKf6JZXmNZhwvKmApHQQICw==
";

    #[test]
    fn minisign_rejects_bytes_the_release_signature_does_not_cover() {
        let name = "openlogi-v0.8.17-linux-amd64.deb";
        let error = verify_minisign(b"not the package", V0_8_17_DEB_MINISIG, name).unwrap_err();
        assert!(error.contains("verification failed"), "{error}");
        assert!(verify_minisign(b"x", "garbage", name).is_err());
    }

    #[test]
    fn trusted_comment_must_name_the_exact_package() {
        let comment = "timestamp:1792704533\tfile:openlogi-v0.8.17-linux-amd64.deb\thashed";
        assert!(trusted_comment_names(
            comment,
            "openlogi-v0.8.17-linux-amd64.deb"
        ));
        assert!(!trusted_comment_names(
            comment,
            "openlogi-v0.8.16-linux-amd64.deb"
        ));
        assert!(!trusted_comment_names(
            comment,
            "openlogi-v0.8.17-linux-amd64"
        ));
        assert!(!trusted_comment_names(
            "timestamp:1",
            "openlogi-v0.8.17-linux-amd64.deb"
        ));
    }

    #[test]
    fn only_a_strictly_newer_version_may_install() {
        let current = Version::parse("0.8.17").unwrap();
        ensure_newer(&Version::parse("0.8.18").unwrap(), &current).unwrap();
        ensure_newer(&current, &current).unwrap_err();
        ensure_newer(&Version::parse("0.8.16").unwrap(), &current).unwrap_err();
        ensure_newer(&Version::parse("0.8.17-beta.1").unwrap(), &current).unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn install_tools_are_looked_up_on_the_given_path_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().as_os_str();
        let add = |name: &str, mode: u32| {
            let file = dir.path().join(name);
            std::fs::write(&file, "").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        };

        let error = require_install_tools(LinuxPackage::Deb, path).unwrap_err();
        assert!(error.starts_with("pkexec was not found"), "{error}");

        add("pkexec", 0o755);
        add("apt-get", 0o644);
        let error = require_install_tools(LinuxPackage::Deb, path).unwrap_err();
        assert!(error.starts_with("apt-get was not found"), "{error}");

        add("apt-get", 0o755);
        require_install_tools(LinuxPackage::Deb, path).unwrap();
        require_install_tools(LinuxPackage::Rpm, path).unwrap_err();
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
