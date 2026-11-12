//! Per-device native battery menu and warning preferences.

use gpui::{Context, IntoElement, ParentElement as _, Styled as _, div};
use gpui_kit::base::Selectable as _;
use gpui_kit::component::{Icon, IconName, v_flex};

use crate::app::AppView;
use crate::state::AppState;
use crate::ui::components::{PanelCard, Toggle};
use crate::ui::theme::{Palette, Typography as _};

/// Render the selected device's supported battery preferences.
pub(super) fn preferences_card(
    pal: Palette,
    cx: &mut Context<AppView>,
) -> Option<impl IntoElement> {
    let state = AppState::try_read(cx)?;
    let record = state.current_record()?;
    if !state.battery_preferences_available(record) {
        return None;
    }
    let key = record.device_key();
    let preferences = state.battery_preferences(key.as_str());
    #[cfg(target_os = "windows")]
    let menu_key = key.clone();
    #[cfg(target_os = "windows")]
    let menu = Toggle::new("battery-show-in-menu")
        .label(Some(tr!("device.battery_show_in_menu")))
        .selected(preferences.show_in_menu)
        .on_change(move |checked, _, cx| {
            AppState::apply(cx, |state| {
                let mut next = state.battery_preferences(menu_key.as_str());
                next.show_in_menu = *checked;
                state.commit_battery_preferences(&menu_key, next)
            });
        });
    let warning = Toggle::new("battery-warn-low")
        .label(Some(tr!("device.battery_warn_low")))
        .selected(preferences.warn_low)
        .on_change(move |checked, _, cx| {
            AppState::apply(cx, |state| {
                let mut next = state.battery_preferences(key.as_str());
                next.warn_low = *checked;
                state.commit_battery_preferences(&key, next)
            });
        });
    let controls = v_flex().gap_3();
    #[cfg(target_os = "windows")]
    let controls = controls.child(menu);
    Some(PanelCard::new(
        tr!("device.battery_preferences"),
        Icon::new(IconName::Battery),
        controls
            .child(warning)
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("device.battery_warn_description")),
            )
            .into_any_element(),
    ))
}
