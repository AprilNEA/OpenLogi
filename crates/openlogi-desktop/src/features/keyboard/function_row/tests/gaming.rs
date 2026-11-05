//! The G913 / G915 render's measured keys and the gaming cluster on it.

use super::*;
use crate::state::GamingKeys;
use openlogi_core::binding::{GKey, GKeySlot, ModeKey};

fn g913_asset(depot: &str, display_name: &str) -> ResolvedAsset {
    ResolvedAsset {
        depot: depot.to_string(),
        display_name: display_name.to_string(),
        kind: Some(DeviceKind::Keyboard),
        image_path: PathBuf::from("/tmp/g913.png"),
        hero_image_path: None,
        glow: None,
        metadata: Metadata { images: Vec::new() },
        png_width: 3600,
        png_height: 1125,
        side_view: None,
    }
}

fn run_by_openlogi(mode: GamingKeyMode) -> GamingKeys {
    GamingKeys {
        m_keys: true,
        macro_record: true,
        software_control: true,
        mode,
    }
}

fn targets(gaming: Option<GamingKeys>) -> Vec<KeyTarget> {
    let asset = g913_asset("g913", "G913");
    key_surface(Some(&asset), None, gaming)
        .slots()
        .into_iter()
        .map(|slot| slot.target)
        .collect()
}

fn g_key(key: GKey) -> KeyTarget {
    KeyTarget::GKey(GKeySlot {
        profile: GKeyProfile::M1,
        key,
    })
}

#[test]
fn the_g913_render_shows_its_f_row_and_the_gaming_cluster_only_once_taken_over() {
    let f_row: Vec<KeyTarget> = FunctionKey::ALL[..13]
        .iter()
        .map(|key| {
            KeyTarget::FunctionKey(openlogi_core::config::KeyTrigger {
                keycode: key.keycode(),
                modifiers: openlogi_core::config::KeyModifiers::default(),
            })
        })
        .collect();
    assert_eq!(targets(None), f_row, "Esc and F1–F12, measured");
    let onboard = GamingKeys {
        software_control: false,
        ..run_by_openlogi(GamingKeyMode::Profiles)
    };
    assert_eq!(
        targets(Some(onboard)),
        f_row,
        "the onboard actions own G1–G5 until the user hands them over"
    );

    let profiles = targets(Some(run_by_openlogi(GamingKeyMode::Profiles)));
    let cluster: Vec<_> = [
        KeyTarget::ModeKey(ModeKey::M1),
        KeyTarget::ModeKey(ModeKey::M2),
        KeyTarget::ModeKey(ModeKey::M3),
    ]
    .into_iter()
    .chain(GKey::ALL.map(g_key))
    .collect();
    assert_eq!(profiles[13..], cluster, "MR keeps recording macros");

    let nine = targets(Some(run_by_openlogi(GamingKeyMode::NineButtons)));
    assert!(nine.contains(&KeyTarget::ModeKey(ModeKey::MacroRecord)));
    assert_eq!(nine.len(), 13 + 9);
}

#[test]
fn only_the_g913_render_is_measured() {
    let gaming = Some(run_by_openlogi(GamingKeyMode::Profiles));
    for (depot, name) in [("g915", "G915"), ("g915_lightspeed", "G915")] {
        let g915 = g913_asset(depot, name);
        assert!(
            key_surface(Some(&g915), None, gaming)
                .slots()
                .iter()
                .any(|slot| matches!(slot.target, KeyTarget::GKey(_))),
            "a G915 ({depot}) shows the same render"
        );
    }
    // The G915 X places its keys from its own depot.
    let g915_x = g913_asset("g915_x_ls", "G915 X LS");
    assert!(
        !key_surface(Some(&g915_x), None, gaming)
            .slots()
            .iter()
            .any(|slot| matches!(slot.target, KeyTarget::GKey(_)))
    );
}

