//! `openlogi diag effects` — list the HID++ `0x8071` effects a device offers,
//! or run one with hand-typed parameters to learn what they do.

use std::io::Write as _;

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_ipc::client::ConnectError;

use crate::cmd::diag::select_device;

/// Parameter bytes `setRgbClusterEffect` carries.
const PARAM_COUNT: usize = 10;

#[derive(Debug, Args)]
pub struct EffectsArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Run this effect id (as `diag effects` lists it, e.g. `0x000a`) on
    /// every cluster that offers it, then ask for parameters again until
    /// `:q`. Without it, only list the effects.
    #[arg(long = "try", value_name = "EFFECT_ID", value_parser = parse_effect_id)]
    pub effect: Option<u16>,

    /// The first parameters to try, as up to ten hex bytes (`ff0000 0bb8`);
    /// missing bytes are zero. Asked for when left out.
    #[arg(long, value_name = "HEX")]
    pub params: Option<String>,
}

pub async fn run(args: EffectsArgs) -> Result<()> {
    // The agent writes lighting whenever a device arrives and does not share
    // its lighting lock with this command, so they must not run together.
    if !matches!(
        crate::agent::connect().await,
        Err(ConnectError::Endpoint(_))
    ) {
        bail!("the OpenLogi agent is running; quit it, then run this again");
    }
    let (route, name) = select_device(args.device.as_deref(), &[0x8071]).await?;
    println!("device: {name} ({route})");

    let Some(effect_id) = args.effect else {
        let effects = openlogi_hid::rgb_effects(&route)
            .await
            .context("read the 0x8071 effects")?;
        println!("cluster  location  index  effect_id  capabilities  period_ms");
        for effect in &effects {
            println!(
                "{:>7}  {:#06x}    {:>5}  {:#06x}     {:#06x}        {}",
                effect.cluster,
                effect.location,
                effect.index,
                effect.effect_id,
                effect.capabilities,
                effect.period
            );
            for info in &effect.onboard {
                let kind = match info.kind {
                    0 => "state",
                    1 => "defaults",
                    4 => "name",
                    _ => "info",
                };
                let bytes = info.bytes.as_ref().map_or_else(
                    || "refused".to_owned(),
                    |bytes| {
                        let text = if info.kind == 4 {
                            effect_name(bytes)
                        } else {
                            String::new()
                        };
                        format!("{}{text}", hex(bytes))
                    },
                );
                println!("           slot {} {kind:<8} {bytes}", info.slot);
            }
        }
        if effects.is_empty() {
            println!("no 0x8071 effects");
        }
        return Ok(());
    };

    let mut line = match args.params {
        Some(params) => params,
        None => match prompt("params (hex bytes, :q to stop): ").await? {
            Some(line) => line,
            None => return Ok(()),
        },
    };
    loop {
        if line.trim() == ":q" {
            return Ok(());
        }
        let params = match parse_params(&line) {
            Ok(params) => params,
            Err(error) => {
                eprintln!("{error}");
                match prompt("params (hex bytes, :q to stop): ").await? {
                    Some(next) => {
                        line = next;
                        continue;
                    }
                    None => return Ok(()),
                }
            }
        };
        println!("effect {effect_id:#06x} with {}", hex(&params));
        let mut next = None;
        let clusters = openlogi_hid::try_rgb_effect(&route, effect_id, params, async || {
            next = prompt("next params (hex bytes, :q to stop): ")
                .await
                .unwrap_or_default();
        })
        .await
        .context("run the effect")?;
        if clusters == 0 {
            bail!("no cluster offers effect {effect_id:#06x}; run `diag effects` for the list");
        }
        match next {
            Some(next) => line = next,
            None => return Ok(()),
        }
    }
}

fn parse_effect_id(text: &str) -> Result<u16, String> {
    let digits = text.trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(digits, 16).map_err(|error| format!("`{text}`: {error}"))
}

/// Up to ten hex bytes, spaces anywhere; missing bytes are zero.
fn parse_params(text: &str) -> Result<[u8; PARAM_COUNT]> {
    let digits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !digits.len().is_multiple_of(2) {
        bail!("`{text}`: an odd number of hex digits");
    }
    if digits.len() > PARAM_COUNT * 2 {
        bail!("`{text}`: more than {PARAM_COUNT} bytes");
    }
    let mut params = [0; PARAM_COUNT];
    for (slot, pair) in params.iter_mut().zip(digits.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).context("hex digits")?;
        *slot = u8::from_str_radix(pair, 16).with_context(|| format!("`{pair}` is not hex"))?;
    }
    Ok(params)
}

/// The printable ASCII in `bytes`, quoted, for an effect name read.
fn effect_name(bytes: &[u8]) -> String {
    let text: String = bytes
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|&byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                char::from(byte)
            } else {
                '.'
            }
        })
        .collect();
    format!("  \"{text}\"")
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Print `question` and read one line off the async runtime; `None` at the
/// end of input.
async fn prompt(question: &str) -> Result<Option<String>> {
    print!("{question}");
    std::io::stdout().flush()?;
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map(|read| (read > 0).then_some(line))
    })
    .await??;
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_pad_with_zeros_and_ignore_spaces() {
        let params = parse_params("ff 00 00 0b b8").expect("params parse");
        assert_eq!(params, [0xff, 0, 0, 0x0b, 0xb8, 0, 0, 0, 0, 0]);
        parse_params("abc").expect_err("odd digit count");
        parse_params("00".repeat(11).as_str()).expect_err("too many bytes");
        parse_params("zz").expect_err("not hex");
    }

    #[test]
    fn effect_ids_read_as_hex_with_or_without_prefix() {
        assert_eq!(parse_effect_id("0x000a"), Ok(0x0a));
        assert_eq!(parse_effect_id("0B"), Ok(0x0b));
    }
}
