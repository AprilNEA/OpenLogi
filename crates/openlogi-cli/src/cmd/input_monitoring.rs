//! The macOS Input Monitoring gate for commands that open devices directly
//! instead of reading them through the agent.

use anyhow::{Result, anyhow};

/// Raise the macOS Input Monitoring consent dialog before a command opens a
/// device directly, mirroring the agent's own gate
/// (`openlogi_agent::lifecycle::request_input_monitoring`).
///
/// Without this, `IOHIDDeviceOpen` is silently denied and this binary's own
/// Input Monitoring row never appears in System Settings at all for a user
/// to grant — `has_access()` only ever answers `false`, forever, for a
/// process that never once called the prompting API. Confirmed live on
/// `diag controls` (#1213): a device already granted to the packaged
/// `OpenLogi Agent` helper still failed to open here, because a grant to the
/// agent does nothing for this separate process. Run from a shell, macOS
/// judges the app that launched the command (the terminal), so that is the
/// row the dialog adds.
pub(crate) async fn request() -> Result<()> {
    if openlogi_hid::permissions::has_access() {
        return Ok(());
    }
    tokio::task::spawn_blocking(openlogi_hid::permissions::request_access)
        .await
        .map_err(|e| anyhow!("Input Monitoring permission request task failed: {e}"))?;
    // A grant (or an existing denial) is never visible to this same
    // process — macOS only updates a process's cached answer on its next
    // launch, the same reason the agent relaunches itself after a fresh
    // grant. A one-shot CLI command has no equivalent to relaunch into, so
    // there is nothing to continue with here regardless of what the user
    // just chose.
    Err(anyhow!(
        "Input Monitoring is not granted to this command — grant it to the app \
         that launched it (for example Terminal) under System Settings → \
         Privacy & Security → Input Monitoring, then run the command again (a \
         fresh grant takes effect on the next run)"
    ))
}
