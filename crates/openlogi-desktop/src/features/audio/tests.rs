//! Which Audio rows a headset that refuses the power-off read shows.

use openlogi_core::config::Config;
use openlogi_core::device::{Capabilities, DeviceInventory, DeviceKind};
use openlogi_core::hid::{SidetoneLevel, WriteError};

use super::{PowerOffFacts, SidetoneFacts};
use crate::services::assets::AssetResolver;
use crate::services::ipc::{Command, ReadPowerOffTimeout, ReadSidetone};
use crate::state::tests::direct_inventory;
use crate::state::tests::pointer_speed::install;
use crate::state::{AppState, PowerOffTimeoutLoad, Sources};

/// A direct headset that lists `0x1F20`, with or without `0x8300`.
fn headset_inventory(sidetone: bool) -> DeviceInventory {
    let mut inventory = direct_inventory([0x0a, 0xc4, 0x00, 0x01]);
    inventory.paired[0].kind = DeviceKind::Headset;
    inventory.paired[0].capabilities = Some(Capabilities {
        sidetone,
        power_off_timeout: true,
        ..Capabilities::default()
    });
    inventory
}

/// Install the headset, answer its sidetone read with a level and refuse
/// its power-off read, as a headset that lists `0x1F20` only for its
/// battery does.
fn refusing_headset(sidetone: bool, cx: &mut gpui::TestAppContext) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[headset_inventory(sidetone)],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    install(state, cx);
    let mut refused = false;
    while let Ok(command) = receiver.try_recv() {
        match command {
            Command::ReadSidetone(ReadSidetone { reply, .. }) => {
                let _ = reply.send(Ok(SidetoneLevel::try_new(40).expect("a level in range")));
            }
            Command::ReadPowerOffTimeout(ReadPowerOffTimeout { reply, .. }) => {
                let _ = reply.send(Err(WriteError::FeatureUnsupported {
                    feature_hex: 0x1f20,
                }));
                refused = true;
            }
            _ => {}
        }
    }
    assert!(
        refused,
        "the state asked the headset for its power-off timeout"
    );
    cx.run_until_parked();
}

/// A G535-style headset: sidetone works, and its `0x1F20` is the battery
/// only. The tab shows the sidetone slider and no power-off row, as the
/// menu-bar menu hides the group, instead of a permanent "unsupported"
/// line under a working control.
#[gpui::test]
fn a_refused_power_off_read_beside_sidetone_shows_no_row(cx: &mut gpui::TestAppContext) {
    refusing_headset(true, cx);

    cx.read(|cx| {
        assert!(
            SidetoneFacts::current(cx).is_some(),
            "the sidetone row stays"
        );
        assert!(
            PowerOffFacts::current(cx).is_none(),
            "the refused power-off row is hidden"
        );
    });
}

/// Without sidetone the power-off row is all the tab has, so it stays and
/// says the setting is unsupported rather than leaving the tab blank.
#[gpui::test]
fn a_refused_power_off_read_alone_says_unsupported(cx: &mut gpui::TestAppContext) {
    refusing_headset(false, cx);

    cx.read(|cx| {
        assert!(SidetoneFacts::current(cx).is_none());
        let facts = PowerOffFacts::current(cx).expect("the power-off row stays");
        assert!(
            matches!(facts.load, PowerOffTimeoutLoad::Unsupported(_)),
            "the row says unsupported"
        );
    });
}
