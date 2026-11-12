//! Verified download + package-manager install for a Linux package update
//! found by [`super::release_check`].
//!
//! Mirrors `packaging/linux/install.sh`'s `install_release` /
//! `install_release_package`: download the package plus its `.minisig` and
//! the release's `SHA256SUMS`, verify both, then hand the verified file to
//! the package manager the script would pick, through `pkexec` (the script
//! uses `sudo`; the GUI has no TTY for a password prompt, so `pkexec` is the
//! GUI equivalent). This never runs on its own — only from an explicit
//! "Download & Install" click.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use gpui_updater::Version;
use sha2::{Digest as _, Sha256};

use super::release_check::ReleaseAsset;
use crate::platform::installation::LinuxPackage;

/// Where polkit installs `pkexec`. A fixed path, never a `PATH` lookup: the
/// app starts this root prompt itself, so a `pkexec` (or package manager)
/// planted earlier on the user's `PATH` must not be what runs.
const PKEXEC: &str = "/usr/bin/pkexec";

/// The three release assets a verified Linux install needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedAssets {
    pub package: ReleaseAsset,
    pub signature: ReleaseAsset,
    pub checksums: ReleaseAsset,
}

fn package_extension(source: LinuxPackage) -> &'static str {
    match source {
        LinuxPackage::Deb => "deb",
        LinuxPackage::Rpm => "rpm",
        LinuxPackage::Arch => "pkg.tar.zst",
    }
}

/// Pick the package, `.minisig` and `SHA256SUMS` assets for package family
/// `source` on release arch tag `arch` out of a release's asset list. An
/// error when any of the three is missing: the release workflow still
/// publishes a release whose Linux build failed, so a newer version is no
/// proof that its package exists.
pub fn match_assets(
    assets: &[ReleaseAsset],
    version: &Version,
    source: LinuxPackage,
    arch: &str,
) -> Result<MatchedAssets, String> {
    let package_name =
        openlogi_core::linux_package::release_file_name(version, arch, package_extension(source));
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

/// Verify `package_bytes` against the `.minisig` text with `public_key`, as
/// `install.sh`'s `minisign -V` does, and require the signed trusted comment
/// to name `package_name`. The release pipeline signs with minisign's default
/// trusted comment (`timestamp:…\tfile:<name>\thashed`), so this binds the
/// signature to that exact, versioned filename. The caller passes the key
/// this build embeds ([`crate::platform::updater::minisign_public_key`]); a
/// build without one never offers an install (see
/// [`super::InstallOffer::PackageManager`]).
pub fn verify_minisign(
    public_key: &str,
    package_bytes: &[u8],
    signature: &str,
    package_name: &str,
) -> Result<(), String> {
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|error| format!("invalid minisign signature: {error}"))?;
    let public_key = minisign_verify::PublicKey::from_base64(public_key)
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

/// One package-manager frontend: its fixed path, never looked up on `PATH`
/// (see [`PKEXEC`]), and the arguments that install a local package file.
struct Frontend {
    program: &'static str,
    args: &'static [&'static str],
}

/// The frontends that install a package of family `source`, in the order
/// `install.sh`'s `select_package_manager` prefers them, with the arguments
/// its `install_release_package` passes. A dnf host updates through dnf, as
/// the script installed it, so dnf resolves any dependency a newer package
/// adds and records the transaction; plain `rpm -Uvh` is the last resort.
/// zypper needs `--allow-unsigned-rpm` because the packages carry a minisign
/// signature, not an RPM GPG one.
fn frontends(source: LinuxPackage) -> &'static [Frontend] {
    match source {
        LinuxPackage::Deb => &[Frontend {
            program: "/usr/bin/apt-get",
            args: &["install", "-y"],
        }],
        LinuxPackage::Rpm => &[
            Frontend {
                program: "/usr/bin/dnf",
                args: &["install", "-y"],
            },
            Frontend {
                program: "/usr/bin/yum",
                args: &["install", "-y"],
            },
            Frontend {
                program: "/usr/bin/zypper",
                args: &["--non-interactive", "install", "--allow-unsigned-rpm"],
            },
            Frontend {
                program: "/usr/bin/rpm",
                args: &["-Uvh"],
            },
        ],
        LinuxPackage::Arch => &[Frontend {
            program: "/usr/bin/pacman",
            args: &["-U", "--noconfirm"],
        }],
    }
}

/// The `pkexec`-wrapped command that installs `package_path`, or the fixed
/// path of the tool that is missing. Resolved before anything downloads, so
/// a missing tool is an actionable message rather than a failure after it.
pub fn install_command(source: LinuxPackage, package_path: &Path) -> Result<Command, &'static str> {
    install_command_with(source, package_path, is_executable)
}

