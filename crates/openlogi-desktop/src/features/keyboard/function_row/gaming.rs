//! A G-series keyboard's gaming-cluster bar in the Keys view: whether
//! OpenLogi runs G1–G5, what M1–M3 do then, and which M profile's G-keys
//! the view edits.
//!
//! The G-keys and M keys themselves are ordinary key slots of the parent
//! view ([`super::key_points::KeyTarget::GKey`]); this only draws the bar and
//! reports the profile choice back to it.

use gpui::{App, Entity, IntoElement, ParentElement, Styled, Window, div};
use gpui_kit::component::button::{Button, ButtonGroup};
use gpui_kit::component::{Selectable as _, Sizable as _, h_flex, v_flex};
use openlogi_core::binding::GKeyProfile;
use openlogi_core::config::{GKeyOwner, GamingKeyMode};

use super::FunctionRowView;
use crate::state::{AppState, GamingKeys};
use crate::ui::components::Toggle;
use crate::ui::theme::{Palette, Typography as _};

/// The bar over a G-series keyboard: the takeover switch, then — while
/// OpenLogi runs the G-keys of a keyboard with M keys — the mode, and in
/// profile mode the profile being edited.
pub(super) fn gaming_bar(
    gaming: GamingKeys,
    editing: GKeyProfile,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
) -> impl IntoElement {
    let takeover = Toggle::new("g-key-software-control")
        .selected(gaming.software_control)
        .label(tr!("keyboard.g_key_software_control"))
        .on_change(|enabled, _, cx| {
            let owner = if *enabled {
                GKeyOwner::OpenLogi
            } else {
                GKeyOwner::Keyboard
            };
            AppState::apply(cx, |state| state.commit_g_key_owner(owner));
        });
    let shows_modes = gaming.software_control && gaming.m_keys;
    let modes = [GamingKeyMode::Profiles, GamingKeyMode::NineButtons];
    let mode_group = shows_modes.then(|| {
        ButtonGroup::new("g-key-mode")
            .outline()
            .small()
            .children(modes.iter().map(|mode| {
                Button::new(mode_id(*mode))
                    .label(mode_label(*mode))
                    .selected(*mode == gaming.mode)
            }))
            .on_click(move |indices: &Vec<usize>, _: &mut Window, cx: &mut App| {
                if let Some(mode) = indices.first().and_then(|index| modes.get(*index)) {
                    let mode = *mode;
                    AppState::apply(cx, |state| state.commit_gaming_key_mode(mode));
                }
            })
    });
    let profile_group = (shows_modes && gaming.mode == GamingKeyMode::Profiles).then(|| {
        let view = view.clone();
        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("keyboard.g_key_profiles")),
            )
            .child(
                ButtonGroup::new("g-key-profiles")
                    .outline()
                    .small()
                    .children(GKeyProfile::ALL.iter().enumerate().map(|(index, profile)| {
                        Button::new(("g-key-profile", index))
                            .label(profile.label())
                            .selected(*profile == editing)
                    }))
                    .on_click(move |indices: &Vec<usize>, _: &mut Window, cx: &mut App| {
                        if let Some(profile) = indices
                            .first()
                            .and_then(|index| GKeyProfile::ALL.get(*index))
                        {
                            view.update(cx, |view, cx| view.set_g_profile(*profile, cx));
                        }
                    }),
            )
    });
    v_flex()
        .items_center()
        .gap_1()
        .child(
            h_flex()
                .gap_4()
                .items_center()
                .child(takeover)
                .children(mode_group)
                .children(profile_group),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("keyboard.g_key_software_control_description")),
        )
}

fn mode_id(mode: GamingKeyMode) -> &'static str {
    match mode {
        GamingKeyMode::Profiles => "g-key-mode-profiles",
        GamingKeyMode::NineButtons => "g-key-mode-nine-keys",
    }
}

fn mode_label(mode: GamingKeyMode) -> gpui::SharedString {
    match mode {
        GamingKeyMode::Profiles => tr!("keyboard.gaming_mode_profiles"),
        GamingKeyMode::NineButtons => tr!("keyboard.gaming_mode_nine_buttons"),
    }
}
