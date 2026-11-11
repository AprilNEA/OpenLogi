//! The dialpad's drawn surface: which parts it shows, where they sit, and
//! that clicking one edits that part's binding.

use super::super::dialpad::{DialpadPart, dial_rect, dialpad_canvas, roller_rect};
use super::*;
use openlogi_core::binding::Rotary;
use openlogi_core::hid::{DialpadDescriptor, DialpadKeyPosition};

/// The MX Creative Dialpad, as its Bluetooth route names it.
fn mx_dialpad() -> &'static DialpadDescriptor {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xbc00,
    }
    .dialpad()
    .expect("the MX Creative Dialpad is registered")
}

fn dialpad_slots() -> Vec<super::super::key_points::KeySlotLayout> {
    key_surface(None, None, Some(mx_dialpad()), None).slots()
}

#[test]
fn the_dialpad_shows_its_keys_and_both_directions_of_each_rotary() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");

    let slots = dialpad_slots();

    let targets: Vec<KeyTarget> = slots.iter().map(|slot| slot.target.clone()).collect();
    assert_eq!(
        targets,
        [
            KeyTarget::Control(ButtonId::control(0x0053)),
            KeyTarget::Control(ButtonId::control(0x0056)),
            KeyTarget::Control(ButtonId::DialLeft),
            KeyTarget::Control(ButtonId::DialRight),
            KeyTarget::Control(ButtonId::RollerUp),
            KeyTarget::Control(ButtonId::RollerDown),
            KeyTarget::Control(ButtonId::control(0x0059)),
            KeyTarget::Control(ButtonId::control(0x005a)),
        ],
        "reading order: top keys, dial, roller, bottom keys"
    );
    let legends: Vec<&str> = slots.iter().map(|slot| slot.legend.as_str()).collect();
    assert_eq!(
        legends,
        [
            "Top Left Key",
            "Top Right Key",
            "Dial Left",
            "Dial Right",
            "Roller Up",
            "Roller Down",
            "Bottom Left Key",
            "Bottom Right Key",
        ],
        "a key is named by its place on the dialpad, never by the mouse \
         button its control ID also names"
    );
}

/// The drawing follows the chassis: the upper keys side by side at the top
/// left, the roller at the top right, the dial below them, the lower keys
/// in the bottom corners beside it — every part inside the canvas and clear
/// of every other and of the dial's ring, each slot's point at its part's
/// centre.
#[test]
fn dialpad_parts_sit_where_the_device_has_them() {
    let dialpad = mx_dialpad();
    let (w, h) = dialpad_canvas();
    let slots = dialpad_slots();
    let parts: Vec<DialpadPart> = slots
        .iter()
        .map(|slot| DialpadPart::of(&slot.target, dialpad).expect("a dialpad part"))
        .collect();

    for (slot, part) in slots.iter().zip(&parts) {
        let rect = part.rect();
        assert!(
            rect.left >= 0. && rect.top >= 0. && rect.left + rect.w <= w && rect.top + rect.h <= h,
            "{part:?} leaves the canvas"
        );
        let (x, y) = rect.center();
        assert_approx_eq(slot.point.x_frac * w, x);
        assert_approx_eq(slot.point.y_frac * h, y);
    }
    for (i, a) in parts.iter().enumerate() {
        for b in &parts[i + 1..] {
            assert!(!a.rect().overlaps(b.rect()), "{a:?} overlaps {b:?}");
        }
    }

    let rect = |part: DialpadPart| part.rect();
    let key = |position| rect(DialpadPart::Key(position));
    let dial_left = rect(DialpadPart::DialLeft);
    let dial_right = rect(DialpadPart::DialRight);
    let roller_up = rect(DialpadPart::RollerUp);
    let roller_down = rect(DialpadPart::RollerDown);
    let top_left = key(DialpadKeyPosition::TopLeft);
    let top_right = key(DialpadKeyPosition::TopRight);
    let bottom_left = key(DialpadKeyPosition::BottomLeft);
    let bottom_right = key(DialpadKeyPosition::BottomRight);

    let dial = dial_rect();
    let roller = roller_rect();

    assert!(top_left.left < top_right.left && bottom_left.left < bottom_right.left);
    assert!(
        top_right.left + top_right.w <= roller.left,
        "the roller right of the upper keys"
    );
    for upper in [top_left, top_right, roller] {
        assert!(upper.top + upper.h <= dial.top, "{upper:?} above the dial");
    }
    for lower in [bottom_left, bottom_right] {
        assert!(
            lower.center().1 > dial.center().1,
            "{lower:?} below the dial's centre"
        );
        // Clear of the ring: the key's nearest point lies outside the circle.
        let (cx, cy) = dial.center();
        let nx = cx.clamp(lower.left, lower.left + lower.w);
        let ny = cy.clamp(lower.top, lower.top + lower.h);
        assert!(
            (nx - cx).hypot(ny - cy) >= dial.w / 2.,
            "{lower:?} cuts into the dial"
        );
    }
    assert!(
        bottom_left.left < dial.left && bottom_right.left + bottom_right.w > dial.left + dial.w
    );
    assert!(
        dial_left.left + dial_left.w <= dial_right.left,
        "counter-clockwise on the left"
    );
    assert!(
        roller_up.top + roller_up.h <= roller_down.top,
        "up above down"
    );
    for half in [roller_up, roller_down] {
        assert!(half.left >= roller.left && half.left + half.w <= roller.left + roller.w);
        assert!(half.top >= roller.top && half.top + half.h <= roller.top + roller.h);
    }
}

