use super::*;
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, DeviceModelInfo, DeviceTransports, PairedDevice, ReceiverInfo,
};

#[derive(Default)]
struct FakeBackend {
    live: BTreeMap<String, AccessoryPower>,
    prepares: usize,
    writes: usize,
    removals: std::rc::Rc<std::cell::Cell<usize>>,
    unavailable: bool,
    fail_write_slot: Option<u8>,
    fail_remove: bool,
}

impl PowerSourceBackend for FakeBackend {
    fn prepare(&mut self) -> Result<(), PowerSourceError> {
        self.prepares += 1;
        if self.unavailable {
            Err(PowerSourceError::Unavailable("missing test SPI"))
        } else {
            Ok(())
        }
    }

    fn upsert(&mut self, accessory: &AccessoryPower) -> Result<(), PowerSourceError> {
        self.writes += 1;
        if self
            .fail_write_slot
            .is_some_and(|slot| accessory.identifier.ends_with(&format!("slot:{slot}")))
        {
            return Err(PowerSourceError::Operation {
                operation: "set",
                code: -1,
            });
        }
        self.live
            .insert(accessory.identifier.clone(), accessory.clone());
        Ok(())
    }

    fn remove(&mut self, identifier: &str) -> Result<(), PowerSourceError> {
        self.removals.set(self.removals.get() + 1);
        self.live.remove(identifier);
        if self.fail_remove {
            Err(PowerSourceError::Operation {
                operation: "release",
                code: -2,
            })
        } else {
            Ok(())
        }
    }
}

fn enabled_config() -> Config {
    let mut config = Config::default();
    config.app_settings.macos_battery_widget = true;
    config
}

fn inventory() -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Logi Bolt Receiver".into(),
            vendor_id: 0x046d,
            product_id: 0xc548,
            unique_id: Some("test-receiver".into()),
        },
        paired: vec![PairedDevice {
            slot: 1,
            codename: Some("MX Master 3S".into()),
            wpid: Some(0xb034),
            kind: DeviceKind::Mouse,
            online: true,
            battery: Some(BatteryInfo {
                percentage: 77,
                level: BatteryLevel::Good,
                status: BatteryStatus::Discharging,
            }),
            model_info: None,
            capabilities: None,
        }],
    }
}

#[test]
fn default_config_never_loads_spi() {
    let mut publisher = PowerSourcePublisher::new(FakeBackend::default());
    publisher.reconcile(&Config::default(), &[inventory()], Instant::now());
    assert_eq!(publisher.status(), BatteryWidgetStatus::Disabled);
    assert_eq!(publisher.backend.prepares, 0);
    assert_eq!(publisher.backend.writes, 0);
}

#[test]
fn updates_only_changed_readings_and_uses_physical_custom_name() {
    let mut config = enabled_config();
    let mut inv = inventory();
    inv.paired[0].model_info = Some(DeviceModelInfo {
        entity_count: 1,
        serial_number: None,
        unit_id: [1, 2, 3, 4],
        transports: DeviceTransports::default(),
        model_ids: [0xb034, 0, 0],
        extended_model_id: 0,
    });
    config.set_device_custom_name("unit:01020304", Some("Office Mouse".into()));
    let mut publisher = PowerSourcePublisher::new(FakeBackend::default());
    let now = Instant::now();
    publisher.reconcile(&config, std::slice::from_ref(&inv), now);
    let reading = publisher.backend.live.values().next().unwrap();
    assert_eq!(reading.name, "Office Mouse");
    assert_eq!(reading.percentage, 77);
    assert_eq!(reading.status, BatteryStatus::Discharging);
    publisher.reconcile(&config, std::slice::from_ref(&inv), now + OFFLINE_GRACE);
    assert_eq!(publisher.backend.writes, 1);
    config.set_device_custom_name("unit:01020304", Some("Renamed".into()));
    inv.paired[0].battery.as_mut().unwrap().status = BatteryStatus::ChargingSlow;
    publisher.reconcile(&config, &[inv], now + OFFLINE_GRACE);
    let reading = publisher.backend.live.values().next().unwrap();
    assert_eq!(reading.name, "Renamed");
    assert_eq!(reading.status, BatteryStatus::ChargingSlow);
    assert_eq!(publisher.backend.writes, 2);
}

