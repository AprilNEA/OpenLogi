//! `openlogi diag mouse-buttons` — probe HID++ `0x8100` `OnboardProfiles` and
//! `0x8110` `MouseButtonSpy`, the gaming-line feature pair G-series mice
//! (e.g. the G502 X PLUS) report instead of `0x1b04 ReprogControlsV4`.
//!
//! Read-only: this never writes onboard-profile memory or the device's
//! onboard/host mode. Its purpose is to answer, against real hardware, the
//! questions button-remap support for these devices needs before it can be
//! designed: does the spy emit events without a mode switch, what's the
//! button-index-to-physical-button mapping, and which buttons also still
//! produce a native OS click alongside a spy event.

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_hid::{DeviceRoute, EmittingFeature, MouseButtonIndex, MouseButtonSpyEvent};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct MouseButtonsArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// devices are paired (e.g. a mouse and a keyboard over Bluetooth).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Start the `0x8110` button spy and print each event live, until Enter
    /// is pressed, instead of the default one-shot descriptor/mode/count read.
    #[arg(long)]
    pub watch: bool,
}

pub async fn run(args: MouseButtonsArgs) -> Result<()> {
    // 0x8110 = MouseButtonSpy — the feature present on G-series gaming mice
    // that lack 0x1b04 ReprogControlsV4 (e.g. the G502 X PLUS).
    let (route, name) = select_device(args.device.as_deref(), &[0x8110]).await?;
    println!("device: {name} ({route})");

    match openlogi_hid::dump_onboard_profiles(&route).await {
        Ok(info) => {
            println!(
                "  0x8100 OnboardProfiles: mode={:?} profile_format=0x{:02x} \
                 button_count={} profiles={}+{} oob sectors={}x{}B",
                info.mode,
                info.description.profile_format_id,
                info.description.button_count,
                info.description.profile_count,
                info.description.profile_count_oob,
                info.description.sector_count,
                info.description.sector_size,
            );
        }
        Err(e) => println!("  0x8100 OnboardProfiles: not available ({e:#})"),
    }

    let spy_available = match openlogi_hid::dump_mouse_button_count(&route).await {
        Ok(count) => {
            println!("  0x8110 MouseButtonSpy: button_count={count}");
            true
        }
        Err(e) => {
            println!("  0x8110 MouseButtonSpy: not available ({e:#})");
            false
        }
    };

    if !args.watch {
        return Ok(());
    }
    if !spy_available {
        bail!("--watch requires HID++ 0x8110 MouseButtonSpy, which this device doesn't report");
    }
    watch(&route).await
}

/// Streams `0x8110` button-state events until Enter is pressed, then stops
/// the spy. A raw `std::thread` reads the blocking stdin line so this needs
/// no extra tokio feature beyond what's already enabled (`sync`, `macros`).
async fn watch(route: &DeviceRoute) -> Result<()> {
    let feature = openlogi_hid::open_mouse_button_spy(route)
        .await
        .context("open HID++ 0x8110 MouseButtonSpy")?;
    let events = feature.listen();
    feature
        .start_spy()
        .await
        .context("start HID++ 0x8110 MouseButtonSpy")?;

    println!(
        "watching — press each physical button once, in isolation, then press Enter here to stop"
    );

    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = stop_tx.send(());
    });

    loop {
        tokio::select! {
            event = events.recv() => {
                let Ok(MouseButtonSpyEvent::Buttons(mask)) = event else { break };
                let down: Vec<u8> = mask.pressed().map(MouseButtonIndex::get).collect();
                println!("  mask=0x{:04x}  down={down:?}", mask.bits());
            }
            _ = &mut stop_rx => break,
        }
    }

    feature
        .stop_spy()
        .await
        .context("stop HID++ 0x8110 MouseButtonSpy")?;
    Ok(())
}
