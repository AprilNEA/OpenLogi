use std::time::Duration;

use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, DeviceKind};
use openlogi_ipc::{NoticeKind, OBSERVE_HOLD};

use super::{LowBatteryWatch, NOTICE_LIFETIME, NoticeBoard};

fn reading(percentage: Option<u8>, level: BatteryLevel, status: BatteryStatus) -> BatteryInfo {
    BatteryInfo {
        percentage,
        level,
        status,
    }
}

fn discharging(percentage: u8) -> BatteryInfo {
    reading(
        Some(percentage),
        BatteryLevel::Good,
        BatteryStatus::Discharging,
    )
}

fn low_battery(name: &str) -> NoticeKind {
    NoticeKind::LowBattery {
        name: Some(name.to_owned()),
        kind: DeviceKind::Mouse,
        battery: discharging(15),
    }
}

#[test]
fn a_battery_is_announced_once_when_it_first_runs_low() {
    let mut watch = LowBatteryWatch::default();
    assert!(!watch.observe("mouse", &discharging(40)));
    assert!(watch.observe("mouse", &discharging(20)));
    assert!(!watch.observe("mouse", &discharging(18)));
    assert!(!watch.observe("mouse", &discharging(5)));
}

#[test]
fn a_battery_already_low_when_first_seen_is_announced() {
    let mut watch = LowBatteryWatch::default();
    assert!(watch.observe("mouse", &discharging(9)));
}

#[test]
fn a_reading_wobbling_around_the_threshold_is_not_a_new_discharge() {
    let mut watch = LowBatteryWatch::default();
    assert!(watch.observe("mouse", &discharging(20)));
    assert!(!watch.observe("mouse", &discharging(21)));
    assert!(!watch.observe("mouse", &discharging(29)));
    assert!(!watch.observe("mouse", &discharging(20)));
}

#[test]
fn a_charged_battery_is_announced_again_on_its_next_discharge() {
    for charged in [
        reading(Some(12), BatteryLevel::Low, BatteryStatus::Charging),
        reading(Some(12), BatteryLevel::Low, BatteryStatus::ChargingSlow),
        reading(None, BatteryLevel::Full, BatteryStatus::Full),
        discharging(30),
        reading(None, BatteryLevel::Good, BatteryStatus::Discharging),
        reading(None, BatteryLevel::Full, BatteryStatus::Discharging),
    ] {
        let mut watch = LowBatteryWatch::default();
        assert!(watch.observe("mouse", &discharging(15)));
        assert!(!watch.observe("mouse", &charged), "{charged:?}");
        assert!(
            watch.observe("mouse", &discharging(15)),
            "after {charged:?}"
        );
    }
}

#[test]
fn a_reading_that_shows_no_charge_keeps_the_announcement() {
    for unclear in [
        reading(None, BatteryLevel::Unknown, BatteryStatus::Discharging),
        reading(None, BatteryLevel::Unknown, BatteryStatus::Unknown),
        reading(Some(25), BatteryLevel::Good, BatteryStatus::Error),
    ] {
        let mut watch = LowBatteryWatch::default();
        assert!(watch.observe("mouse", &discharging(15)));
        assert!(!watch.observe("mouse", &unclear), "{unclear:?}");
        assert!(
            !watch.observe("mouse", &discharging(15)),
            "after {unclear:?}"
        );
    }
}

#[test]
fn a_level_only_battery_is_announced_by_its_level() {
    let mut watch = LowBatteryWatch::default();
    let level = |level| reading(None, level, BatteryStatus::Discharging);
    assert!(!watch.observe("keyboard", &level(BatteryLevel::Good)));
    assert!(watch.observe("keyboard", &level(BatteryLevel::Low)));
    assert!(!watch.observe("keyboard", &level(BatteryLevel::Critical)));
}

#[test]
fn each_device_is_announced_on_its_own() {
    let mut watch = LowBatteryWatch::default();
    assert!(watch.observe("mouse", &discharging(15)));
    assert!(watch.observe("keyboard", &discharging(15)));
    assert!(!watch.observe("keyboard", &discharging(30)));
    assert!(!watch.observe("mouse", &discharging(14)));
    assert!(watch.observe("keyboard", &discharging(15)));
}

#[test]
fn a_recharge_while_muted_still_ends_the_discharge() {
    let mut watch = LowBatteryWatch::default();
    assert!(watch.observe("mouse", &discharging(15)));
    // The user turns the notices off; the mouse charges, then runs low again.
    watch.observe_muted("mouse", &discharging(80));
    watch.observe_muted("mouse", &discharging(15));
    assert!(
        watch.observe("mouse", &discharging(15)),
        "turned back on, the new discharge is announced"
    );
}

#[test]
fn a_low_reading_while_muted_is_not_counted_as_announced() {
    let mut watch = LowBatteryWatch::default();
    watch.observe_muted("mouse", &discharging(15));
    assert!(watch.observe("mouse", &discharging(15)));
    assert!(!watch.observe("mouse", &discharging(14)));
}

