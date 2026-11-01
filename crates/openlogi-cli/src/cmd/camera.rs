//! `openlogi camera` — read and write device-level UVC image controls.
//!
//! Exercises the `openlogi-camera` UVC path (the same primitive the GUI controls
//! panel uses). Changes land in the camera's own registers, so other apps
//! (Google Meet, Zoom, OBS) see them too.

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand, ValueEnum};
use openlogi_camera::{AutoToggle, CameraControl, ControlError};

#[derive(Debug, Args)]
pub struct CameraArgs {
    #[command(subcommand)]
    pub cmd: Option<CameraCmd>,
    /// Operate on the camera with this unique id (default: first Logitech).
    #[arg(long, global = true)]
    pub camera: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum CameraCmd {
    /// Show each control's min/max/default/current and each auto mode's state
    /// (the default action).
    Get,
    /// Set a control to a value (or an auto toggle to 0/1); persists on the
    /// device.
    Set {
        /// zoom | pan | tilt | focus | exposure | field_of_view | show_mode |
        /// power_line_frequency | low_light_compensation | brightness |
        /// contrast | saturation | sharpness | gain | backlight_compensation |
        /// white_balance | tint, or focus_auto | exposure_auto |
        /// white_balance_auto
        control: String,
        value: i32,
    },
    /// Switch the built-in microphone on or off. The camera stores the
    /// switch and acts on it when it next enumerates, so this reconnects it
    /// unless told not to.
    Microphone {
        state: Switch,
        /// Store the switch only; it takes effect at the next replug.
        #[arg(long)]
        no_reconnect: bool,
    },
    /// Make the camera drop off the bus and enumerate again, as a replug
    /// would. Needs root on Linux and an administrator on Windows.
    Reconnect,
}

/// The two positions of a camera switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

pub fn run(args: CameraArgs) -> Result<()> {
    let uid = match args.camera {
        Some(id) => id,
        None => openlogi_camera::enumerate_cameras()
            .into_iter()
            .next()
            .map(|c| c.unique_id)
            .ok_or_else(|| anyhow!("no Logitech camera found"))?,
    };

    match args.cmd.unwrap_or(CameraCmd::Get) {
        CameraCmd::Get => {
            println!("controls for {uid}:");
            match openlogi_camera::read_camera_state(&uid) {
                Ok(state) if !state.controls.is_empty() => {
                    for (control, r) in &state.controls {
                        println!(
                            "  {}: min={} max={} default={} current={}",
                            control.name(),
                            r.min,
                            r.max,
                            r.default,
                            r.current
                        );
                    }
                    for (toggle, st) in &state.autos {
                        println!(
                            "  {}: current={} default={}",
                            toggle.name(),
                            st.current,
                            st.default
                        );
                    }
                    if let Some(on) = state.microphone {
                        println!("  microphone: {}", if on { "on" } else { "off" });
                    }
                }
                Ok(_) => println!("  (no adjustable controls, or camera not found)"),
                Err(e) => println!("  {e}"),
            }
        }
        CameraCmd::Set { control, value } => {
            let raw = control.to_ascii_lowercase();
            if let Some(toggle) = AutoToggle::ALL.iter().find(|t| t.name() == raw) {
                openlogi_camera::set_auto(&uid, *toggle, value != 0).map_err(|e| anyhow!("{e}"))?;
                println!("set {} = {}", toggle.name(), value != 0);
            } else {
                let control = parse_control(&raw)?;
                openlogi_camera::set_control(&uid, control, value).map_err(|e| anyhow!("{e}"))?;
                println!("set {} = {value}", control.name());
            }
        }
        CameraCmd::Microphone {
            state,
            no_reconnect,
        } => {
            let on = state == Switch::On;
            openlogi_camera::set_microphone(&uid, on).map_err(|e| anyhow!("{e}"))?;
            println!("stored microphone = {}", if on { "on" } else { "off" });
            if !no_reconnect {
                reconnect(&uid)?;
            }
        }
        CameraCmd::Reconnect => reconnect(&uid)?,
    }
    Ok(())
}

/// Reconnect the camera, and say how to get there when this process may not.
fn reconnect(uid: &str) -> Result<()> {
    match openlogi_camera::reconnect(uid) {
        Ok(()) => {
            println!("reconnected {uid}");
            Ok(())
        }
        Err(ControlError::NeedsElevation) => Err(anyhow!(
            "reconnecting the camera needs root (Linux) or an administrator (Windows): \
             rerun `openlogi camera reconnect` that way, or unplug the camera and plug it back in"
        )),
        Err(e) => Err(anyhow!("{e}")),
    }
}

fn parse_control(raw: &str) -> Result<CameraControl> {
    CameraControl::ALL
        .into_iter()
        .find(|c| c.name() == raw)
        .ok_or_else(|| {
            let names: Vec<&str> = CameraControl::ALL
                .iter()
                .map(|c| c.name())
                .chain(AutoToggle::ALL.iter().map(|t| t.name()))
                .collect();
            anyhow!("unknown control {raw:?} ({})", names.join("|"))
        })
}
