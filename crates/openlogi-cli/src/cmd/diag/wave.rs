//! `openlogi diag wave` — a host-drawn wave across a per-key keyboard, with
//! chosen keys held at their own colour, to see how it looks and how many
//! frames per second the keyboard takes.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_core::color::Rgb;
use openlogi_core::config::LightingKey;
use openlogi_ipc::client::ConnectError;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct WaveArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// The wave's colour.
    #[arg(long, default_value = "00a0ff", value_name = "RRGGBB")]
    pub color: Rgb,

    /// A key held at its own colour while the wave passes, as `NAME=RRGGBB`
    /// (`W=ff00ff`). Repeatable.
    #[arg(long = "key", value_name = "NAME=RRGGBB", value_parser = parse_key)]
    pub keys: Vec<(LightingKey, Rgb)>,

    /// Milliseconds the wave takes to cross the keyboard.
    #[arg(long, default_value_t = 3000)]
    pub period_ms: u64,

    /// The keys' brightness outside the band, in percent (1–100). Held keys
    /// keep their own colour at full strength.
    #[arg(long, default_value_t = 70)]
    pub brightness: u8,

    /// The lit part's width, as a share of the keyboard (0.05–1).
    #[arg(long, default_value_t = 0.35)]
    pub width: f32,

    /// Brightness steps the wave is drawn in. Fewer steps take fewer requests
    /// per frame, so more frames per second.
    #[arg(long, default_value_t = 8)]
    pub steps: u8,

    /// The darkest the passing band gets, as a share of `--brightness`
    /// (0–1): 0.5 dims the keys to half. With `--light`, the share the keys
    /// the band is not on keep.
    #[arg(long, default_value_t = 0.5)]
    pub floor: f32,

    /// Draw a lit band crossing dark keys, instead of a dark band crossing
    /// lit ones.
    #[arg(long)]
    pub light: bool,

    /// Seconds to run.
    #[arg(long, default_value_t = 20)]
    pub seconds: u64,

    /// Draw the wave from right to left.
    #[arg(long)]
    pub reverse: bool,

    /// After the first frame, send only the keys whose colour changed.
    #[arg(long)]
    pub delta: bool,

    /// The depot metadata whose `PERKEY_KEYBOARD` zone places the keys.
    #[arg(long, value_name = "PATH")]
    pub metadata: Option<PathBuf>,
}

pub async fn run(args: WaveArgs) -> Result<()> {
    if !matches!(
        crate::agent::connect().await,
        Err(ConnectError::Endpoint(_))
    ) {
        bail!("the OpenLogi agent is running; quit it, then run this again");
    }
    let path = match args.metadata.clone() {
        Some(path) => path,
        None => dirs_home()?.join(".local/share/openlogi/assets/g515_ls_tkl/metadata.json"),
    };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let metadata: openlogi_assets::Metadata =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let centres = key_centres(&metadata);
    if centres.is_empty() {
        bail!("{} draws no PERKEY_KEYBOARD keys", path.display());
    }

    let (route, name) = select_device(args.device.as_deref(), &[0x8081]).await?;
    println!(
        "device: {name} ({route}); {} keys, {} held; {} s",
        centres.len(),
        args.keys.len(),
        args.seconds
    );

    let stop = Arc::new(AtomicBool::new(false));
    let timer = {
        let stop = Arc::clone(&stop);
        let seconds = args.seconds;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            stop.store(true, Ordering::Relaxed);
        })
    };
    let wave = Wave::new(&args);
    let held = args.keys.clone();
    let delta = args.delta;
    let render = move |elapsed: Duration| {
        let phase = wave.phase(elapsed);
        centres
            .iter()
            .map(|&(usage, x)| {
                let color = held
                    .iter()
                    .rev()
                    .find(|(key, _)| key.usage() == usage)
                    .map_or_else(|| wave.color_at(x, phase), |(_, color)| *color);
                (usage, color)
            })
            .collect()
    };
    let run =
        openlogi_hid::animate_per_key(&route, render, || !stop.load(Ordering::Relaxed), delta)
            .await
            .context("run the wave")?;
    timer.abort();
    let seconds = run.elapsed.as_secs_f64().max(f64::EPSILON);
    println!(
        "{} frames in {seconds:.1} s: {:.1} frames per second; up to {} requests a frame",
        run.frames,
        f64::from(run.frames) / seconds,
        run.max_requests
    );
    Ok(())
}

/// Each drawn key's usage and the horizontal centre of its box, from 0 (left
/// edge of the keys) to 1 (right edge).
fn key_centres(metadata: &openlogi_assets::Metadata) -> Vec<(u8, f32)> {
    let regions = metadata.key_regions();
    let left = regions
        .iter()
        .map(|r| r.rect.x)
        .fold(f32::INFINITY, f32::min);
    let right = regions
        .iter()
        .map(|r| r.rect.x + r.rect.width)
        .fold(f32::NEG_INFINITY, f32::max);
    let span = (right - left).max(1.);
    regions
        .iter()
        .filter(|region| LightingKey::from_usage(region.usage).is_some())
        .map(|region| {
            let centre = region.rect.x + region.rect.width / 2.;
            (region.usage, (centre - left) / span)
        })
        .collect()
}

