//! Pointer speed and precision mode as the orchestrator drives them: which
//! mice get the saved speed, and which reloads end precision mode.

use openlogi_core::hid::PointerSpeed;
use openlogi_hid::DpiTarget;

use super::super::{assignments_changed, saved_speed};
use super::*;
use crate::pointer_speed::{PrecisionPress, SavedSpeed, SpeedTarget};

fn speed(raw: u16) -> PointerSpeed {
    PointerSpeed::try_new(raw).expect("a speed in range")
}

/// An online mouse `a` whose speed `0x2205` scales.
fn scaled_mouse() -> AgentDevice {
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        pointer_scaling: true,
        ..Capabilities::default()
    });
    device
}

/// An online mouse `a` whose sensor DPI sets its speed.
fn dpi_mouse() -> AgentDevice {
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        pointer: true,
        ..Capabilities::default()
    });
    device
}

/// An orchestrator with [`scaled_mouse`] online, its precision mode on.
fn orchestrator_in_precision(config: Config) -> Orchestrator {
    orchestrator_with_precision_on(config, scaled_mouse())
}

/// An orchestrator with `mouse` online as `a`, its precision mode on.
fn orchestrator_with_precision_on(config: Config, mouse: AgentDevice) -> Orchestrator {
    let mut orch = orchestrator(config);
    orch.devices = vec![mouse];
    orch.publish_device_runtime();
    let (on, _) = orch
        .shared
        .pointer_speeds
        .toggle_precision(Some("a"), PrecisionPress::Assigned)
        .expect("an online mouse whose speed OpenLogi sets");
    assert!(on);
    orch
}

#[test]
fn saved_speed_follows_the_speed_feature_and_the_saved_value() {
    let mut config = Config::default();
    let mut device = scaled_mouse();
    assert_eq!(
        saved_speed(&config, &device),
        Some(SavedSpeed::Scaled(None))
    );
    config.set_pointer_speed("a", speed(0x0180));
    assert_eq!(
        saved_speed(&config, &device),
        Some(SavedSpeed::Scaled(Some(speed(0x0180))))
    );

    // Sensor DPI sets the speed of a DPI mouse, even with `0x2205`.
    device.capabilities = Some(Capabilities {
        pointer: true,
        pointer_scaling: true,
        ..Capabilities::default()
    });
    assert_eq!(saved_speed(&config, &device), Some(SavedSpeed::Dpi(None)));
    config.set_dpi("a", Dpi::new(1600));
    assert_eq!(
        saved_speed(&config, &device),
        Some(SavedSpeed::Dpi(Some(Dpi::new(1600))))
    );

    // A mouse without either feature, or not yet probed, is never written.
    device.capabilities = Some(Capabilities::default());
    assert_eq!(saved_speed(&config, &device), None);
    device.capabilities = None;
    assert_eq!(saved_speed(&config, &device), None);
}

#[test]
fn assignments_change_with_bindings_but_not_with_other_settings() {
    let mut previous = Config::default();
    previous.set_binding(
        "a",
        ButtonId::Back,
        Binding::Single(Action::PreviousDesktop),
    );

    let mut same = previous.clone();
    same.set_dpi_presets("a", vec![Dpi::new(800), Dpi::new(1600)]);
    assert!(!assignments_changed(&previous, &same, "a"));
    assert!(!assignments_changed(&previous, &previous, "absent"));
    // Creating the entry with a first preset leaves the default assignments.
    let mut created = Config::default();
    created.set_dpi_presets("b", vec![Dpi::new(800)]);
    assert!(!assignments_changed(&Config::default(), &created, "b"));

    let mut rebound = previous.clone();
    rebound.set_binding(
        "a",
        ButtonId::Back,
        Binding::Single(Action::TogglePrecisionMode),
    );
    assert!(assignments_changed(&previous, &rebound, "a"));

    let mut per_app = previous.clone();
    per_app.set_per_app_binding("a", "browser", ButtonId::Back, Some(Action::BrowserBack));
    assert!(assignments_changed(&previous, &per_app, "a"));

    assert!(assignments_changed(&Config::default(), &previous, "a"));
}

