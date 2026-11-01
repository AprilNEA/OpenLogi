//! Running one OpenLogi CLI command with administrator rights, for the few
//! operations the OS reserves for them.
//!
//! Only reconnecting a camera needs this today: Linux lets only root write a
//! camera's usbfs node, and Windows lets only an administrator cycle a hub
//! port. The GUI never elevates itself. It asks the OS to run the bundled
//! `openlogi` CLI elevated for that one command, through the OS's own consent
//! prompt (polkit's `pkexec` on Linux, the UAC `runas` verb on Windows), and
//! waits for the command to finish. macOS needs no elevation for it.

use openlogi_camera::ControlError;

/// Reconnect the camera `unique_id` by running `openlogi camera --camera
/// <unique_id> reconnect` elevated.
///
/// # Errors
/// [`ControlError::NeedsElevation`] when the CLI is not installed next to the
/// GUI, the OS offers no elevation here (Flatpak, macOS), or the user declines
/// the prompt; [`ControlError::Io`] when the elevated command fails.
pub fn reconnect_camera(unique_id: &str) -> Result<(), ControlError> {
    imp::run_cli(&["camera", "--camera", unique_id, "reconnect"])
}

/// Where the CLI sits relative to the GUI executable's directory, in the order
/// to try. Every Linux package and the install script put both in one `bin`
/// directory; the MSI puts the CLI in `bin\` under the GUI's folder, and a
/// development build leaves both in `target\<profile>\`.
#[cfg(target_os = "linux")]
const LINUX_CLI: [&str; 1] = ["openlogi"];
#[cfg(target_os = "windows")]
const WINDOWS_CLI: [&str; 2] = [r"bin\openlogi.exe", "openlogi.exe"];

/// The first of `candidates` that exists under the GUI executable's directory.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn cli_path(candidates: &[&str]) -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    candidates
        .iter()
        .map(|relative| dir.join(relative))
        .find(|path| path.is_file())
}

#[cfg(target_os = "linux")]
mod imp {
    use std::process::{Command, Stdio};

    use openlogi_camera::ControlError;

    /// Run the CLI with `args` through `pkexec`, which shows polkit's
    /// password prompt and runs it as root.
    pub(super) fn run_cli(args: &[&str]) -> Result<(), ControlError> {
        // A Flatpak sandbox has no pkexec, and no host CLI it may run.
        if std::env::var_os("FLATPAK_ID").is_some() {
            return Err(ControlError::NeedsElevation);
        }
        let cli = super::cli_path(&super::LINUX_CLI).ok_or(ControlError::NeedsElevation)?;
        // The arguments go to pkexec as separate argv entries, with no shell.
        let status = Command::new("pkexec")
            .arg(&cli)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| ControlError::NeedsElevation)?;
        match status.code() {
            Some(0) => Ok(()),
            // pkexec: 126 when the prompt is dismissed, 127 when not
            // authorized.
            Some(126 | 127) => Err(ControlError::NeedsElevation),
            _ => Err(ControlError::Io(format!("elevated openlogi: {status}"))),
        }
    }
}

#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "ShellExecuteExW and the process handle it returns"
)]
mod imp {
    use std::os::windows::ffi::OsStrExt as _;

    use openlogi_camera::ControlError;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows::core::{PCWSTR, w};

    /// `SW_HIDE`: the CLI is a console program, and its window has nothing to
    /// show.
    const SW_HIDE: i32 = 0;

    /// Run the CLI with `args` through the `runas` verb, which shows the UAC
    /// prompt and starts it as an administrator.
    pub(super) fn run_cli(args: &[&str]) -> Result<(), ControlError> {
        let cli = super::cli_path(&super::WINDOWS_CLI).ok_or(ControlError::NeedsElevation)?;
        let file: Vec<u16> = cli.as_os_str().encode_wide().chain(Some(0)).collect();
        let line = args
            .iter()
            .map(|arg| super::quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let parameters: Vec<u16> = line.encode_utf16().chain(Some(0)).collect();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(size_of::<SHELLEXECUTEINFOW>())
                .map_err(|_| ControlError::NeedsElevation)?,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(parameters.as_ptr()),
            nShow: SW_HIDE,
            ..Default::default()
        };
        // SAFETY: `info` is a valid SHELLEXECUTEINFOW whose strings are
        // NUL-terminated and outlive the call. Declining the UAC prompt fails
        // the call with ERROR_CANCELLED.
        unsafe { ShellExecuteExW(&raw mut info) }.map_err(|_| ControlError::NeedsElevation)?;
        if info.hProcess.is_invalid() {
            return Err(ControlError::Io(
                "elevated openlogi: no process".to_string(),
            ));
        }
        let mut code = 1u32;
        // SAFETY: `hProcess` is the live process handle SEE_MASK_NOCLOSEPROCESS
        // asked for. It is waited on, read, and closed once.
        let read = unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            let read = GetExitCodeProcess(info.hProcess, &raw mut code);
            let _ = CloseHandle(info.hProcess);
            read
        };
        match read {
            Ok(()) if code == 0 => Ok(()),
            Ok(()) => Err(ControlError::Io(format!(
                "elevated openlogi exited with {code}"
            ))),
            Err(error) => Err(ControlError::Io(error.to_string())),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod imp {
    use openlogi_camera::ControlError;

    /// Nothing here needs, or offers, elevation.
    pub(super) fn run_cli(_args: &[&str]) -> Result<(), ControlError> {
        Err(ControlError::NeedsElevation)
    }
}

/// `arg` as one argument of a Windows command line, quoted the way
/// `CommandLineToArgvW` and the Rust runtime split it back: backslashes are
/// literal except before a quote, where each one and the quote are escaped.
#[cfg(any(test, target_os = "windows"))]
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        let escapes = if c == '"' {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        quoted.extend(std::iter::repeat_n('\\', escapes));
        quoted.push(c);
        backslashes = 0;
    }
    // The closing quote must not be escaped by a trailing backslash.
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::quote;

    /// A DirectShow device path has backslashes, `#`, `&` and braces but no
    /// space, so it passes through unquoted and arrives unchanged.
    #[test]
    fn a_device_path_needs_no_quotes() {
        let path = r"\\?\usb#vid_046d&pid_085e&mi_00#7&1a2b3c4d&0&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";
        assert_eq!(quote(path), path);
    }

    #[test]
    fn spaces_and_quotes_are_quoted_and_escaped() {
        assert_eq!(quote("a b"), r#""a b""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r"dir with space\"), r#""dir with space\\""#);
        assert_eq!(quote(""), r#""""#);
    }
}
