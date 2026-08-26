//! `openlogi diag onboard-profiles-write-test` — round-trip test for HID++
//! `0x8100`'s write path: read a sector, write the exact same content back,
//! read again, and confirm the content is unchanged and the trailing CRC is
//! self-consistent.
//!
//! Content bytes (everything but the last 2) must come back byte-identical.
//! The trailing 2 bytes are allowed to *change*: `write_sector` always
//! re-stamps them with a freshly computed CRC over the content before
//! sending (see `hidpp::feature::onboard_profiles`), so a sector whose
//! existing trailer didn't already match its own content (e.g. a blank,
//! never-written sector, all `0xff`) will legitimately get a different
//! trailer back — that's the write path correctly doing its job, not
//! corruption. What actually matters is verified here instead: the new
//! trailer must equal `crc_ccitt` of the returned content, computed
//! independently by this command rather than trusted from the write call.
//!
//! This performs a **real write** to onboard flash. Not risk-free (a device
//! disconnect mid-write could leave the sector short of what it started
//! with), so run it deliberately, one sector at a time, and read the printed
//! diff before trusting the result.

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::crc_ccitt;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct OnboardProfilesWriteTestArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Which onboard-memory sector to round-trip.
    #[arg(long, value_name = "SECTOR")]
    pub sector: u16,
}

pub async fn run(args: OnboardProfilesWriteTestArgs) -> Result<()> {
    // 0x8100 = OnboardProfiles.
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");

    let info = openlogi_hid::dump_onboard_profiles_info(&route)
        .await
        .context("dump HID++ 0x8100 onboard-profiles info")?;

    println!(
        "reading sector {:#06x} ({} bytes) before write...",
        args.sector, info.sector_size
    );
    let before = openlogi_hid::dump_onboard_profiles_sector(&route, args.sector, info.sector_size)
        .await
        .context("read sector before write")?;

    println!("writing the same {} bytes back...", before.len());
    openlogi_hid::write_onboard_profiles_sector(&route, args.sector, before.clone())
        .await
        .context("write sector")?;

    println!("reading sector {:#06x} again after write...", args.sector);
    let after = openlogi_hid::dump_onboard_profiles_sector(&route, args.sector, info.sector_size)
        .await
        .context("read sector after write")?;

    if before.len() != after.len() {
        anyhow::bail!(
            "length changed: was {} bytes, now {} bytes",
            before.len(),
            after.len()
        );
    }
    let trailer_start = after.len() - 2;

    let content_matches = before[..trailer_start] == after[..trailer_start];
    let expected_trailer = crc_ccitt(&after[..trailer_start]).to_be_bytes();
    let trailer_self_consistent = after[trailer_start..] == expected_trailer;
    let trailer_changed = before[trailer_start..] != after[trailer_start..];

    if content_matches && trailer_self_consistent {
        if trailer_changed {
            println!(
                "PASS: sector {:#06x} content round-tripped byte-for-byte; \
                 trailer was re-stamped to {:02x}{:02x} (its old trailer \
                 {:02x}{:02x} didn't match its own content — expected for a \
                 blank/unwritten sector).",
                args.sector,
                expected_trailer[0],
                expected_trailer[1],
                before[trailer_start],
                before[trailer_start + 1]
            );
        } else {
            println!(
                "PASS: sector {:#06x} round-tripped byte-for-byte, including \
                 the trailer (it already matched its own content).",
                args.sector
            );
        }
        return Ok(());
    }

    println!("FAIL: sector {:#06x} round trip is not clean:", args.sector);
    for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
        if b != a {
            println!("  byte {i:#06x}: was {b:02x}, now {a:02x}");
        }
    }
    if !trailer_self_consistent {
        println!(
            "  trailer {:02x}{:02x} does not match crc_ccitt of the returned content ({:02x}{:02x})",
            after[trailer_start],
            after[trailer_start + 1],
            expected_trailer[0],
            expected_trailer[1]
        );
    }
    anyhow::bail!("round-trip mismatch on sector {:#06x}", args.sector);
}
