use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::{OnboardMode, ReportRate};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct OnboardArgs {
    /// Run against the device whose name contains this string.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: OnboardArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");

    let state = openlogi_hid::get_onboard(&route)
        .await
        .context("read onboard memory")?;
    let mode = match state.mode {
        OnboardMode::Onboard => "onboard",
        OnboardMode::Host => "host",
    };
    println!("mode: {mode}");
    for profile in &state.profiles {
        let active = if state.active_profile == Some(profile.index) {
            "  [active]"
        } else {
            ""
        };
        let enabled = if profile.enabled { "" } else { "  (disabled)" };
        let name = profile.name.as_deref().unwrap_or("—");
        println!("  profile {}: {name}{enabled}{active}", profile.index);
    }
    if let Some(rate) = &state.report_rate {
        let supported = rate
            .supported
            .iter()
            .map(ReportRate::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        println!("report rate: {} (supported: {supported})", rate.current);
    }
    Ok(())
}
