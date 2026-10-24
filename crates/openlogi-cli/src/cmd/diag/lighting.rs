//! `openlogi diag lighting <RRGGBB>` — set an online RGB device to a colour,
//! held steady or animated, via the same HID++ lighting write path the GUI
//! uses. Nothing is saved: the device keeps it until it loses power.

use anyhow::Result;
use clap::{Args, ValueEnum};
use openlogi_core::color::Rgb;
use openlogi_core::config::{Lighting, LightingEffect};
use openlogi_hid::LightingMethod;
use openlogi_hid::write::LightingWrite;

use super::select_device;

const LIGHTING_FEATURES: &[u16] = &[0x8070, 0x8071, 0x8080, 0x8081];

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Method {
    /// Prefer 0x8070 then 0x8071 effects, falling back to 0x8081 then 0x8080.
    Auto,
    /// Force 0x8070 ColorLedEffects (the fixed-effect onboard override).
    Effects,
    /// Force 0x8080 PerKeyLighting (the raw per-key stream).
    Perkey,
    /// Force 0x8081 PerKeyLighting2 (the zone-addressed successor).
    Perkeyv2,
}

impl From<Method> for LightingMethod {
    fn from(m: Method) -> Self {
        match m {
            Method::Auto => Self::Auto,
            Method::Effects => Self::Effects,
            Method::Perkey => Self::PerKey,
            Method::Perkeyv2 => Self::PerKeyV2,
        }
    }
}

/// The animation to play, as `config.toml`'s `effect` names it.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Effect {
    /// The colour held steady.
    Static,
    /// The colour fading in and out (0x8070 only).
    Breathing,
    /// Every hue in turn; the colour is not used (0x8070 only).
    ColorCycle,
}

impl From<Effect> for LightingEffect {
    fn from(effect: Effect) -> Self {
        match effect {
            Effect::Static => Self::Static,
            Effect::Breathing => Self::Breathing,
            Effect::ColorCycle => Self::ColorCycle,
        }
    }
}

#[derive(Debug, Args)]
pub struct LightingArgs {
    /// Colour as `RRGGBB` hex (e.g. `ff0000` for red).
    pub color: String,

    /// Run against the online device whose name contains this string
    /// (case-insensitive). Useful when several devices are connected.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Which HID++ lighting path to drive.
    #[arg(long, value_enum, default_value_t = Method::Auto)]
    pub method: Method,

    /// How the colour animates. A device that cannot play the effect shows
    /// the colour instead.
    #[arg(long, value_enum, default_value_t = Effect::Static)]
    pub effect: Effect,

    /// One breath or colour-wheel turn, in milliseconds (1000–20000).
    #[arg(long, value_name = "MS", default_value_t = 5_000,
          value_parser = clap::value_parser!(u16).range(1_000..=20_000))]
    pub period_ms: u16,

    /// Brightness percent (0–100), applied as the GUI applies it.
    #[arg(long, value_name = "PERCENT", default_value_t = 100,
          value_parser = clap::value_parser!(u8).range(0..=100))]
    pub brightness: u8,
}

pub async fn run(args: LightingArgs) -> Result<()> {
    let color: Rgb = args.color.trim_start_matches('#').parse()?;
    let (r, g, b) = color.components();

    let (route, name) = select_device(args.device.as_deref(), LIGHTING_FEATURES).await?;

    let method: LightingMethod = args.method.into();
    let effect: LightingEffect = args.effect.into();
    let write = LightingWrite {
        method,
        ..LightingWrite::for_settings(&Lighting {
            enabled: true,
            color,
            brightness: args.brightness,
            effect,
            effect_period_ms: args.period_ms,
        })
    };
    println!(
        "setting {name} ({route}) to #{r:02x}{g:02x}{b:02x} at {}%, {:?}, via {method:?}",
        args.brightness, write.animation
    );
    openlogi_hid::write_lighting(&route, write).await?;
    println!("done — {name} should now show #{r:02x}{g:02x}{b:02x} as {effect:?}");
    Ok(())
}

#[cfg(test)]
mod color_validation_tests {
    use openlogi_core::color::RgbParseError;

    use super::{Effect, LightingArgs, Method, run};

    fn args(color: &str) -> LightingArgs {
        LightingArgs {
            color: color.to_string(),
            device: None,
            method: Method::Auto,
            effect: Effect::Static,
            period_ms: 5_000,
            brightness: 100,
        }
    }

    /// Invalid colours are rejected before any device I/O, so `run` is safe to
    /// call in-process here. Valid colours proceed to hardware enumeration and
    /// are deliberately not exercised.
    #[tokio::test]
    async fn rejects_malformed_colors_before_touching_hardware() {
        for bad in ["zzz", "ff000", "ff00001", "gg0000", ""] {
            let err = run(args(bad)).await.unwrap_err();
            assert!(
                err.downcast_ref::<RgbParseError>().is_some(),
                "{bad:?} should fail Rgb parsing, got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn hash_prefix_is_stripped_before_validation() {
        // `#zzzzzz` still fails, and the rejected input the error reports is
        // `zzzzzz` — proving the `#` is stripped rather than counted toward
        // the 6-digit length.
        let err = run(args("#zzzzzz")).await.unwrap_err();
        let parse = err
            .downcast_ref::<RgbParseError>()
            .expect("Rgb parse error");
        assert_eq!(
            parse.to_string(),
            r#"invalid RGB color "zzzzzz": expected 6 hex digits ("RRGGBB", no '#')"#
        );
    }
}
