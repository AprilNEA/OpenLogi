//! Low-battery notices: which inventory refreshes post one, and what it says.

use std::sync::Arc;

use openlogi_core::config::Config;
use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, DeviceKind};
use openlogi_ipc::{Notice, NoticeKind};

use super::direct_inventory;
use crate::observable::ObservableState;
use crate::orchestrator::Orchestrator;

/// The key `direct_inventory(Some("ABC123"), ..)` resolves to.
const KEY: &str = "serial:abc123";

fn discharging(percentage: u8) -> BatteryInfo {
    BatteryInfo {
        percentage: Some(percentage),
        level: BatteryLevel::Good,
        status: BatteryStatus::Discharging,
    }
}

fn refresh(orch: &mut Orchestrator, battery: BatteryInfo, online: bool, codename: &str) {
    let mut inventory = direct_inventory(Some("ABC123"), [1, 2, 3, 4]);
    let paired = &mut inventory.paired[0];
    paired.battery = Some(battery);
    paired.online = online;
    paired.codename = Some(codename.to_owned());
    orch.refresh_inventory(&[inventory], &[], false);
}

fn wired(config: Config) -> (Orchestrator, Arc<ObservableState>) {
    let observable = Arc::new(ObservableState::new("test".to_owned()));
    (
        Orchestrator::new(config, Arc::clone(&observable)),
        observable,
    )
}

async fn notices(observable: &ObservableState) -> Vec<Notice> {
    observable.notices().observe(0).await.notices
}

#[tokio::test]
async fn a_battery_that_runs_low_posts_one_notice() {
    let mut config = Config::default();
    config.app_settings.language = Some("fr".to_owned());
    let (mut orch, observable) = wired(config);

    refresh(&mut orch, discharging(40), true, "MX Master 3S");
    assert!(notices(&observable).await.is_empty());

    refresh(&mut orch, discharging(18), true, "MX Master 3S");
    let posted = notices(&observable).await;
    assert_eq!(posted.len(), 1);
    assert_eq!(
        posted[0].kind,
        NoticeKind::LowBattery {
            name: Some("MX Master 3S".to_owned()),
            kind: DeviceKind::Mouse,
            battery: discharging(18),
        }
    );
    assert_eq!(
        observable.notices().observe(0).await.language.as_deref(),
        Some("fr")
    );

    refresh(&mut orch, discharging(12), true, "MX Master 3S");
    let still = notices(&observable).await;
    assert_eq!(still.len(), 1, "a battery still low is not announced again");
    assert_eq!(still[0].id, posted[0].id);
}

#[tokio::test]
async fn the_notice_names_the_device_as_the_user_does() {
    let mut config = Config::default();
    config.set_device_custom_name(KEY, Some("Work mouse".to_owned()));
    let (mut orch, observable) = wired(config);
    refresh(&mut orch, discharging(10), true, "MX Master 3S");
    let posted = notices(&observable).await;
    assert!(
        matches!(&posted[0].kind, NoticeKind::LowBattery { name: Some(name), .. } if name == "Work mouse"),
        "{posted:?}"
    );
}

#[tokio::test]
async fn a_shouted_device_name_is_tidied() {
    let (mut orch, observable) = wired(Config::default());
    refresh(&mut orch, discharging(10), true, "G502 X PLUS");
    let posted = notices(&observable).await;
    assert!(
        matches!(&posted[0].kind, NoticeKind::LowBattery { name: Some(name), .. } if name == "G502 X Plus"),
        "{posted:?}"
    );
}

#[tokio::test]
async fn an_offline_device_s_last_known_battery_posts_nothing() {
    let (mut orch, observable) = wired(Config::default());
    refresh(&mut orch, discharging(10), false, "MX Master 3S");
    assert!(notices(&observable).await.is_empty());

    refresh(&mut orch, discharging(10), true, "MX Master 3S");
    assert_eq!(
        notices(&observable).await.len(),
        1,
        "announced once it is online"
    );
}

#[tokio::test]
async fn no_notice_is_posted_while_the_user_turned_them_off() {
    let mut config = Config::default();
    config.app_settings.low_battery_notifications = false;
    let (mut orch, observable) = wired(config);
    refresh(&mut orch, discharging(10), true, "MX Master 3S");
    assert!(notices(&observable).await.is_empty());

    orch.config.app_settings.low_battery_notifications = true;
    refresh(&mut orch, discharging(10), true, "MX Master 3S");
    assert_eq!(
        notices(&observable).await.len(),
        1,
        "a battery that ran low while they were off is announced once they are on"
    );
}

#[tokio::test]
async fn a_recharge_while_the_notices_are_off_is_still_taken_in() {
    let (mut orch, observable) = wired(Config::default());
    refresh(&mut orch, discharging(10), true, "MX Master 3S");
    let first = notices(&observable).await;
    assert_eq!(first.len(), 1);

    orch.config.app_settings.low_battery_notifications = false;
    refresh(&mut orch, discharging(80), true, "MX Master 3S");
    refresh(&mut orch, discharging(10), true, "MX Master 3S");
    assert_eq!(
        notices(&observable).await,
        first,
        "nothing is posted while off"
    );

    orch.config.app_settings.low_battery_notifications = true;
    refresh(&mut orch, discharging(10), true, "MX Master 3S");
    let posted = notices(&observable).await;
    assert_eq!(
        posted.len(),
        2,
        "the discharge after the recharge is announced once they are on"
    );
    assert_ne!(posted[1].id, first[0].id);
}
