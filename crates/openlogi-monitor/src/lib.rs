//! Monitor discovery and DDC/CI input switching.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInputAssignment {
    pub monitor_id: String,
    pub input: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: String,
    pub friendly_name: String,
    pub display_name: String,
    pub description: String,
    pub current_input: Option<u32>,
    pub inputs: Vec<MonitorInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInput {
    pub value: u32,
    pub label: String,
}

#[derive(Debug, Error)]
pub enum MonitorError {
    #[error("monitor control is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("Windows monitor API failed while {operation}")]
    WindowsApi { operation: &'static str },
    #[error("monitor was not found: {0}")]
    MonitorNotFound(String),
}

pub fn list_monitors() -> Result<Vec<MonitorInfo>, MonitorError> {
    platform::list_monitors()
}

pub fn set_monitor_input(monitor_id: &str, input: u32) -> Result<(), MonitorError> {
    platform::set_monitor_input(monitor_id, input)
}

pub fn test_monitor_input(
    monitor_id: &str,
    input: u32,
    restore_after: Duration,
) -> Result<(), MonitorError> {
    if restore_after.is_zero() {
        return set_monitor_input(monitor_id, input);
    }
    let previous = list_monitors()?
        .into_iter()
        .find(|monitor| monitor.id == monitor_id)
        .and_then(|monitor| monitor.current_input);
    set_monitor_input(monitor_id, input)?;
    if let Some(previous) = previous.filter(|previous| *previous != input) {
        std::thread::sleep(restore_after);
        set_monitor_input(monitor_id, previous)?;
    }
    Ok(())
}

pub fn apply_input_assignments(assignments: &[MonitorInputAssignment]) {
    let mut ordered = assignments.iter().enumerate().collect::<Vec<_>>();
    ordered
        .sort_by_key(|(index, assignment)| (monitor_switch_order(&assignment.monitor_id), *index));

    for (_, assignment) in ordered {
        if let Err(error) = set_monitor_input(&assignment.monitor_id, assignment.input) {
            tracing::warn!(
                %error,
                monitor_id = assignment.monitor_id,
                input = assignment.input,
                "monitor input switch failed"
            );
        }
    }
}

fn monitor_switch_order(monitor_id: &str) -> u8 {
    if monitor_id.starts_with(r"\\.\DISPLAY2") {
        0
    } else if monitor_id.starts_with(r"\\.\DISPLAY1") {
        1
    } else {
        2
    }
}

pub fn input_label(value: u32) -> String {
    match value {
        0x01 => "DVI-1",
        0x02 => "DVI-2",
        0x03 => "VGA-1",
        0x04 => "S-Video-1",
        0x05 => "Composite-1",
        0x06 => "Component-1",
        0x07 => "Component-2",
        0x08 => "DisplayPort-1",
        0x09 => "DisplayPort-2",
        0x0f => "DisplayPort-1",
        0x10 => "DisplayPort-2",
        0x11 => "HDMI-1",
        0x12 => "HDMI-2",
        0x13 => "USB-C",
        0x14 => "USB-C-2",
        _ => return format!("Input 0x{value:02x}"),
    }
    .to_string()
}

#[cfg(target_os = "windows")]
mod platform;

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{MonitorError, MonitorInfo};

    pub fn list_monitors() -> Result<Vec<MonitorInfo>, MonitorError> {
        Err(MonitorError::UnsupportedPlatform)
    }

    pub fn set_monitor_input(_monitor_id: &str, _input: u32) -> Result<(), MonitorError> {
        Err(MonitorError::UnsupportedPlatform)
    }
}
