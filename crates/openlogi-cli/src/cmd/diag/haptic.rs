//! `openlogi diag haptic` — list what the haptic engine advertises and play it.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::Args;
use openlogi_core::hid::WriteError;
use openlogi_hid::{DeviceRoute, HapticReport, HapticWaveform};

use crate::cmd::diag::select_device;

/// HID++ `HapticFeedback`.
const HAPTIC_FEATURE: u16 = 0x19b0;

#[derive(Debug, Args)]
pub struct HapticArgs {
    /// Play this waveform by its Logitech name (e.g. `subtle_collision`,
    /// `whisper_collision`). Without `--waveform` or `--all`, only the
    /// advertised set is listed.
    #[arg(long, value_name = "NAME", conflicts_with = "all", value_parser = parse_waveform)]
    pub waveform: Option<HapticWaveform>,

    /// Play every waveform the device advertises, one after another.
    #[arg(long)]
    pub all: bool,

    /// Play each waveform this many times.
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub repeat: u32,

    /// Pause between plays, so each one can be felt on its own. Pauses
    /// shorter than a waveform runs blur into one buzz.
    #[arg(long, value_name = "MS", default_value_t = 400)]
    pub interval_ms: u64,

    /// Open the feature once and time only the plays: the steady-state
    /// latency of the agent's cached play, rather than the cold path.
    #[arg(long)]
    pub warm: bool,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

fn parse_waveform(name: &str) -> Result<HapticWaveform, String> {
    HapticWaveform::from_name(name).ok_or_else(|| {
        let names: Vec<_> = HapticWaveform::ALL
            .into_iter()
            .map(HapticWaveform::name)
            .collect();
        format!("unknown waveform `{name}`; one of: {}", names.join(", "))
    })
}

pub async fn run(args: HapticArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[HAPTIC_FEATURE]).await?;
    println!("device: {name} ({route})");

    let report = match openlogi_hid::read_haptic_report(&route).await {
        Err(WriteError::FeatureUnsupported { .. }) => {
            bail!("{name} does not expose HID++ 0x19b0 HapticFeedback")
        }
        result => result?,
    };
    print_report(&report);

    let waveforms: Vec<HapticWaveform> = match (args.waveform, args.all) {
        (Some(waveform), _) => vec![waveform],
        (None, true) => HapticWaveform::ALL
            .into_iter()
            .filter(|waveform| report.waveforms.supports(*waveform))
            .collect(),
        (None, false) => return Ok(()),
    };
    if !report
        .configuration
        .flags
        .contains(openlogi_hid::HapticConfigFlags::ENABLED)
    {
        println!(
            "  note: the firmware haptic engine is disabled (flags {:#04x}); \
             plays may be silent until it is enabled",
            report.configuration.flags.bits()
        );
    }

    if args.warm {
        let plays: Vec<_> = waveforms
            .iter()
            .flat_map(|waveform| std::iter::repeat_n(*waveform, args.repeat as usize))
            .collect();
        let interval = Duration::from_millis(args.interval_ms);
        for (waveform, took) in openlogi_hid::time_haptic_plays(&route, plays, interval).await? {
            println!(
                "  played {} warm in {:.1} ms",
                waveform.name(),
                took.as_secs_f64() * 1000.0
            );
        }
        return Ok(());
    }

    for waveform in waveforms {
        play(
            &route,
            waveform,
            args.repeat,
            Duration::from_millis(args.interval_ms),
        )
        .await?;
    }
    Ok(())
}

fn print_report(report: &HapticReport) {
    let config = &report.configuration;
    println!(
        "  config: flags={:#04x} intensity={}% levels={} (step {}%)",
        config.flags.bits(),
        config.intensity.get(),
        config.level_count,
        config.level_step
    );
    match report.status {
        Some(status) => println!(
            "  status: {:#04x} ({})",
            status.raw(),
            if status.is_ready() {
                "ready"
            } else {
                "refuses plays"
            }
        ),
        None => println!("  status: not answered"),
    }
    println!("  advertised mask: {:#010x}", report.waveforms.bits());
    for waveform in HapticWaveform::ALL {
        println!(
            "    {:>2} {:<20} {}",
            u8::from(waveform),
            waveform.name(),
            if report.waveforms.supports(waveform) {
                "yes"
            } else {
                "no"
            }
        );
    }
    let unnamed = report
        .waveforms
        .difference(openlogi_hid::SupportedWaveforms::all());
    if !unnamed.is_empty() {
        println!("    unnamed bits set: {:#010x}", unnamed.bits());
    }
}

/// Play `waveform` `repeat` times and print how long each call took.
///
/// Every call here opens the feature afresh (ping, root lookup, capability
/// and status reads, then the play), so the figures are an upper bound for the
/// cold path, not the steady-state latency of the agent's cached play.
async fn play(
    route: &DeviceRoute,
    waveform: HapticWaveform,
    repeat: u32,
    interval: Duration,
) -> Result<()> {
    for attempt in 1..=repeat {
        let started = Instant::now();
        let result = openlogi_hid::play_haptic(route, waveform).await;
        let took = started.elapsed();
        match result {
            Ok(()) => println!(
                "  played {} ({attempt}/{repeat}) in {:.1} ms",
                waveform.name(),
                took.as_secs_f64() * 1000.0
            ),
            Err(WriteError::HapticWaveformUnsupported { .. }) => {
                println!(
                    "  skipped {}: not advertised by this device",
                    waveform.name()
                );
                break;
            }
            Err(error) => bail!("playing {} failed: {error}", waveform.name()),
        }
        tokio::time::sleep(interval).await;
    }
    Ok(())
}
