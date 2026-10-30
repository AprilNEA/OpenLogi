//! The Linux reduce-motion preference: GNOME's `enable-animations`, read
//! through `gsettings` and killed if it hangs. Other desktops keep motion
//! full.

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::MotionSetting;

/// How long a `gsettings` read may run before it is killed.
const QUERY_TIMEOUT: Duration = Duration::from_millis(200);

/// Blocks the calling thread for up to [`QUERY_TIMEOUT`].
pub(super) fn read() -> Option<MotionSetting> {
    let mut command = Command::new("gsettings");
    command.args(["get", "org.gnome.desktop.interface", "enable-animations"]);
    read_bool_command(command, QUERY_TIMEOUT).map(MotionSetting::from_animations)
}

/// Run `command` and parse its stdout as a `gsettings` boolean, killing it if
/// it has not exited within `timeout`.
fn read_bool_command(mut command: Command, timeout: Duration) -> Option<bool> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut stdout = String::new();
                child.stdout.take()?.read_to_string(&mut stdout).ok()?;
                return parse_gsettings_bool(&stdout);
            }
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `gsettings get` prints a GVariant boolean; anything else is no signal.
fn parse_gsettings_bool(stdout: &str) -> Option<bool> {
    match stdout.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::{QUERY_TIMEOUT, parse_gsettings_bool, read_bool_command};

    #[test]
    fn gsettings_booleans_parse_and_anything_else_is_no_signal() {
        assert_eq!(parse_gsettings_bool("true\n"), Some(true));
        assert_eq!(parse_gsettings_bool("false\n"), Some(false));
        assert_eq!(parse_gsettings_bool(""), None);
        assert_eq!(parse_gsettings_bool("No such schema\n"), None);
    }

    #[test]
    fn a_motion_query_returns_its_parsed_answer() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo false"]);
        assert_eq!(
            read_bool_command(command, Duration::from_secs(5)),
            Some(false)
        );
    }

    #[test]
    fn a_hung_motion_query_is_killed_at_its_timeout() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5; echo false"]);
        let started = Instant::now();
        assert_eq!(read_bool_command(command, QUERY_TIMEOUT), None);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
