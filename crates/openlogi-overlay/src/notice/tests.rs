use gpui::{Bounds, point, px, size};
use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, DeviceKind};
use openlogi_ipc::{Notice, NoticeKind};

use super::*;

fn low_battery(id: u64, name: Option<&str>, percentage: Option<u8>) -> Notice {
    Notice {
        id,
        kind: NoticeKind::LowBattery {
            name: name.map(str::to_owned),
            kind: DeviceKind::Mouse,
            battery: BatteryInfo {
                percentage,
                level: BatteryLevel::Low,
                status: BatteryStatus::Discharging,
            },
        },
    }
}

fn ids(cards: &[Notice]) -> Vec<u64> {
    cards.iter().map(|notice| notice.id).collect()
}

#[test]
fn the_first_list_is_shown_and_an_unchanged_one_is_not_redrawn() {
    let mut shelf = NoticeShelf::default();
    let list = vec![low_battery(1, Some("MX Master 3S"), Some(15))];
    assert_eq!(
        shelf.observe(list.clone()).as_deref().map(ids),
        Some(vec![1])
    );
    assert_eq!(
        shelf.observe(list),
        None,
        "a repeat of what is on screen must not reopen the window"
    );
}

#[test]
fn the_last_notice_ending_clears_the_screen() {
    let mut shelf = NoticeShelf::default();
    shelf.observe(vec![low_battery(1, None, Some(15))]);
    assert_eq!(shelf.observe(Vec::new()), Some(Vec::new()));
    assert_eq!(shelf.observe(Vec::new()), None);
}

#[test]
fn a_dismissed_card_stays_away_while_the_agent_still_lists_it() {
    let mut shelf = NoticeShelf::default();
    shelf.observe(vec![
        low_battery(1, None, Some(15)),
        low_battery(2, None, Some(9)),
    ]);
    assert_eq!(ids(&shelf.dismiss(1)), vec![2]);
    // The agent's next list still carries notice 1 until its time is up.
    assert_eq!(
        shelf.observe(vec![
            low_battery(1, None, Some(15)),
            low_battery(2, None, Some(9))
        ]),
        None,
        "the dismissed card must not come back"
    );
    assert_eq!(ids(&shelf.dismiss(2)), Vec::<u64>::new());
}

#[test]
fn a_dismissal_is_forgotten_once_the_agent_drops_the_notice() {
    let mut shelf = NoticeShelf::default();
    shelf.observe(vec![low_battery(1, None, Some(15))]);
    shelf.dismiss(1);
    shelf.observe(Vec::new());
    // Ids are unique within an agent run; a replacement agent numbers its own
    // notices from the start, so a remembered id would hide a new notice.
    assert_eq!(
        shelf
            .observe(vec![low_battery(1, None, Some(12))])
            .as_deref()
            .map(ids),
        Some(vec![1])
    );
}

#[test]
fn only_the_newest_cards_show_when_too_many_are_current() {
    let mut shelf = NoticeShelf::default();
    let list = (1..=5).map(|id| low_battery(id, None, Some(10))).collect();
    assert_eq!(
        shelf.observe(list).as_deref().map(ids),
        Some(vec![3, 4, 5]),
        "oldest first, capped at MAX_CARDS"
    );
}

#[test]
fn a_dismissed_card_makes_room_for_one_that_was_waiting() {
    let mut shelf = NoticeShelf::default();
    let list: Vec<Notice> = (1..=4).map(|id| low_battery(id, None, Some(10))).collect();
    assert_eq!(
        shelf.observe(list.clone()).as_deref().map(ids),
        Some(vec![2, 3, 4])
    );
    // A click brings no new list from the agent, so the dismissal itself must
    // reveal the notice that was waiting.
    assert_eq!(ids(&shelf.dismiss(3)), vec![1, 2, 4]);
    assert_eq!(
        shelf.observe(list),
        None,
        "the agent repeating its list must not redraw the revealed card"
    );
    assert_eq!(ids(&shelf.dismiss(4)), vec![1, 2]);
}

#[test]
fn the_window_fits_its_cards_and_their_shadow() {
    assert_eq!(stack_size(1), size(px(344.0), px(88.0)));
    assert_eq!(stack_size(3), size(px(344.0), px(232.0)));
    assert_eq!(
        stack_size(7),
        stack_size(MAX_CARDS),
        "more notices than cards never grow the window"
    );
    assert_eq!(
        stack_size(0),
        stack_size(1),
        "a window is never sized for less than one card"
    );
}

#[test]
fn the_stack_sits_centred_above_the_bottom_edge() {
    let area = Bounds::new(point(px(0.0), px(37.0)), size(px(1512.0), px(875.0)));
    let origin = bottom_centre(area, stack_size(1));
    // (1512 - 344) / 2 = 584; 37 + 875 - 88 - 48 = 776.
    assert_eq!(origin, point(px(584.0), px(776.0)));
}

#[test]
fn the_stack_stays_inside_an_area_too_small_for_it() {
    let area = Bounds::new(point(px(100.0), px(50.0)), size(px(200.0), px(60.0)));
    assert_eq!(
        bottom_centre(area, stack_size(1)),
        point(px(100.0), px(50.0))
    );
}

#[test]
fn a_low_battery_card_names_the_device_and_its_charge() {
    let notice = low_battery(1, Some("MX Master 3S"), Some(15));
    assert_eq!(
        notice_text(&notice.kind),
        NoticeText {
            title: "Low battery".into(),
            detail: "MX Master 3S · 15%".into(),
        }
    );
}

#[test]
fn a_nameless_device_is_named_by_its_kind_and_a_level_only_reading_shows_no_percentage() {
    let notice = low_battery(1, None, None);
    assert_eq!(
        notice_text(&notice.kind).detail,
        SharedString::from("Mouse")
    );
}
