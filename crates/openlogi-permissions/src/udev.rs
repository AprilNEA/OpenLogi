//! Installing the OpenLogi udev rules — the Linux remedy for a `Denied`
//! [`input_device_access`](crate::input_device_access).
//!
//! The remedy has two halves in one executable. The desktop app calls
//! [`grant_input_device_access`], which asks polkit (`pkexec`) to run the
//! app's own executable again, as root, with one fixed argument. That process
//! sees [`is_install_request`] first thing in `main`, runs [`install_rules`],
//! and exits.
//!
//! ## Security boundary
//!
//! - pkexec runs a fixed program with a fixed argument vector: this process's
//!   own executable, as the kernel resolves it (`/proc/self/exe`), followed by
//!   `--install-udev-rules`. No user input, environment value, or configuration
//!   reaches that command line, and polkit shows it verbatim in its dialog.
//! - The root half takes no input. The rules are compiled into the binary,
//!   the destination is a constant, and `udevadm` gets constant arguments.
//!   pkexec has already replaced the environment with its minimal one, so
//!   `udevadm` resolves through pkexec's fixed `PATH`.
//! - The rules file is staged next to its destination and renamed over it. A
//!   symlink planted at the destination is replaced, never followed.
//!
//! ## Where the rules are not ours to write
//!
//! A Flatpak sandbox has neither the host's `/etc` nor pkexec, and on NixOS
//! `/etc/udev` is generated from the system configuration. [`rules_manager`]
//! names those hosts. The desktop shows their own instructions instead of the
//! button, and [`install_rules`] refuses to run on them.
//!
//! No group membership is involved: the rules tag nodes `uaccess`, which
//! systemd-logind (or elogind) turns into an ACL for the user of the active
//! seat. Systems without a seat manager need the `input`-group variant from
//! the guide, which is a decision about who may read every input device and so
//! stays manual.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The rules every package installs, byte for byte.
const RULES: &str = include_str!("../../../packaging/linux/udev/70-openlogi.rules");

/// Where the packages and `install.sh` put [`RULES`].
const RULES_DIR: &str = "/etc/udev/rules.d";
const RULES_FILE: &str = "70-openlogi.rules";

/// Not a `.rules` name, so udev never reads a partly written file.
const STAGING_FILE: &str = ".70-openlogi.rules.tmp";

/// The only argument of the root half's command line.
const INSTALL_ARG: &str = "--install-udev-rules";

/// What the root half runs after writing [`RULES`]: reload, then re-run the
/// rules for every node they cover so a device that is already connected gets
/// its `uaccess` ACL now rather than at its next reconnect.
///
/// `misc` devices have no `name` attribute, so uinput is matched by sysname.
const UDEVADM_STEPS: &[&[&str]] = &[
    &["control", "--reload-rules"],
    &[
        "trigger",
        "--subsystem-match=misc",
        "--sysname-match=uinput",
    ],
    &["trigger", "--subsystem-match=hidraw"],
    &["trigger", "--subsystem-match=input"],
];

/// pkexec's exit status when the user dismissed the authentication dialog.
const PKEXEC_DISMISSED: i32 = 126;

/// How pkexec and `main` lead the one line that says what went wrong.
const ERROR_PREFIXES: [&str; 2] = ["Error executing command as another user: ", "Error: "];

/// A host whose udev rules a package manager or sandbox owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RulesManager {
    /// This process runs inside a Flatpak sandbox.
    Flatpak,
    /// The host is NixOS, which generates `/etc/udev` from its configuration.
    NixOs,
}

impl fmt::Display for RulesManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Flatpak => "Flatpak",
            Self::NixOs => "NixOS",
        })
    }
}

/// Who owns this host's udev rules when it is not something
/// [`install_rules`] may write to.
#[must_use]
pub fn rules_manager() -> Option<RulesManager> {
    if Path::new("/.flatpak-info").exists() {
        Some(RulesManager::Flatpak)
    } else if Path::new("/etc/NIXOS").exists() {
        Some(RulesManager::NixOs)
    } else {
        None
    }
}

/// The terminal command that installs the rules bundled in this Flatpak on
/// the host, or `None` outside a Flatpak.
#[must_use]
pub fn flatpak_host_command() -> Option<String> {
    let app_id = std::env::var("FLATPAK_ID").ok()?;
    Some(format!(
        "flatpak run --command=cat {app_id} /app/share/openlogi/udev/{RULES_FILE} \
         | sudo tee {RULES_DIR}/{RULES_FILE} >/dev/null \
         && sudo udevadm control --reload-rules && sudo udevadm trigger"
    ))
}

/// Why [`grant_input_device_access`] did not install the rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantError {
    /// The user dismissed the polkit authentication dialog.
    Cancelled,
    /// `pkexec` is not installed.
    PkexecMissing,
    /// pkexec refused, or the root half failed; the text says which and why.
    Failed(String),
}

