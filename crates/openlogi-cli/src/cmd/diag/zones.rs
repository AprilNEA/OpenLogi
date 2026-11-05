//! `openlogi diag zones` — light each HID++ `0x8081` zone of a keyboard alone
//! and record which key it drives, the map per-key colours need.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_core::color::Rgb;
use openlogi_hid::write::{LitZone, ZoneWalkStep};
use openlogi_ipc::client::ConnectError;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct ZonesArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Colour of the lit zone as `RRGGBB` hex.
    #[arg(long, value_name = "RRGGBB", default_value = "ffffff")]
    pub color: String,

    /// Where to write the zone map, as tab-separated `zone_id` and `key`.
    #[arg(long, value_name = "FILE", default_value = "zones.tsv")]
    pub output: PathBuf,
}

/// What one answer at the prompt asks the walk to do.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    /// The key the lit zone drives, or `None` when nothing visible lit.
    Key(Option<String>),
    /// Go back one zone.
    Back,
    /// End the walk.
    Quit,
}

fn parse_answer(line: &str) -> Answer {
    match line.trim() {
        "" => Answer::Key(None),
        ":b" => Answer::Back,
        ":q" => Answer::Quit,
        key => Answer::Key(Some(key.to_owned())),
    }
}

pub async fn run(args: ZonesArgs) -> Result<()> {
    // The agent writes lighting whenever a device arrives; the walk does not
    // take its lighting lock, so they must not run together.
    if !matches!(
        crate::agent::connect().await,
        Err(ConnectError::Endpoint(_))
    ) {
        bail!("the OpenLogi agent is running; quit it, then run this again");
    }
    let lit: Rgb = args.color.trim_start_matches('#').parse()?;
    let (route, name) = select_device(args.device.as_deref(), &[0x8081]).await?;
    println!("device: {name} ({route})");
    println!(
        "Each zone lights up alone. Type the key it lit and press Enter.\n\
         Enter alone: nothing visible lit · `:b`: back one zone · `:q`: stop\n"
    );

    let mut keys: BTreeMap<u8, Option<String>> = BTreeMap::new();
    let walked = openlogi_hid::walk_per_key_zones(&route, lit, async |zone: LitZone| {
        let answer = match prompt(zone).await {
            Ok(line) => parse_answer(&line),
            Err(error) => {
                eprintln!("could not read the answer ({error}); stopping");
                Answer::Quit
            }
        };
        match answer {
            Answer::Key(key) => {
                keys.insert(zone.zone_id, key);
                ZoneWalkStep::Next
            }
            Answer::Back => ZoneWalkStep::Back,
            Answer::Quit => ZoneWalkStep::Stop,
        }
    })
    .await;

    // Keep what was named even when the walk failed part-way.
    if !keys.is_empty() {
        std::fs::write(&args.output, render(&keys))
            .with_context(|| format!("write {}", args.output.display()))?;
        println!("\nwrote {} zones to {}", keys.len(), args.output.display());
    }
    walked.context("walk the keyboard's zones")?;
    Ok(())
}

/// Ask which key `zone` lit, reading the answer off the async runtime.
async fn prompt(zone: LitZone) -> Result<String> {
    print!(
        "zone {}/{} (id {:#04x}): ",
        zone.position + 1,
        zone.total,
        zone.zone_id
    );
    std::io::stdout().flush()?;
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
    .await??;
    Ok(line)
}

/// The zone map as tab-separated `zone_id` (hex) and `key`; an empty key means
/// nothing visible lit.
fn render(keys: &BTreeMap<u8, Option<String>>) -> String {
    let mut out = String::from("zone_id\tkey\n");
    for (zone_id, key) in keys {
        let _ = writeln!(out, "{zone_id:#04x}\t{}", key.as_deref().unwrap_or(""));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_name_a_key_skip_go_back_or_quit() {
        assert_eq!(parse_answer("Esc\n"), Answer::Key(Some("Esc".into())));
        assert_eq!(parse_answer("  \n"), Answer::Key(None));
        assert_eq!(parse_answer("-"), Answer::Key(Some("-".into())));
        assert_eq!(parse_answer(":b"), Answer::Back);
        assert_eq!(parse_answer(":q"), Answer::Quit);
    }

    #[test]
    fn the_map_lists_zones_in_id_order() {
        let keys = BTreeMap::from([(0x0b, Some("B".to_owned())), (0x04, None)]);
        assert_eq!(render(&keys), "zone_id\tkey\n0x04\t\n0x0b\tB\n");
    }
}
