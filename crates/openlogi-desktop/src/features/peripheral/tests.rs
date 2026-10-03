use super::*;
use gpui::{Modifiers, TestAppContext};
use openlogi_core::{config::Config, device::DeviceKind, peripheral::*};
use openlogi_ipc::{AgentSnapshot, AgentStatus, InventoryHealth, PROTOCOL_VERSION};

use crate::{services::assets::AssetResolver, state::Sources};

#[gpui::test]
fn single_key_editor_saves_and_keeps_the_previous_binding_after_invalid_input(
    cx: &mut TestAppContext,
) {
    let snapshot = snapshot();
    let record = snapshot.peripherals.devices[0].clone();
    let capability = record.capabilities[0].id.clone();
    let Capability::InputRemap(input) = &record.capabilities[0].capability else {
        panic!("input capability");
    };
    let control = input.controls[0].id.clone();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    cx.update(gpui_component::init);
    cx.update(|cx| {
        let resolver = AssetResolver::new();
        let state = cx.new(|_| {
            let mut state =
                AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands));
            state.apply_agent_snapshot(&snapshot, &resolver, &[]);
            state
        });
        AppState::set_global(state, cx);
    });
    let (panel, cx) = cx.add_window_view(|_, cx| PeripheralPanel::new(cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let choose = cx
        .debug_bounds("input-remap/main-button")
        .expect("the declared control is visible")
        .center();
    cx.simulate_click(choose, Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let save = cx
        .debug_bounds("shortcut-submit")
        .expect("the shared shortcut picker opens")
        .center();
    cx.simulate_click(save, Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let state = AppState::try_read(cx).unwrap();
        let rule = state.peripheral_rule(&record).unwrap().unwrap();
        assert_eq!(
            rule.capabilities[&capability].bindings[&control],
            Action::CustomShortcut("F18".parse().unwrap())
        );
        assert!(
            !rule.enabled,
            "choosing a target alone must not enable device writes"
        );
        assert!(panel.read(cx).editor.is_none());
    });
    let choose = cx
        .debug_bounds("input-remap/main-button")
        .expect("the saved binding is editable")
        .center();
    cx.simulate_click(choose, Modifiers::default());
    cx.update(|window, cx| {
        let input = panel.read(cx).editor.as_ref().unwrap().input.clone();
        input.update(cx, |input, cx| input.set_value("Ctrl+F18", window, cx));
        window.draw(cx).clear(cx);
    });
    let save = cx
        .debug_bounds("shortcut-submit")
        .expect("the shared shortcut picker reopens")
        .center();
    cx.simulate_click(save, Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let state = AppState::try_read(cx).unwrap();
        assert!(matches!(
            state.peripheral_issue(),
            Some(PeripheralError::InvalidSettings(_))
        ));
        let rule = state.peripheral_rule(&record).unwrap().unwrap();
        assert_eq!(
            rule.capabilities[&capability].bindings[&control],
            Action::CustomShortcut("F18".parse().unwrap())
        );
        assert!(
            panel.read(cx).editor.is_some(),
            "invalid input stays editable"
        );
    });
}

fn snapshot() -> AgentSnapshot {
    let capability = CapabilityId::try_new("input-remap/main").unwrap();
    let control = ControlId::try_new("button").unwrap();
    let record = PeripheralRecord {
        model: ModelId::try_new("example.button").unwrap(),
        physical: None,
        session: SessionId {
            endpoint: EndpointId("mock".into()),
            generation: 1,
        },
        endpoints: Vec::new(),
        name: "Test button".into(),
        kind: DeviceKind::Unknown,
        driver: DriverSelection {
            descriptor: DescriptorId::try_new("example.usb").unwrap(),
            driver: DriverId::try_new("example.native").unwrap(),
            source: DriverSource::Builtin,
            digest: None,
        },
        driver_error: None,
        connection: ConnectionStatus::Online,
        scopes: vec![ScopeKind::Model],
        capabilities: vec![CapabilityRecord {
            id: capability.clone(),
            version: 1,
            capability: Capability::InputRemap(InputRemapCapability {
                controls: vec![InputControl {
                    id: control.clone(),
                    labels: BTreeMap::from([("en".into(), "Button".into())]),
                    source: InputSource::HidUsage(HidUsage {
                        page: 12,
                        usage: 233,
                    }),
                    trigger: Trigger::ShortPress,
                    recommended_key: Some("F18".parse().unwrap()),
                }],
                targets: TargetKind::KeyboardKey,
                per_app: false,
            }),
            scopes: vec![ScopeKind::Model],
            unavailable: None,
            evidence: CapabilityEvidence::Declared,
            values: BTreeMap::new(),
        }],
        operations: Vec::new(),
    };
    AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: true,
            hook_installed: false,
            launch_at_login: false,
            inventory: InventoryHealth::Ready,
            protocol_version: PROTOCOL_VERSION,
            agent_version: "test-mock".into(),
            input_monitoring_granted: true,
            hid_open_failures: false,
        },
        inventory: Vec::new(),
        standalone: Vec::new(),
        camera_active: false,
        pairing: None,
        foreground: openlogi_ipc::ForegroundApps::default(),
        peripherals: PeripheralSnapshot {
            devices: vec![record.clone()],
            ..PeripheralSnapshot::default()
        },
    }
}