#[test]
fn full_charge_transition_is_published_without_losing_external_power_state() {
    let now = Instant::now();
    let config = enabled_config();
    let mut inv = inventory();
    let mut publisher = PowerSourcePublisher::new(FakeBackend::default());
    inv.paired[0].battery.as_mut().unwrap().percentage = 100;
    inv.paired[0].battery.as_mut().unwrap().status = BatteryStatus::Charging;
    publisher.reconcile(&config, std::slice::from_ref(&inv), now);
    inv.paired[0].battery.as_mut().unwrap().status = BatteryStatus::Full;
    publisher.reconcile(&config, &[inv], now);
    assert_eq!(publisher.backend.writes, 2);
    assert_eq!(
        publisher.backend.live.values().next().unwrap().status,
        BatteryStatus::Full
    );
}

#[test]
fn offline_and_absent_devices_receive_the_full_grace_period() {
    for absent in [false, true] {
        let mut publisher = PowerSourcePublisher::new(FakeBackend::default());
        let config = enabled_config();
        let now = Instant::now();
        let mut inv = inventory();
        publisher.reconcile(&config, std::slice::from_ref(&inv), now);
        inv.paired[0].online = false;
        let offline = if absent { vec![] } else { vec![inv] };
        let offline_at = now + Duration::from_secs(10);
        publisher.reconcile(&config, &offline, offline_at);
        assert_eq!(publisher.next_deadline(), Some(offline_at + OFFLINE_GRACE));
        publisher.reconcile(
            &config,
            &offline,
            offline_at + OFFLINE_GRACE.saturating_sub(Duration::from_secs(1)),
        );
        assert_eq!(publisher.backend.live.len(), 1);
        publisher.reconcile(&config, &offline, offline_at + OFFLINE_GRACE);
        assert!(publisher.backend.live.is_empty());
        assert_eq!(publisher.next_deadline(), None);
    }
}

#[test]
fn online_without_telemetry_cancels_offline_deadline_and_keeps_last_reading() {
    let mut publisher = PowerSourcePublisher::new(FakeBackend::default());
    let config = enabled_config();
    let now = Instant::now();
    let mut inv = inventory();
    publisher.reconcile(&config, std::slice::from_ref(&inv), now);
    publisher.reconcile(&config, &[], now + Duration::from_secs(1));
    inv.paired[0].battery = None;
    publisher.reconcile(
        &config,
        std::slice::from_ref(&inv),
        now + Duration::from_secs(2),
    );
    assert_eq!(publisher.next_deadline(), None);
    publisher.reconcile(&config, &[inv], now + OFFLINE_GRACE * 2);
    assert_eq!(
        publisher.backend.live.values().next().unwrap().percentage,
        77
    );
    assert_eq!(publisher.backend.writes, 1);
}

#[test]
fn filters_receivers_vendor_direct_devices_and_unsupported_kinds() {
    for (vendor, product, kind, expected) in [
        (0x046d, 0xc548, DeviceKind::Mouse, 1),
        (0x046d, 0xc52b, DeviceKind::Keyboard, 1),
        (0x046d, 0xc539, DeviceKind::Mouse, 0),
        (0x046d, 0xb807, DeviceKind::Keyboard, 0),
        (0x1234, 0xc548, DeviceKind::Mouse, 0),
        (0x046d, 0xc548, DeviceKind::Unknown, 0),
    ] {
        let mut inv = inventory();
        inv.receiver.vendor_id = vendor;
        inv.receiver.product_id = product;
        inv.paired[0].kind = kind;
        let mut publisher = PowerSourcePublisher::new(FakeBackend::default());
        publisher.reconcile(&enabled_config(), &[inv], Instant::now());
        assert_eq!(
            publisher.backend.live.len(),
            expected,
            "{vendor:x}:{product:x} {kind:?}"
        );
    }
}

#[test]
fn offline_first_sighting_and_missing_initial_reading_never_publish_zero() {
    for online in [false, true] {
        let mut inv = inventory();
        inv.paired[0].online = online;
        inv.paired[0].battery = None;
        let mut publisher = PowerSourcePublisher::new(FakeBackend::default());
        publisher.reconcile(&enabled_config(), &[inv], Instant::now());
        assert_eq!(publisher.backend.writes, 0);
        assert_eq!(publisher.next_deadline(), None);
    }
}