/// A dialpad render OpenLogi did not measure is drawn from the model,
/// whatever its depot marks: no marker names a turning direction.
#[test]
fn a_dialpad_is_drawn_even_when_its_depot_marks_its_keys() {
    let asset = asset_with_controls(&[(0x0053, 20., 20.), (0x0056, 80., 20.)]);

    let surface = key_surface(Some(&asset), None, Some(mx_dialpad()), None);

    let KeySurface::Dialpad(dialpad, None, slots) = surface else {
        panic!("a dialpad without a measured render gets its drawn surface");
    };
    assert_eq!(dialpad.name, "MX Creative Dialpad");
    assert_eq!(slots.len(), 8);
}

/// The dialpad's depot as Logi ships it: the front render.
fn dialpad_asset(png: (u32, u32)) -> ResolvedAsset {
    ResolvedAsset {
        depot: "mx_creative_dialpad".to_string(),
        png_width: png.0,
        png_height: png.1,
        ..asset_with_controls(&[])
    }
}

/// On the render OpenLogi measured, the picture is the selector: every part
/// lands where it is on that render, the dial's halves split the dial at its
/// centre, the roller's halves split the roller, and no part covers another.
#[test]
fn the_dialpad_render_puts_every_part_on_the_photo() {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");

    let surface = key_surface(
        Some(&dialpad_asset((1124, 1105))),
        None,
        Some(mx_dialpad()),
        None,
    );

    let KeySurface::Dialpad(dialpad, Some(render), slots) = surface else {
        panic!("the measured render carries the parts");
    };
    let drawn: Vec<KeyTarget> = dialpad_slots()
        .into_iter()
        .map(|slot| slot.target)
        .collect();
    assert_eq!(
        slots
            .iter()
            .map(|slot| slot.target.clone())
            .collect::<Vec<_>>(),
        drawn,
        "the same parts in the same order as the drawing"
    );
    let (w, h) = render.size();
    assert_eq!((w, h), (1124., 1105.));
    let centre = |part: DialpadPart| {
        let slot = slots
            .iter()
            .find(|slot| DialpadPart::of(&slot.target, dialpad) == Some(part))
            .expect("every part has a slot");
        (slot.point.x_frac * w, slot.point.y_frac * h)
    };
    let near = |(x, y): (f32, f32), (ex, ey): (f32, f32)| {
        assert!(
            (x - ex).abs() < 0.01 && (y - ey).abs() < 0.01,
            "{x},{y} vs {ex},{ey}"
        );
    };
    near(
        centre(DialpadPart::Key(DialpadKeyPosition::TopLeft)),
        (172., 172.),
    );
    near(
        centre(DialpadPart::Key(DialpadKeyPosition::TopRight)),
        (372., 172.),
    );
    near(
        centre(DialpadPart::Key(DialpadKeyPosition::BottomLeft)),
        (172., 930.),
    );
    near(
        centre(DialpadPart::Key(DialpadKeyPosition::BottomRight)),
        (952., 930.),
    );
    near(centre(DialpadPart::DialLeft), (562. - 147.5, 627.));
    near(centre(DialpadPart::DialRight), (562. + 147.5, 627.));
    near(centre(DialpadPart::RollerUp), (851., 173. - 37.5));
    near(centre(DialpadPart::RollerDown), (851., 173. + 37.5));

    let parts: Vec<DialpadPart> = slots
        .iter()
        .filter_map(|slot| DialpadPart::of(&slot.target, dialpad))
        .collect();
    for (i, a) in parts.iter().enumerate() {
        let rect = render.rect(*a);
        assert!(
            rect.left >= 0. && rect.top >= 0. && rect.left + rect.w <= w && rect.top + rect.h <= h,
            "{a:?} leaves the render"
        );
        for b in &parts[i + 1..] {
            assert!(!rect.overlaps(render.rect(*b)), "{a:?} overlaps {b:?}");
        }
    }
    let dial = render.dial_rect();
    let (left, right) = (
        render.rect(DialpadPart::DialLeft),
        render.rect(DialpadPart::DialRight),
    );
    assert_approx_eq(left.w + right.w, dial.w);
    assert_approx_eq(left.left + left.w, right.left);
}

