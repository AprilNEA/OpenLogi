use std::process::Command;

use super::Motion;

pub(super) const MOTION_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

#[must_use]
pub(super) fn read() -> Option<Motion> {
    let mut command = Command::new("gsettings");
    command.args(["get", "org.gnome.desktop.interface", "enable-animations"]);
    read_bool_command(command, MOTION_QUERY_TIMEOUT).map(|enabled| {
        if enabled {
            Motion::Full
        } else {
            Motion::Reduced
        }
    })
}

pub(super) fn read_bool_command(
    mut command: Command,
    timeout: std::time::Duration,
) -> Option<bool> {
    use std::io::Read as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

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

pub(super) fn parse_gsettings_bool(stdout: &str) -> Option<bool> {
    match stdout.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}