/// A band crossing the keyboard. By default the keys are lit and the band
/// darkens them, deepest at its front and recovering behind it; `light`
/// flips that into a lit band over dark keys.
#[derive(Debug, Clone, Copy)]
struct Wave {
    color: Rgb,
    period: f32,
    width: f32,
    steps: f32,
    floor: f32,
    reverse: bool,
    light: bool,
}

impl Wave {
    fn new(args: &WaveArgs) -> Self {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a period in milliseconds fits an f32 closely enough"
        )]
        let period = args.period_ms.max(100) as f32 / 1000.;
        Self {
            color: scale(args.color, f32::from(args.brightness.clamp(1, 100)) / 100.),
            period,
            width: args.width.clamp(0.05, 1.),
            steps: f32::from(args.steps.max(2)),
            floor: args.floor.clamp(0., 1.),
            reverse: args.reverse,
            light: args.light,
        }
    }

    /// Where the front is, from just before the left edge to just past the
    /// right one, so the band enters and leaves whole.
    fn phase(&self, elapsed: Duration) -> f32 {
        let turn = (elapsed.as_secs_f32() / self.period).fract();
        turn * (1. + self.width)
    }

    fn color_at(&self, x: f32, front: f32) -> Rgb {
        let x = if self.reverse { 1. - x } else { x };
        // Behind the front (it moves towards larger x) the light fades over
        // `width`; ahead of it the key is still dark.
        let behind = front - x;
        let lit = if (0. ..=self.width).contains(&behind) {
            1. - behind / self.width
        } else {
            0.
        };
        let band = if self.light { lit } else { 1. - lit };
        let level = (band * (self.steps - 1.)).round() / (self.steps - 1.);
        let share = self.floor + (1. - self.floor) * level;
        scale(self.color, share)
    }
}

fn scale(color: Rgb, share: f32) -> Rgb {
    let (r, g, b) = color.components();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "share is clamped to 0–1, so the product stays in 0–255"
    )]
    let channel = |value: u8| (f32::from(value) * share.clamp(0., 1.)).round() as u8;
    Rgb::new(channel(r), channel(g), channel(b))
}

fn parse_key(text: &str) -> Result<(LightingKey, Rgb), String> {
    let (name, color) = text
        .split_once('=')
        .ok_or_else(|| format!("`{text}`: expected NAME=RRGGBB"))?;
    let key = name
        .parse::<LightingKey>()
        .map_err(|error| format!("`{name}`: {error}"))?;
    let color = color
        .trim_start_matches('#')
        .parse::<Rgb>()
        .map_err(|error| format!("`{color}`: {error}"))?;
    Ok((key, color))
}

fn dirs_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set; pass --metadata")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave() -> Wave {
        Wave {
            color: Rgb::new(0, 0xa0, 0xff),
            period: 2.,
            width: 0.5,
            steps: 5.,
            floor: 0.,
            reverse: false,
            light: true,
        }
    }

    #[test]
    fn a_dark_band_is_deepest_at_its_front_over_lit_keys() {
        let wave = Wave {
            light: false,
            ..wave()
        };
        assert_eq!(wave.color_at(0.5, 0.5), Rgb::new(0, 0, 0), "the front");
        assert_eq!(wave.color_at(0.75, 0.5), Rgb::new(0, 0xa0, 0xff), "ahead");
        assert_eq!(
            wave.color_at(0.0, 0.5),
            Rgb::new(0, 0xa0, 0xff),
            "recovered"
        );
    }

    #[test]
    fn the_front_is_brightest_and_the_light_fades_behind_it() {
        let wave = wave();
        assert_eq!(wave.color_at(0.5, 0.5), Rgb::new(0, 0xa0, 0xff));
        assert_eq!(
            wave.color_at(0.75, 0.5),
            Rgb::new(0, 0, 0),
            "ahead of the front"
        );
        assert_eq!(wave.color_at(0.0, 0.5), Rgb::new(0, 0, 0), "past the tail");
        let mid = wave.color_at(0.25, 0.5);
        assert!(
            mid.components().2 > 0 && mid.components().2 < 0xff,
            "{mid:?}"
        );
    }

    #[test]
    fn keys_parse_as_name_and_colour() {
        let (key, color) = parse_key("W=ff00ff").expect("parses");
        assert_eq!(key.name(), "W");
        assert_eq!(color, Rgb::new(0xff, 0, 0xff));
        parse_key("W").expect_err("no colour");
        parse_key("Nope=ff00ff").expect_err("unknown key");
    }
}