/// A re-staged dialpad render is not the one measured: the drawing stands
/// in rather than landing the parts off the device.
#[test]
fn a_restaged_dialpad_render_falls_back_to_the_drawing() {
    let surface = key_surface(
        Some(&dialpad_asset((1200, 1180))),
        None,
        Some(mx_dialpad()),
        None,
    );

    assert!(matches!(surface, KeySurface::Dialpad(_, None, _)));
}

/// Only the dialpad's own controls are its parts; each turning direction
/// is the part on its side.
#[test]
fn a_target_names_a_dialpad_part_only_when_it_is_one() {
    let dialpad = mx_dialpad();
    let part = |target: KeyTarget| DialpadPart::of(&target, dialpad);

    assert_eq!(
        part(KeyTarget::Control(Rotary::Dial.negative())),
        Some(DialpadPart::DialLeft)
    );
    assert_eq!(
        part(KeyTarget::Control(Rotary::Dial.positive())),
        Some(DialpadPart::DialRight)
    );
    assert_eq!(
        part(KeyTarget::Control(Rotary::Roller.positive())),
        Some(DialpadPart::RollerUp)
    );
    assert_eq!(
        part(KeyTarget::Control(Rotary::Roller.negative())),
        Some(DialpadPart::RollerDown)
    );
    assert_eq!(
        part(KeyTarget::Control(ButtonId::control(0x005a))),
        Some(DialpadPart::Key(DialpadKeyPosition::BottomRight))
    );
    assert_eq!(part(KeyTarget::Control(ButtonId::control(0x01a1))), None);
    assert_eq!(part(KeyTarget::grid(GridKey::Middle)), None);
}

/// Clicking a dial half selects that turning direction, and the panel's
/// pick binds it on the dialpad like any of its keys.
#[gpui::test]
fn clicking_the_dial_selects_its_direction_for_binding(cx: &mut gpui::TestAppContext) {
    use gpui::Modifiers;

    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let (state, _commands) = crate::state::tests::dialpad::dialpad_state();
    init_ui(cx);
    let (view, cx) = open_keys_view(cx, state);

    // Slot 3 is the dial turned clockwise.
    let bounds = cx
        .debug_bounds("dialpad-part-3")
        .expect("the dial's right half");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    view.read_with(cx, |view, _| assert_eq!(view.selected_key, Some(3)));

    let clockwise = KeyTarget::Control(ButtonId::DialRight);
    cx.update(|_, cx| {
        AppState::apply(cx, |state| {
            commit_key_action(state, &clockwise, Some(Action::VolumeUp))
        });
    });
    cx.update(|_, cx| {
        let state = AppState::try_read(cx).unwrap();
        assert_eq!(bound_action(state, &clockwise), Some(Action::VolumeUp));
        assert_eq!(
            bound_action(state, &KeyTarget::Control(ButtonId::DialLeft)),
            None,
            "each direction binds on its own"
        );
    });

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

/// Set up the component library, once per test.
fn init_ui(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
}

/// The Keys view over `state`, drawn in a window wide enough for its panel.
fn open_keys_view(
    cx: &mut gpui::TestAppContext,
    state: AppState,
) -> (Entity<FunctionRowView>, &mut gpui::VisualTestContext) {
    cx.update(|cx| {
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
    });
    let (view, cx) = cx.add_window_view(|_, cx| FunctionRowView::new(cx));
    cx.simulate_resize(gpui::size(px(1200.), px(900.)));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    (view, cx)
}

/// The dialpad shows its rotary speed above it; the keypad, which has no
/// rotary control, does not.
#[gpui::test]
fn only_the_dialpad_shows_its_rotary_speed(cx: &mut gpui::TestAppContext) {
    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");

    init_ui(cx);
    let (state, _commands) = crate::state::tests::dialpad::dialpad_state();
    let (view, window) = open_keys_view(cx, state);
    assert!(window.debug_bounds("dial-speed").is_some());
    drop(view);
    window.update(|window, _| window.remove_window());

    let (state, _commands) = crate::state::tests::keypad_pages::keypad_state();
    let (view, window) = open_keys_view(cx, state);
    assert!(window.debug_bounds("dial-speed").is_none());
    drop(view);
    window.update(|window, _| window.remove_window());
    cx.run_until_parked();
}