/// Ask polkit to install the udev rules as root, and wait for the outcome.
///
/// Blocks until the user answers the authentication dialog and the root half
/// exits, so call it off the UI thread. Success means the rules are in place
/// and udev has re-applied them; re-probe [`input_device_access`] for the
/// verdict, since a seat manager still has to grant the ACL.
///
/// [`input_device_access`]: crate::input_device_access
pub fn grant_input_device_access() -> Result<(), GrantError> {
    let executable = std::env::current_exe().map_err(|error| {
        GrantError::Failed(format!("could not locate the OpenLogi executable: {error}"))
    })?;
    let output = Command::new("pkexec")
        // Without a graphical polkit agent, fail instead of prompting on
        // whatever terminal launched the app.
        .arg("--disable-internal-agent")
        .arg(&executable)
        .arg(INSTALL_ARG)
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(output) => grant_outcome(output.status.code(), &output.stderr),
        Err(error) if error.kind() == ErrorKind::NotFound => Err(GrantError::PkexecMissing),
        Err(error) => Err(GrantError::Failed(format!("could not run pkexec: {error}"))),
    }
}

/// Read the root half's result from pkexec's exit status and stderr.
///
/// pkexec exits 126 when the dialog was dismissed and 127 when it could not
/// authorize at all, with its reason on stderr. Any other status is the root
/// half's own: zero, or failure with the [`InstallError`] that `main` printed
/// as `Error: …`. The reason is that first stderr line without its prefix.
fn grant_outcome(code: Option<i32>, stderr: &[u8]) -> Result<(), GrantError> {
    match code {
        Some(0) => Ok(()),
        Some(PKEXEC_DISMISSED) => Err(GrantError::Cancelled),
        _ => {
            let stderr = String::from_utf8_lossy(stderr);
            let reason = stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| {
                    ERROR_PREFIXES
                        .iter()
                        .find_map(|prefix| line.strip_prefix(prefix))
                        .unwrap_or(line)
                        .to_owned()
                });
            Err(GrantError::Failed(reason.unwrap_or_else(|| match code {
                Some(code) => format!("pkexec exited with status {code}"),
                None => "pkexec was terminated by a signal".to_owned(),
            })))
        }
    }
}

/// Whether this process was started as the root half: exactly one argument,
/// `--install-udev-rules`. Anything else is an ordinary launch.
#[must_use]
pub fn is_install_request(args: impl IntoIterator<Item = OsString>) -> bool {
    let mut args = args.into_iter().skip(1);
    args.next().is_some_and(|arg| arg == INSTALL_ARG) && args.next().is_none()
}

/// Why [`install_rules`] stopped.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The host's rules belong to a package manager or sandbox.
    #[error("the udev rules on this system are managed by {0}")]
    Managed(RulesManager),
    /// The rules file could not be written.
    #[error("could not write {}: {error}", path.display())]
    Write {
        /// The file being written.
        path: PathBuf,
        /// What the filesystem reported.
        error: io::Error,
    },
    /// A `udevadm` step failed.
    #[error("`udevadm {command}` failed: {detail}")]
    Udevadm {
        /// The `udevadm` arguments of the failed step.
        command: String,
        /// Its stderr, or why it could not run.
        detail: String,
    },
}

/// The root half: install the bundled `70-openlogi.rules` into
/// `/etc/udev/rules.d`, reload udev, and re-run the rules for the nodes they
/// cover. Needs root.
///
/// Writes nothing when the installed file already matches, so a package's
/// copy is left untouched and the run only re-applies it.
pub fn install_rules() -> Result<(), InstallError> {
    if let Some(manager) = rules_manager() {
        return Err(InstallError::Managed(manager));
    }
    write_rules(Path::new(RULES_DIR))?;
    for args in UDEVADM_STEPS {
        udevadm(args)?;
    }
    // Wait for the triggered events so the caller's re-probe sees the ACLs.
    // A timeout is not a failure: the events still land, just later.
    let _ = udevadm(&["settle", "--timeout=10"]);
    Ok(())
}

/// Write [`RULES`] into `dir` unless an identical file is already there.
fn write_rules(dir: &Path) -> Result<(), InstallError> {
    let path = dir.join(RULES_FILE);
    if fs::read(&path).is_ok_and(|current| current == RULES.as_bytes()) {
        return Ok(());
    }
    let write_error = |error| InstallError::Write {
        path: path.clone(),
        error,
    };
    fs::create_dir_all(dir).map_err(write_error)?;
    let staging = dir.join(STAGING_FILE);
    // A run killed mid-write leaves its staging file behind.
    match fs::remove_file(&staging) {
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(write_error(error)),
        _ => {}
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&staging)
        .map_err(write_error)?;
    file.write_all(RULES.as_bytes())
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(0o644)))
        .and_then(|()| file.sync_all())
        .map_err(write_error)?;
    fs::rename(&staging, &path).map_err(write_error)
}

/// Run one `udevadm` step, reporting its stderr when it fails.
fn udevadm(args: &[&str]) -> Result<(), InstallError> {
    let failed = |detail: String| InstallError::Udevadm {
        command: args.join(" "),
        detail,
    };
    let output = Command::new("udevadm")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| failed(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(failed(match stderr.trim() {
            "" => output.status.to_string(),
            stderr => stderr.to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests;