#[tokio::test(start_paused = true)]
async fn a_posted_notice_is_observed_with_its_locale() {
    let board = NoticeBoard::default();
    let empty = board.observe(0).await;
    assert!(empty.notices.is_empty());

    board.post(low_battery("MX"), Some("de".to_owned()));
    let observed = board.observe(empty.generation).await;
    assert_eq!(observed.notices.len(), 1);
    assert_eq!(observed.notices[0].kind, low_battery("MX"));
    assert_eq!(observed.language.as_deref(), Some("de"));
    assert!(observed.generation > empty.generation);
}

#[tokio::test(start_paused = true)]
async fn a_long_poll_wakes_when_a_notice_is_posted() {
    let board = std::sync::Arc::new(NoticeBoard::default());
    let since = board.observe(0).await.generation;
    let started = tokio::time::Instant::now();
    let waiting = tokio::spawn({
        let board = std::sync::Arc::clone(&board);
        async move { board.observe(since).await }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    board.post(low_battery("MX"), None);
    let observed = waiting.await.expect("observer task");
    assert_eq!(observed.notices.len(), 1);
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(1),
        "the post answered the poll, not the hold"
    );
}

#[tokio::test(start_paused = true)]
async fn a_notice_ends_when_its_time_is_up_and_wakes_the_poll() {
    let board = NoticeBoard::default();
    board.post(low_battery("MX"), None);
    let shown = board.observe(0).await;
    assert_eq!(shown.notices.len(), 1);

    let started = tokio::time::Instant::now();
    let ended = board.observe(shown.generation).await;
    assert!(ended.notices.is_empty());
    assert!(ended.generation > shown.generation);
    assert_eq!(started.elapsed(), NOTICE_LIFETIME);
}

#[tokio::test(start_paused = true)]
async fn an_overlay_that_connects_late_sees_no_ended_notice() {
    let board = NoticeBoard::default();
    board.post(low_battery("MX"), None);
    tokio::time::advance(NOTICE_LIFETIME).await;
    assert!(board.observe(0).await.notices.is_empty());
}

#[tokio::test(start_paused = true)]
async fn notices_end_one_by_one_oldest_first() {
    let board = NoticeBoard::default();
    board.post(low_battery("first"), Some("de".to_owned()));
    tokio::time::advance(Duration::from_secs(3)).await;
    board.post(low_battery("second"), Some("de".to_owned()));
    let both = board.observe(0).await;
    assert_eq!(both.notices.len(), 2);
    assert_eq!(both.notices[0].kind, low_battery("first"));
    assert!(both.notices[0].id < both.notices[1].id);

    let one = board.observe(both.generation).await;
    assert_eq!(one.notices.len(), 1);
    assert_eq!(one.notices[0].kind, low_battery("second"));
    assert_eq!(one.notices[0].id, both.notices[1].id);
    assert_eq!(
        one.language.as_deref(),
        Some("de"),
        "an ending notice keeps the locale the rest were posted in"
    );
}

#[tokio::test(start_paused = true)]
async fn a_new_overlay_is_answered_at_once() {
    // An overlay that starts or reconnects asks from generation 0, and must
    // learn at once what is showing instead of waiting out the hold.
    let board = NoticeBoard::default();
    let started = tokio::time::Instant::now();
    let first = board.observe(0).await;
    assert!(first.notices.is_empty());
    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn an_unobserved_board_keeps_only_current_notices() {
    // With no overlay running, nothing reads the board: only a post can drop
    // what has ended.
    let board = NoticeBoard::default();
    board.post(low_battery("first"), None);
    tokio::time::advance(NOTICE_LIFETIME).await;
    board.post(low_battery("second"), None);
    let published = board.published.borrow();
    assert_eq!(published.notices.len(), 1);
    assert_eq!(published.notices[0].kind, low_battery("second"));
}

#[tokio::test(start_paused = true)]
async fn ending_drops_the_notices_whose_time_is_up_and_names_the_next_end() {
    let board = NoticeBoard::default();
    let first_posted = tokio::time::Instant::now();
    board.post(low_battery("first"), None);
    tokio::time::advance(Duration::from_secs(3)).await;
    board.post(low_battery("second"), None);
    let second_ends = first_posted + Duration::from_secs(3) + NOTICE_LIFETIME;

    assert_eq!(
        board.end_due(first_posted + NOTICE_LIFETIME),
        Some(second_ends)
    );
    let published = board.published.borrow();
    assert_eq!(published.notices.len(), 1);
    assert_eq!(published.notices[0].kind, low_battery("second"));
}

#[tokio::test(start_paused = true)]
async fn an_unchanged_board_answers_after_the_hold() {
    let board = NoticeBoard::default();
    let since = board.observe(0).await.generation;
    let started = tokio::time::Instant::now();
    let unchanged = board.observe(since).await;
    assert_eq!(unchanged.generation, since);
    assert_eq!(started.elapsed(), OBSERVE_HOLD);
}