/// [`install_command`] with the executable check passed in, so tests cover
/// every frontend choice without depending on the host's tools.
fn install_command_with(
    source: LinuxPackage,
    package_path: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Result<Command, &'static str> {
    if !exists(Path::new(PKEXEC)) {
        return Err(PKEXEC);
    }
    let candidates = frontends(source);
    let frontend = candidates
        .iter()
        .find(|frontend| exists(Path::new(frontend.program)))
        // The family's base tool (rpm, apt-get, pacman) is the one to name:
        // every frontend before it sits on top of it.
        .ok_or_else(|| candidates.last().map_or(PKEXEC, |frontend| frontend.program))?;
    let mut command = Command::new(PKEXEC);
    command
        .arg(frontend.program)
        .args(frontend.args)
        .arg(package_path);
    Ok(command)
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
        let matched = match_assets(&assets(), &version, LinuxPackage::Deb, "amd64").unwrap();
        assert_eq!(matched.package.name, "openlogi-v0.8.15-linux-amd64.deb");
        assert_eq!(
            matched.signature.name,
            "openlogi-v0.8.15-linux-amd64.deb.minisig"
        );
        assert_eq!(matched.checksums.name, "SHA256SUMS");

        let matched = match_assets(&assets(), &version, LinuxPackage::Rpm, "arm64").unwrap();
        assert_eq!(matched.package.name, "openlogi-v0.8.15-linux-arm64.rpm");
    }

    #[test]
    fn missing_asset_is_an_error_not_a_panic() {
        let version = Version::parse("0.8.15").unwrap();
        match_assets(&assets(), &version, LinuxPackage::Arch, "amd64").unwrap_err();
        match_assets(&assets(), &version, LinuxPackage::Deb, "arm64").unwrap_err();
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

    /// The public half of the key that made [`V0_8_17_DEB_MINISIG`]: a test
    /// fixture paired with that signature, not a trust anchor. The app only
    /// ever verifies with the key the build embeds.
    const V0_8_17_SIGNING_KEY: &str = "RWRRkFtw+rqkvTlCTGKUszSE5dX9CK1teaQD45jO4P9rYlWLO4/nHVUF";

    #[test]
    fn minisign_rejects_bytes_the_release_signature_does_not_cover() {
        let name = "openlogi-v0.8.17-linux-amd64.deb";
        let error = verify_minisign(
            V0_8_17_SIGNING_KEY,
            b"not the package",
            V0_8_17_DEB_MINISIG,
            name,
        )
        .unwrap_err();
        assert!(error.contains("verification failed"), "{error}");
        let error = verify_minisign(V0_8_17_SIGNING_KEY, b"x", "garbage", name).unwrap_err();
        assert!(error.contains("invalid minisign signature"), "{error}");
        let error = verify_minisign("not a key", b"x", V0_8_17_DEB_MINISIG, name).unwrap_err();
        assert!(error.contains("invalid minisign public key"), "{error}");
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

    const PK: &str = "/usr/bin/pkexec";

    fn present(installed: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |path| installed.iter().any(|tool| Path::new(tool) == path)
    }

    fn argv(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn install_needs_pkexec_and_a_frontend_at_their_fixed_paths() {
        let path = Path::new("/tmp/openlogi.pkg");
        for (source, installed, missing) in [
            (LinuxPackage::Deb, &[][..], PK),
            (LinuxPackage::Deb, &["/usr/bin/apt-get"][..], PK),
            (LinuxPackage::Deb, &[PK][..], "/usr/bin/apt-get"),
            // A copy elsewhere on `PATH` is not the fixed path.
            (
                LinuxPackage::Deb,
                &[PK, "/usr/local/bin/apt-get"][..],
                "/usr/bin/apt-get",
            ),
            (LinuxPackage::Rpm, &[PK][..], "/usr/bin/rpm"),
            (LinuxPackage::Arch, &[PK][..], "/usr/bin/pacman"),
        ] {
            assert_eq!(
                install_command_with(source, path, present(installed)).unwrap_err(),
                missing,
                "{source:?} with {installed:?}"
            );
        }
    }

    #[test]
    fn rpm_installs_through_the_frontend_install_sh_would_pick() {
        let path = Path::new("/tmp/openlogi.rpm");
        for (installed, expected) in [
            (
                &[
                    PK,
                    "/usr/bin/dnf",
                    "/usr/bin/yum",
                    "/usr/bin/zypper",
                    "/usr/bin/rpm",
                ][..],
                &[PK, "/usr/bin/dnf", "install", "-y", "/tmp/openlogi.rpm"][..],
            ),
            (
                &[PK, "/usr/bin/yum", "/usr/bin/zypper", "/usr/bin/rpm"][..],
                &[PK, "/usr/bin/yum", "install", "-y", "/tmp/openlogi.rpm"][..],
            ),
            (
                &[PK, "/usr/bin/zypper", "/usr/bin/rpm"][..],
                &[
                    PK,
                    "/usr/bin/zypper",
                    "--non-interactive",
                    "install",
                    "--allow-unsigned-rpm",
                    "/tmp/openlogi.rpm",
                ][..],
            ),
            (
                &[PK, "/usr/bin/rpm"][..],
                &[PK, "/usr/bin/rpm", "-Uvh", "/tmp/openlogi.rpm"][..],
            ),
        ] {
            let command = install_command_with(LinuxPackage::Rpm, path, present(installed));
            assert_eq!(argv(&command.unwrap()), expected, "{installed:?}");
        }
    }

    #[test]
    fn deb_and_arch_install_as_install_sh_does() {
        let path = Path::new("/tmp/openlogi.deb");
        let command =
            install_command_with(LinuxPackage::Deb, path, present(&[PK, "/usr/bin/apt-get"]));
        assert_eq!(
            argv(&command.unwrap()),
            [PK, "/usr/bin/apt-get", "install", "-y", "/tmp/openlogi.deb"]
        );
        let path = Path::new("/tmp/openlogi.pkg.tar.zst");
        let command =
            install_command_with(LinuxPackage::Arch, path, present(&[PK, "/usr/bin/pacman"]));
        assert_eq!(
            argv(&command.unwrap()),
            [
                PK,
                "/usr/bin/pacman",
                "-U",
                "--noconfirm",
                "/tmp/openlogi.pkg.tar.zst"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_an_executable_file_counts_as_a_tool() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tool");
        assert!(!is_executable(&tool));
        std::fs::write(&tool, "").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(&tool));
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&tool));
        assert!(!is_executable(dir.path()));
    }
}