#[test]
fn per_device_failure_preserves_successful_count_and_recovers_on_next_pass() {
    let backend = FakeBackend {
        fail_write_slot: Some(1),
        ..FakeBackend::default()
    };
    let mut publisher = PowerSourcePublisher::new(backend);
    let mut inv = inventory();
    let mut keyboard = inv.paired[0].clone();
    keyboard.slot = 2;
    keyboard.kind = DeviceKind::Keyboard;
    inv.paired.push(keyboard);
    let config = enabled_config();
    let now = Instant::now();
    publisher.reconcile(&config, std::slice::from_ref(&inv), now);
    assert!(matches!(
        publisher.status(),
        BatteryWidgetStatus::Failed {
            published_devices: 1,
            ..
        }
    ));
    assert_eq!(publisher.backend.writes, 2);
    publisher.backend.fail_write_slot = None;
    publisher.reconcile(&config, std::slice::from_ref(&inv), now);
    assert_eq!(
        publisher.status(),
        BatteryWidgetStatus::Active {
            published_devices: 2
        }
    );
    assert_eq!(publisher.backend.writes, 3);
    publisher.backend.fail_write_slot = Some(1);
    inv.paired[0].battery.as_mut().unwrap().percentage = 50;
    publisher.reconcile(&config, &[inv], now);
    assert!(matches!(
        publisher.status(),
        BatteryWidgetStatus::Failed {
            published_devices: 2,
            ..
        }
    ));
    assert_eq!(
        publisher.backend.live["receiver:test-receiver:slot:1"].percentage,
        77
    );
}

#[test]
fn unavailable_is_visible_without_devices_and_disable_clears_status() {
    let backend = FakeBackend {
        unavailable: true,
        ..FakeBackend::default()
    };
    let mut publisher = PowerSourcePublisher::new(backend);
    publisher.reconcile(&enabled_config(), &[], Instant::now());
    assert_eq!(
        publisher.status(),
        BatteryWidgetStatus::Unavailable {
            reason: "missing test SPI".into()
        }
    );
    publisher.reconcile(&Config::default(), &[], Instant::now());
    assert_eq!(publisher.status(), BatteryWidgetStatus::Disabled);
    assert_eq!(publisher.backend.prepares, 1);
}

#[test]
fn disable_removes_immediately_and_reenable_republishes() {
    let now = Instant::now();
    let mut publisher = PowerSourcePublisher::new(FakeBackend::default());
    publisher.reconcile(&enabled_config(), &[inventory()], now);
    publisher.reconcile(&Config::default(), &[inventory()], now);
    assert_eq!(publisher.status(), BatteryWidgetStatus::Disabled);
    assert!(publisher.backend.live.is_empty());
    assert_eq!(publisher.backend.removals.get(), 1);
    publisher.reconcile(&enabled_config(), &[inventory()], now);
    assert_eq!(
        publisher.status(),
        BatteryWidgetStatus::Active {
            published_devices: 1
        }
    );
    assert_eq!(publisher.backend.writes, 2);
}

#[test]
fn failed_removal_is_reported_but_consumed_handle_is_never_released_twice() {
    let now = Instant::now();
    let config = enabled_config();
    let mut publisher = PowerSourcePublisher::new(FakeBackend::default());
    publisher.reconcile(&config, &[inventory()], now);
    publisher.backend.fail_remove = true;
    publisher.reconcile(&config, &[], now);
    publisher.reconcile(&config, &[], now + OFFLINE_GRACE);
    assert!(matches!(
        publisher.status(),
        BatteryWidgetStatus::Failed {
            published_devices: 0,
            ..
        }
    ));
    assert_eq!(publisher.next_deadline(), None);
    publisher.clear_all();
    assert_eq!(publisher.backend.removals.get(), 1);
    publisher.reconcile(&config, &[inventory()], now + OFFLINE_GRACE);
    assert_eq!(
        publisher.status(),
        BatteryWidgetStatus::Active {
            published_devices: 1
        }
    );
}

#[test]
fn shutdown_cleans_up_a_failed_initial_publication() {
    let backend = FakeBackend {
        fail_write_slot: Some(1),
        ..FakeBackend::default()
    };
    let removals = backend.removals.clone();
    let mut publisher = PowerSourcePublisher::new(backend);
    publisher.reconcile(&enabled_config(), &[inventory()], Instant::now());
    assert!(matches!(
        publisher.status(),
        BatteryWidgetStatus::Failed {
            published_devices: 0,
            ..
        }
    ));
    drop(publisher);
    assert_eq!(removals.get(), 1);
}
