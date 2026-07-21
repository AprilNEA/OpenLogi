//! `openlogi camera` — read and write device-level UVC image controls.
//!
//! Exercises the `openlogi-camera` UVC path (the same primitive the GUI controls
//! panel uses). Changes land in the camera's own registers, so other apps
//! (Google Meet, Zoom, OBS) see them too.

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use openlogi_camera::{AutoToggle, CameraControl};

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
        /// zoom | focus | exposure | brightness | contrast | saturation |
        /// sharpness | white_balance | tint, or focus_auto | exposure_auto |
        /// white_balance_auto
        control: String,
        value: i32,
    },
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
    }
    Ok(())
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