#[test]
fn a_reload_that_rebinds_a_button_ends_precision_mode() {
    let orch_config = Config::default();
    let mut orch = orchestrator_in_precision(orch_config.clone());

    let mut rebound = orch_config;
    rebound.set_binding(
        "a",
        ButtonId::Back,
        Binding::Single(Action::PreviousDesktop),
    );
    orch.replace_config(rebound);

    assert!(!orch.shared.pointer_speeds.precision("a"));
}

#[test]
fn an_unrelated_reload_keeps_precision_mode() {
    let config = Config::default();
    let mut orch = orchestrator_in_precision(config.clone());

    let mut presets = config;
    presets.set_dpi_presets("a", vec![Dpi::new(800), Dpi::new(1600)]);
    orch.replace_config(presets);

    assert!(orch.shared.pointer_speeds.precision("a"));
}

#[test]
fn a_reload_ends_precision_mode_on_a_mouse_that_is_offline() {
    let config = Config::default();
    let mut orch = orchestrator_in_precision(config.clone());
    orch.devices[0].online = false;
    orch.publish_device_runtime();

    let mut rebound = config;
    rebound.set_binding(
        "a",
        ButtonId::Back,
        Binding::Single(Action::PreviousDesktop),
    );
    orch.replace_config(rebound);

    assert!(!orch.shared.pointer_speeds.precision("a"));
    // Nothing is saved, so the reconnect writes 1×, not the precision speed.
    let route = orch.devices[0].route.clone().expect("a route");
    let write = orch
        .shared
        .pointer_speeds
        .reapply("a", &route, SavedSpeed::Scaled(None))
        .expect("a write owed to the reconnect");
    assert_eq!(write.target, SpeedTarget::Scaled(PointerSpeed::DEFAULT));
}

#[test]
fn a_reload_with_a_new_speed_ends_precision_mode() {
    let config = Config::default();
    let mut orch = orchestrator_in_precision(config.clone());

    let mut faster = config;
    faster.set_pointer_speed("a", speed(0x0200));
    orch.replace_config(faster);

    assert!(!orch.shared.pointer_speeds.precision("a"));
}

#[test]
fn a_mouse_not_yet_probed_has_no_precision_mode() {
    let mut orch = orchestrator(Config::default());
    orch.devices = vec![dev("a", 1, true)];
    orch.publish_device_runtime();

    assert!(
        orch.shared
            .pointer_speeds
            .toggle_precision(Some("a"), PrecisionPress::Assigned)
            .is_none()
    );
}

#[test]
fn a_reload_with_a_new_dpi_ends_precision_mode_on_a_dpi_mouse() {
    let config = Config::default();
    let mut orch = orchestrator_with_precision_on(config.clone(), dpi_mouse());

    let mut faster = config;
    faster.set_dpi("a", Dpi::new(1600));
    orch.replace_config(faster);

    assert!(!orch.shared.pointer_speeds.precision("a"));
}

#[test]
fn an_offline_dpi_mouse_whose_mode_a_reload_ends_reconnects_at_its_default_dpi() {
    let config = Config::default();
    let mut orch = orchestrator_with_precision_on(config.clone(), dpi_mouse());
    orch.devices[0].online = false;
    orch.publish_device_runtime();

    let mut rebound = config;
    rebound.set_binding(
        "a",
        ButtonId::Back,
        Binding::Single(Action::PreviousDesktop),
    );
    orch.replace_config(rebound);

    assert!(!orch.shared.pointer_speeds.precision("a"));
    let route = orch.devices[0].route.clone().expect("a route");
    let write = orch
        .shared
        .pointer_speeds
        .reapply("a", &route, SavedSpeed::Dpi(None))
        .expect("a write owed to the reconnect");
    assert_eq!(write.target, SpeedTarget::Dpi(DpiTarget::Default));
}
