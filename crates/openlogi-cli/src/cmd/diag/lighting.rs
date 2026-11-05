//! `openlogi diag lighting <RRGGBB>` — set an online RGB device to a solid
//! colour via the same HID++ lighting write path the GUI uses.

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use openlogi_core::color::Rgb;
use openlogi_core::config::LightingKey;
use openlogi_hid::LightingMethod;

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

    /// A colour for one key over `color`, as `NAME=RRGGBB` (`Esc=ff0000`).
    /// Repeatable. Shown only on a keyboard whose zone map is known; needs
    /// `--method auto`.
    #[arg(long = "key", value_name = "NAME=RRGGBB")]
    pub keys: Vec<String>,
}

/// Parse one `--key NAME=RRGGBB`.
fn parse_key(arg: &str) -> Result<(LightingKey, Rgb)> {
    let (name, color) = arg
        .split_once('=')
        .with_context(|| format!("--key {arg:?}: expected NAME=RRGGBB"))?;
    let key = name
        .trim()
        .parse::<LightingKey>()
        .with_context(|| format!("--key {arg:?}"))?;
    let color = color
        .trim()
        .trim_start_matches('#')
        .parse::<Rgb>()
        .with_context(|| format!("--key {arg:?}"))?;
    Ok((key, color))
}

pub async fn run(args: LightingArgs) -> Result<()> {
    let color: Rgb = args.color.trim_start_matches('#').parse()?;
    let (r, g, b) = color.components();

    let mut keys = Vec::with_capacity(args.keys.len());
    for arg in &args.keys {
        let (key, color) = parse_key(arg)?;
        if keys.iter().any(|&(known, _)| known == key) {
            bail!("--key {key} is given more than once");
        }
        keys.push((key, color));
    }
    if !keys.is_empty() && !matches!(args.method, Method::Auto) {
        bail!("--key needs --method auto");
    }

    let (route, name) = select_device(args.device.as_deref(), LIGHTING_FEATURES).await?;

    if !keys.is_empty() {
        println!(
            "setting {name} ({route}) to #{r:02x}{g:02x}{b:02x} with {} key colours",
            keys.len()
        );
        let keys = keys
            .into_iter()
            .map(|(key, color)| (key.usage(), color))
            .collect();
        openlogi_hid::set_keyboard_key_colors(&route, color, keys).await?;
        println!("done");
        return Ok(());
    }

    let method: LightingMethod = args.method.into();
    println!("setting {name} ({route}) to #{r:02x}{g:02x}{b:02x} via {method:?}");
    openlogi_hid::set_keyboard_color_with(&route, method, r, g, b).await?;
    println!("done — {name} should now be solid #{r:02x}{g:02x}{b:02x}");
    Ok(())
}

#[cfg(test)]
mod color_validation_tests {
    use openlogi_core::color::RgbParseError;

    use super::{LightingArgs, Method, parse_key, run};

    fn args(color: &str) -> LightingArgs {
        LightingArgs {
            color: color.to_string(),
            device: None,
            method: Method::Auto,
            keys: Vec::new(),
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

    #[test]
    fn key_colours_parse_as_name_equals_hex() {
        let (key, color) = parse_key("esc=#ff0000").expect("a valid --key");
        assert_eq!(key.name(), "Esc");
        assert_eq!(color.components(), (0xff, 0, 0));
        parse_key("Esc").expect_err("not a valid --key");
        parse_key("Hyper=ff0000").expect_err("not a valid --key");
        parse_key("Esc=red").expect_err("not a valid --key");
    }

    #[tokio::test]
    async fn a_key_given_twice_is_refused() {
        let err = run(LightingArgs {
            keys: vec!["Esc=ff0000".into(), "esc=00ff00".into()],
            ..args("00ff00")
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[tokio::test]
    async fn key_colours_need_the_auto_method() {
        let err = run(LightingArgs {
            keys: vec!["Esc=ff0000".into()],
            method: Method::Perkeyv2,
            ..args("00ff00")
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--method auto"), "{err}");
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