#[test]
fn g_key_cards_sit_beside_the_board_and_the_top_rows_above_it() {
    let asset = g913_asset("g913", "G913");
    let slots = key_surface(
        Some(&asset),
        None,
        Some(run_by_openlogi(GamingKeyMode::NineButtons)),
    )
    .slots();
    let markers: Vec<(f32, f32)> = slots
        .iter()
        .map(|slot| (slot.point.x_frac, slot.point.y_frac))
        .collect();
    let (img_w, img_h) = (KEYBOARD_W, KEYBOARD_W * 1125. / 3600.);
    let layout = callout_layout(&markers, img_w, img_h);
    for (slot, card) in slots.iter().zip(&layout.cards) {
        let beside = card.top >= CALLOUT_BAND_H;
        assert_eq!(
            beside,
            matches!(slot.target, KeyTarget::GKey(_)),
            "{}: {card:?}",
            slot.legend
        );
    }
    assert!(
        layout.image_left > 0.,
        "the G-key column gets a band on the left"
    );
}

/// M1–M3 and MR sit right above F1–F4. Their cards read as one row of their
/// own — one lane, in key order — and the F-keys under them take the other
/// lane, so neither row's cards interleave with the other's.
#[test]
fn the_m_key_row_takes_a_lane_of_its_own_above_the_f_keys() {
    for mode in [GamingKeyMode::Profiles, GamingKeyMode::NineButtons] {
        m_key_row_takes_a_lane_of_its_own(mode);
    }
}

fn m_key_row_takes_a_lane_of_its_own(mode: GamingKeyMode) {
    let asset = g913_asset("g913", "G913");
    let slots = key_surface(Some(&asset), None, Some(run_by_openlogi(mode))).slots();
    let markers: Vec<(f32, f32)> = slots
        .iter()
        .map(|slot| (slot.point.x_frac, slot.point.y_frac))
        .collect();
    let (img_w, img_h) = (KEYBOARD_W, KEYBOARD_W * 1125. / 3600.);
    let layout = callout_layout(&markers, img_w, img_h);
    let card = |target: &KeyTarget| {
        let index = slots
            .iter()
            .position(|slot| slot.target == *target)
            .expect("the key shows");
        layout.cards[index]
    };
    let m_row: Vec<CardRect> = ModeKey::ALL
        .iter()
        .filter(|key| mode == GamingKeyMode::NineButtons || key.profile().is_some())
        .map(|key| card(&KeyTarget::ModeKey(*key)))
        .collect();
    assert!(
        m_row.iter().all(|card| card.top == KEY_CALLOUT_TOP_UPPER),
        "M1–M3 and MR share the upper lane: {m_row:?}"
    );
    assert!(
        m_row.windows(2).all(|pair| pair[0].left < pair[1].left),
        "in key order: {m_row:?}"
    );
    // F1–F4 sit under M1–M3 and MR; profile mode shows no MR over F4.
    for f_key in &FunctionKey::ALL[1..=m_row.len()] {
        let target = KeyTarget::FunctionKey(openlogi_core::config::KeyTrigger {
            keycode: f_key.keycode(),
            modifiers: openlogi_core::config::KeyModifiers::default(),
        });
        assert_eq!(
            card(&target).top,
            KEY_CALLOUT_TOP_LOWER,
            "{} sits under the M keys",
            f_key.label()
        );
    }
    assert_clean_callouts(&layout);
}

#[test]
fn the_edited_profile_moves_the_g_keys_and_m_keys_switch_it() {
    let asset = g913_asset("g913", "G913");
    let layouts = key_surface(
        Some(&asset),
        None,
        Some(run_by_openlogi(GamingKeyMode::Profiles)),
    )
    .slots();
    let slots = key_slots_from_layouts(
        layouts,
        None,
        KeyEditScope {
            page: KeypadPage::FIRST,
            g_profile: GKeyProfile::M2,
            gaming_mode: GamingKeyMode::Profiles,
        },
    );
    let g1 = slots
        .iter()
        .find(|slot| matches!(slot.target, KeyTarget::GKey(_)))
        .expect("G1 shows");
    assert_eq!(
        g1.target,
        KeyTarget::GKey(GKeySlot {
            profile: GKeyProfile::M2,
            key: GKey::G1,
        })
    );
    assert_eq!(g1.label.as_ref(), "G1 (M2)");
    let profile_cards: Vec<(ModeKey, bool)> = slots
        .iter()
        .filter_map(|slot| match slot.target {
            KeyTarget::ModeKey(key) => Some((key, slot.bound)),
            _ => None,
        })
        .collect();
    assert_eq!(
        profile_cards,
        [
            (ModeKey::M1, false),
            (ModeKey::M2, true),
            (ModeKey::M3, false)
        ],
        "the profile being edited reads as the active one"
    );
}

