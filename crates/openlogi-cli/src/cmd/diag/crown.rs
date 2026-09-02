//! `openlogi diag crown` — read the HID++ `0x4600 Crown` capabilities and
//! current mode on the Craft's rotary crown, write its ratchet mode, or
//! sample its diverted event stream.
//!
//! `get_info`/`get_mode` were the M2 smoke test confirming `crown.rs` decodes
//! real firmware payloads (previously only exercised against unit-test
//! fixtures); `--ratchet` extended that to the write path. `--listen` is the
//! M4 event-pipeline smoke test: it confirms what sign
//! `relative_slot_rotation` reports for a physical clockwise turn, which
//! settles how the new `ButtonId` rotation-direction variants get named
//! before any of them are added.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use openlogi_hid::{CrownEvent, CrownInfo, CrownMode, RatchetMode, SetCrownMode};

use crate::cmd::diag::select_device;

/// Upper bound on events collected by `--listen`, so a stuck-diverted crown
/// (or a very fast spin) can't make the command run away.
const LISTEN_MAX_EVENTS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RatchetModeArg {
    /// Free-spinning mode.
    Free,
    /// Ratchet (detented) mode.
    Ratchet,
}

impl From<RatchetModeArg> for RatchetMode {
    fn from(value: RatchetModeArg) -> Self {
        match value {
            RatchetModeArg::Free => Self::Free,
            RatchetModeArg::Ratchet => Self::Ratchet,
        }
    }
}

#[derive(Debug, Args)]
pub struct CrownArgs {
    /// Ratchet mode to write directly to the device. This does not update
    /// config.toml; there is no persistent config for this yet.
    #[arg(long, value_enum)]
    pub ratchet: Option<RatchetModeArg>,

    /// Divert the crown and print diverted events for this many seconds
    /// (rotate it by hand while this runs), then restore its original
    /// reporting mode. Mutually exclusive with `--ratchet`.
    #[arg(long, value_name = "SECONDS", conflicts_with = "ratchet")]
    pub listen: Option<u64>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: CrownArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x4600]).await?;
    println!("device: {name} ({route})");

    let info = openlogi_hid::get_crown_info(&route)
        .await
        .context("read Crown info")?;
    print_info(info);

    let before = openlogi_hid::get_crown_mode(&route)
        .await
        .context("read Crown mode")?;
    print_mode("current", before);

    if let Some(seconds) = args.listen {
        println!(
            "listening for up to {LISTEN_MAX_EVENTS} events over {seconds}s — rotate the crown \
             clockwise, then counterclockwise, and press it"
        );
        let events = openlogi_hid::sample_crown_events(
            &route,
            LISTEN_MAX_EVENTS,
            Duration::from_secs(seconds),
        )
        .await
        .context("sample Crown events")?;
        if events.is_empty() {
            println!("  (no events — is the crown being turned?)");
        }
        for (i, event) in events.iter().enumerate() {
            print_event(i, *event);
        }
        let restored = openlogi_hid::get_crown_mode(&route)
            .await
            .context("read Crown mode after --listen")?;
        print_mode("restored", restored);
        if restored.diverting != before.diverting {
            anyhow::bail!(
                "Crown reporting mode not restored: was {:?}, now {:?}",
                before.diverting,
                restored.diverting
            );
        }
        return Ok(());
    }

    let Some(requested) = args.ratchet.map(RatchetMode::from) else {
        return Ok(());
    };

    let after = openlogi_hid::set_crown_mode(
        &route,
        SetCrownMode {
            diverting: None,
            ratchet_mode: Some(requested),
            rotation_timeout: None,
            short_long_timeout: None,
            double_tap_speed: None,
        },
    )
    .await
    .context("set Crown ratchet mode")?;
    print_mode("read-back", after);

    if after.ratchet_mode != requested {
        anyhow::bail!(
            "Crown ratchet mode write not applied: requested {requested:?}, device reports {:?}",
            after.ratchet_mode
        );
    }
    if after.diverting != before.diverting {
        anyhow::bail!(
            "Crown reporting mode changed unexpectedly: was {:?}, now {:?}",
            before.diverting,
            after.diverting
        );
    }

    println!("✓ Crown ratchet mode set to {requested:?} (reporting mode preserved)");
    Ok(())
}

fn print_info(info: CrownInfo) {
    println!(
        "  info: controls={:?} sensors={:?} slots={} ratchets={}",
        info.controls, info.sensors, info.slots, info.ratchets
    );
}

fn print_event(index: usize, event: CrownEvent) {
    // `CrownEvent` is `#[non_exhaustive]`: a future firmware event this crate
    // does not decode is possible even though `Update` is the only variant
    // today.
    let CrownEvent::Update(update) = event else {
        println!("  [{index:>3}] unrecognized crown event: {event:?}");
        return;
    };
    println!(
        "  [{index:>3}] rotation_state={:?} slot={:+} ratchet={:+} speed={:+} proximity={:?} \
         touch={:?} gesture={:?} button={:?}",
        update.rotation_state,
        update.relative_slot_rotation,
        update.relative_ratchet_rotation,
        update.speed,
        update.proximity,
        update.touch,
        update.gesture,
        update.button,
    );
}

fn print_mode(label: &str, mode: CrownMode) {
    println!(
        "  {label}: diverting={:?} ratchet_mode={:?} rotation_timeout={} short_long_timeout={} double_tap_speed={}",
        mode.diverting,
        mode.ratchet_mode,
        mode.rotation_timeout,
        mode.short_long_timeout,
        mode.double_tap_speed
    );
}
