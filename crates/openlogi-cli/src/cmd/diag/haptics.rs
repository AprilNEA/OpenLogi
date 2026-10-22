//! `openlogi diag haptics` — inspect, configure, and play the HID++ `0x19b0`
//! haptic feature.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use openlogi_hid::{
    DeviceRoute, HapticChange, HapticConfigFlags, HapticConfiguration, HapticIntensity,
    HapticReport, HapticWaveform,
};

use crate::cmd::api::Switch;
use crate::cmd::diag::select_device;

/// The pause between waveforms in `--play all`, long enough to tell them
/// apart by feel.
const PLAY_ALL_GAP: Duration = Duration::from_millis(1500);

#[derive(Debug, Args)]
pub struct HapticsArgs {
    /// Turn the firmware haptic engine on or off. Written directly to the
    /// device; this does not update config.toml.
    #[arg(long, value_enum)]
    pub engine: Option<Switch>,

    /// Turn battery-saving haptics on or off.
    #[arg(long, value_enum)]
    pub power_save: Option<Switch>,

    /// Intensity percentage (0–100) to write.
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(0..=100))]
    pub intensity: Option<u8>,

    /// Play one waveform ID, or `all` to play every advertised waveform in
    /// turn.
    #[arg(long, value_name = "ID|all")]
    pub play: Option<String>,

    /// Stop the waveform playing now.
    #[arg(long)]
    pub stop: bool,

    /// Read which waveform is playing.
    #[arg(long)]
    pub playing: bool,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: HapticsArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x19b0]).await?;
    println!("device: {name} ({route})");

    let report = openlogi_hid::read_haptics(&route)
        .await
        .context("read haptic feature")?;
    print_report(&report);

    let change = HapticChange {
        enabled: args.engine.map(Switch::is_on),
        power_save: args.power_save.map(Switch::is_on),
        intensity: args.intensity.and_then(HapticIntensity::new),
    };
    if change != HapticChange::default() {
        write_change(&route, change).await?;
    }
    if let Some(play) = args.play.as_deref() {
        play_waveforms(&route, &report, play).await?;
    }
    if args.stop {
        let status = openlogi_hid::stop_haptic(&route)
            .await
            .context("stop haptic waveform")?;
        println!("  stop: was {status:?}");
    }
    if args.playing {
        let (waveform, status) = openlogi_hid::get_playing_haptic(&route)
            .await
            .context("read playing haptic waveform")?;
        println!("  playing: waveform={waveform} status={status:?}");
    }
    Ok(())
}

/// Write `change`, then check the device reports every field it asked for.
async fn write_change(route: &DeviceRoute, change: HapticChange) -> Result<()> {
    let applied = openlogi_hid::set_haptics(route, change)
        .await
        .context("write haptic configuration")?;
    print_configuration("read-back", applied);

    let mismatch = |field: &str, requested: String, actual: String| {
        anyhow!("haptic {field} write not applied: requested {requested}, device reports {actual}")
    };
    if let Some(enabled) = change.enabled
        && applied.flags.contains(HapticConfigFlags::ENABLED) != enabled
    {
        return Err(mismatch(
            "engine",
            enabled.to_string(),
            (!enabled).to_string(),
        ));
    }
    if let Some(power_save) = change.power_save
        && applied.flags.contains(HapticConfigFlags::POWER_SAVE) != power_save
    {
        return Err(mismatch(
            "power save",
            power_save.to_string(),
            (!power_save).to_string(),
        ));
    }
    if let Some(intensity) = change.intensity
        && applied.intensity != intensity
    {
        return Err(mismatch(
            "intensity",
            intensity.get().to_string(),
            applied.intensity.get().to_string(),
        ));
    }
    println!("✓ haptic configuration applied");
    Ok(())
}

async fn play_waveforms(route: &DeviceRoute, report: &HapticReport, play: &str) -> Result<()> {
    let waveforms = if play.eq_ignore_ascii_case("all") {
        advertised_waveforms(report)
    } else {
        let id: u8 = play
            .parse()
            .map_err(|_| anyhow!("--play takes a waveform ID or `all`, not {play:?}"))?;
        vec![HapticWaveform::try_from(id).map_err(|_| anyhow!("unknown waveform ID {id}"))?]
    };
    if !report
        .configuration
        .flags
        .contains(HapticConfigFlags::ENABLED)
    {
        println!("  note: the haptic engine is off; plays may be accepted but not felt");
    }
    for (position, waveform) in waveforms.iter().enumerate() {
        if position > 0 {
            tokio::time::sleep(PLAY_ALL_GAP).await;
        }
        let reply = openlogi_hid::play_haptic(route, *waveform)
            .await
            .with_context(|| format!("play {waveform:?}"))?;
        println!(
            "  play {:>2} {waveform:?}: reply waveform={} status={} ({})",
            u8::from(*waveform),
            reply.waveform,
            reply.status,
            if reply.started() {
                "started"
            } else {
                "not started"
            }
        );
    }
    if waveforms.is_empty() {
        bail!("the device advertises no waveform this build can name");
    }
    Ok(())
}

/// The named waveforms the device advertises, in ID order.
fn advertised_waveforms(report: &HapticReport) -> Vec<HapticWaveform> {
    (0..=u8::MAX)
        .filter_map(|id| HapticWaveform::try_from(id).ok())
        .filter(|waveform| report.capabilities.waveforms.supports(*waveform))
        .collect()
}

fn print_report(report: &HapticReport) {
    let caps = report.capabilities;
    println!(
        "  capabilities: reserved={:#04x} flags={:#04x} levels={:?} default_intensity={} mask={:#010x}",
        caps.reserved,
        caps.flags.bits(),
        caps.levels,
        caps.default_intensity,
        caps.waveforms.bits(),
    );
    println!("    waveform ids: {}", waveform_ids(caps.waveforms.bits()));
    print_configuration("configuration", report.configuration);
    println!(
        "  status: {:#04x} ({})",
        report.status.raw(),
        if report.status.is_ready() {
            "ready"
        } else {
            "not ready"
        }
    );
}

fn print_configuration(label: &str, config: HapticConfiguration) {
    println!(
        "  {label}: flags={:#04x} (engine={} power_save={}) intensity={} levels={:#04x}",
        config.flags.bits(),
        config.flags.contains(HapticConfigFlags::ENABLED),
        config.flags.contains(HapticConfigFlags::POWER_SAVE),
        config.intensity.get(),
        (config.level_step << 4) | config.level_count,
    );
}

/// The waveform IDs a capability mask advertises: bit *N* is ID *N*.
fn waveform_ids(mask: u32) -> String {
    let ids: Vec<String> = (0..32)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit: u32| bit.to_string())
        .collect();
    if ids.is_empty() {
        "none".to_owned()
    } else {
        ids.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_bits_are_listed_as_waveform_ids() {
        assert_eq!(
            waveform_ids(0x0800_7fff),
            "0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,27"
        );
        assert_eq!(waveform_ids(0), "none");
    }
}