#[test]
fn nine_button_mode_edits_the_m1_g_keys_whatever_profile_was_last_edited() {
    let asset = g913_asset("g913", "G913");
    let layouts = key_surface(
        Some(&asset),
        None,
        Some(run_by_openlogi(GamingKeyMode::NineButtons)),
    )
    .slots();
    let slots = key_slots_from_layouts(
        layouts,
        None,
        KeyEditScope {
            page: KeypadPage::FIRST,
            g_profile: GKeyProfile::M3,
            gaming_mode: GamingKeyMode::NineButtons,
        },
    );
    let g_keys: Vec<_> = slots
        .iter()
        .filter_map(|slot| match slot.target {
            KeyTarget::GKey(slot) => Some(slot.profile),
            _ => None,
        })
        .collect();
    assert_eq!(g_keys, [GKeyProfile::M1; 5], "presses run the M1 slots");
}

#[gpui::test]
fn the_profile_picker_wins_over_a_selected_m_key_card(cx: &mut gpui::TestAppContext) {
    use crate::services::assets::AssetResolver;
    use crate::state::Sources;

    let asset = g913_asset("g913", "G913");
    let gaming = Some(run_by_openlogi(GamingKeyMode::Profiles));
    let layouts = || key_surface(Some(&asset), None, gaming).slots();
    let m2_card = layouts()
        .iter()
        .position(|slot| slot.target == KeyTarget::ModeKey(ModeKey::M2))
        .expect("M2 shows");
    cx.update(|cx| {
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let resolver = AssetResolver::new();
        let state = cx.new(|_| {
            AppState::new(Sources::in_memory(
                openlogi_core::config::Config::ephemeral(),
                &resolver,
                commands,
            ))
        });
        AppState::set_global(state, cx);
        let view = cx.new(FunctionRowView::new);
        view.update(cx, |view, cx| {
            view.select_key(Some(m2_card), cx);
            view.edited_slots(layouts(), None, gaming);
            assert_eq!(view.g_profile, GKeyProfile::M2, "the card selects M2");

            view.set_g_profile(GKeyProfile::M3, cx);
            view.edited_slots(layouts(), None, gaming);
            assert_eq!(view.g_profile, GKeyProfile::M3, "the picker's choice stays");
            assert_eq!(view.selected_key, None, "the M2 card is deselected");
        });
    });
}
#[gpui::test]
fn a_mode_change_drops_the_selection_and_its_draft(cx: &mut gpui::TestAppContext) {
    use crate::services::assets::AssetResolver;
    use crate::state::Sources;

    let asset = g913_asset("g913", "G913");
    let profiles = Some(run_by_openlogi(GamingKeyMode::Profiles));
    let nine = Some(run_by_openlogi(GamingKeyMode::NineButtons));
    let g1 = key_surface(Some(&asset), None, profiles)
        .slots()
        .iter()
        .position(|slot| matches!(slot.target, KeyTarget::GKey(_)))
        .expect("G1 shows");
    cx.update(|cx| {
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let resolver = AssetResolver::new();
        let state = cx.new(|_| {
            AppState::new(Sources::in_memory(
                openlogi_core::config::Config::ephemeral(),
                &resolver,
                commands,
            ))
        });
        AppState::set_global(state, cx);
        let view = cx.new(FunctionRowView::new);
        view.update(cx, |view, cx| {
            view.edited_slots(
                key_surface(Some(&asset), None, profiles).slots(),
                None,
                profiles,
            );
            view.select_key(Some(g1), cx);
            view.open_editor(PowerUserKind::TypeText, cx);

            view.edited_slots(key_surface(Some(&asset), None, nine).slots(), None, nine);
            assert_eq!(view.selected_key, None, "the index no longer names G1");
            assert_eq!(view.active_editor, None, "the draft can't save to MR");
        });
    });
}
