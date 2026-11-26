use std::sync::Arc;

use gpui::{
    Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, Window, div, px,
    size,
};
use openlogi_core::binding::FlashWrite;
use openlogi_core::hid::{
    DisabledProfile, FlashProfiles, OnboardMode, OnboardProfile, OnboardState, ProfileIndex,
};

use super::{
    FlashProfilesFacts, NAME_UNITS, NameCheck, flash_profiles_card, name_check, profile_name,
};
use crate::services::i18n::LOCALE_LOCK;
use crate::state::{DeviceKey, Load};
use crate::ui::theme;

fn index(number: u8) -> ProfileIndex {
    ProfileIndex::new(number).expect("a profile number is not 0")
}

fn slot(number: u8, name: Option<&str>, enabled: bool) -> OnboardProfile {
    OnboardProfile {
        index: index(number),
        name: name.map(str::to_owned),
        enabled,
    }
}

/// The user's mouse as stage 1 dumped it: three listed profiles, two of
/// them with names whose high bytes were lost, and a former profile on
/// page 5.
fn profiles(write: FlashWrite) -> FlashProfiles {
    FlashProfiles {
        onboard: OnboardState {
            mode: OnboardMode::Onboard,
            active_profile: Some(index(1)),
            profiles: vec![
                slot(1, Some("\u{1f}@>D8;L 1"), true),
                slot(2, Some("Профиль 2"), true),
                slot(3, Some("\u{1f}@>D8;L 3"), true),
                slot(4, None, false),
                slot(5, None, false),
            ],
        },
        active: None,
        disabled: vec![DisabledProfile {
            page: 5,
            name: Some("Профиль 4".to_owned()),
        }],
        can_create: true,
        write,
        pages: vec![2, 3, 4],
    }
}

struct CardHost {
    profiles: FlashProfiles,
    reachable: bool,
}

impl Render for CardHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facts = FlashProfilesFacts {
            key: DeviceKey::from("receiver:046d:c531:slot:1"),
            load: Load::Ready(Arc::new(self.profiles.clone())),
            reachable: self.reachable,
            refusal: None,
        };
        div()
            .w(px(360.))
            .children(flash_profiles_card(Some(facts), theme::palette(cx)))
    }
}

fn draw(
    cx: &mut TestAppContext,
    profiles: FlashProfiles,
    reachable: bool,
) -> &mut gpui::VisualTestContext {
    cx.update(gpui_kit::component::init);
    let (_view, cx) = cx.add_window_view(|_, _| CardHost {
        profiles,
        reachable,
    });
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx
}

#[gpui::test]
fn every_listed_profile_and_the_unlisted_one_get_a_row_inside_the_card(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("ru");
    let cx = draw(cx, profiles(FlashWrite::Idle), true);

    for row in [
        "flash-profile-1",
        "flash-profile-2",
        "flash-profile-3",
        "flash-unlisted-0",
    ] {
        let bounds = cx
            .debug_bounds(row)
            .unwrap_or_else(|| panic!("{row} renders"));
        assert!(
            bounds.size.width <= px(360.),
            "{row} {bounds:?} must fit its card"
        );
        // A name squeezed by its buttons wraps into a tower of lines.
        assert!(
            bounds.size.height <= px(200.),
            "{row} {bounds:?} keeps its name on a line or two"
        );
    }
    for button in [
        "flash-profile-1-rename",
        "flash-profile-1-copy",
        "flash-profile-1-disable",
        "flash-unlisted-0-enable",
    ] {
        let row = cx
            .debug_bounds(&button[..button.rfind('-').expect("a button suffix")])
            .expect("row");
        let bounds = cx
            .debug_bounds(button)
            .unwrap_or_else(|| panic!("{button} renders"));
        assert!(
            bounds.left() >= row.left() && bounds.right() <= row.right(),
            "{button} {bounds:?} must sit inside its row {row:?}"
        );
    }
    assert!(cx.debug_bounds("flash-profiles-write").is_none());
}

#[gpui::test]
fn a_running_write_is_shown(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let cx = draw(cx, profiles(FlashWrite::Writing), true);

    assert!(cx.debug_bounds("flash-profiles-write").is_some());
}

#[test]
fn editing_waits_for_a_running_write_and_a_reachable_mouse() {
    assert!(super::editable(&profiles(FlashWrite::Idle), true));
    assert!(super::editable(
        &profiles(FlashWrite::Failed("no".into())),
        true
    ));
    assert!(!super::editable(&profiles(FlashWrite::Pending), true));
    assert!(!super::editable(&profiles(FlashWrite::Writing), true));
    assert!(!super::editable(&profiles(FlashWrite::Idle), false));
}

#[test]
fn a_name_with_lost_high_bytes_is_numbered_and_damaged() {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    // "Профиль 3" with each UTF-16 unit cut to its low byte, as a real G700
    // stored it.
    let shown = profile_name(index(3), Some("\u{1f}@>D8;L 3"));
    assert!(shown.damaged);
    assert_eq!(shown.text.as_ref(), "Profile 3");

    let shown = profile_name(index(2), Some(" Профиль 2 "));
    assert!(!shown.damaged);
    assert_eq!(shown.text.as_ref(), "Профиль 2");
}

#[test]
fn a_profile_without_a_name_is_numbered() {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    assert_eq!(profile_name(index(4), None).text.as_ref(), "Profile 4");
    assert_eq!(
        profile_name(index(4), Some("  ")).text.as_ref(),
        "Profile 4"
    );
}

#[test]
fn a_name_is_measured_in_utf16_units() {
    assert_eq!(name_check("  "), NameCheck::Empty);
    assert_eq!(name_check("Профиль 2"), NameCheck::Fits { used: 9 });
    let exact = "a".repeat(NAME_UNITS);
    assert_eq!(name_check(&exact), NameCheck::Fits { used: NAME_UNITS });
    // An emoji takes two units, so 22 letters and one emoji are one too many.
    let over = format!("{}🙂", "a".repeat(NAME_UNITS - 1));
    assert_eq!(
        name_check(&over),
        NameCheck::TooLong {
            used: NAME_UNITS + 1
        }
    );
}
