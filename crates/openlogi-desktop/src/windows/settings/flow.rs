//! Flow settings page: how the pointer takes the mouse and keyboard to another
//! computer, and whether copied text goes with it.

use super::{
    AppState, IconName, InteractiveElement, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_component::radio::{Radio, RadioGroup};

pub(super) fn flow_page() -> SettingPage {
    SettingPage::new(tr!("flow.flow"))
        .icon(IconName::Network)
        .description(tr!("flow.flow_description"))
        .resettable(false)
        .group(
            SettingGroup::new()
                .item(switch_computers_item())
                .item(copy_and_paste_item()),
        )
}

/// The two ways Logi Options+ offers to switch: the edge alone, or the edge
/// with Control held, so reaching for something at the edge never switches.
fn switch_computers_item() -> SettingItem {
    SettingItem::new(
        tr!("flow.switch_computers"),
        SettingField::render(|_, _, cx| {
            let required = AppState::try_read(cx).is_some_and(|s| s.flow_config().require_modifier);
            RadioGroup::vertical("flow-switch-computers")
                .child(
                    Radio::new("flow-edge")
                        .label(tr!("flow.edge_of_screen"))
                        .debug_selector(|| "flow-edge".into()),
                )
                .child(
                    Radio::new("flow-edge-holding-control")
                        .label(if cfg!(target_os = "macos") {
                            tr!("flow.edge_of_screen_holding_control")
                        } else {
                            tr!("flow.edge_of_screen_holding_ctrl")
                        })
                        .debug_selector(|| "flow-edge-holding-control".into()),
                )
                .selected_index(Some(usize::from(required)))
                .on_change(|index, _, cx| {
                    let required = match index {
                        0 => false,
                        1 => true,
                        _ => unreachable!("Flow has exactly two ways to switch computers"),
                    };
                    AppState::apply(cx, |state| state.commit_flow_require_modifier(required));
                })
        }),
    )
    .layout(gpui::Axis::Vertical)
    .description(tr!("flow.switch_computers_description"))
}

/// The shared-clipboard switch. Both computers need it on.
fn copy_and_paste_item() -> SettingItem {
    SettingItem::new(
        tr!("flow.copy_and_paste"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.flow_config().clipboard),
            |enabled, cx| {
                AppState::apply(cx, |state| state.commit_flow_clipboard(enabled));
            },
        ),
    )
    .description(tr!("flow.copy_and_paste_description"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::{ConfigPersistence, Sources};
    use crate::ui::theme;
    use crate::windows::{WindowRegistry, settings};
    use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext, px, size};
    use openlogi_core::config::Config;

    #[gpui::test]
    fn switch_computers_choices_update_state_and_respect_failed_saves(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            theme::register_builtin_themes(cx);
        });

        for read_only in [false, true] {
            let handle = cx.update(|cx| {
                let (commands, _) = tokio::sync::mpsc::unbounded_channel();
                let resolver = AssetResolver::new();
                let mut sources = Sources::in_memory(Config::ephemeral(), &resolver, commands);
                if read_only {
                    sources.persistence = ConfigPersistence::ReadOnly("read-only test".into());
                }
                AppState::set_global(cx.new(|_| AppState::new(sources)), cx);
                settings::open_at(settings::SettingsPage::Flow, cx);
                cx.global::<WindowRegistry>().settings.unwrap()
            });
            let mut visual = VisualTestContext::from_window(handle.into(), cx);
            visual.simulate_resize(size(px(920.), px(640.)));
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let edge = visual.debug_bounds("flow-edge").unwrap();
            let holding_control = visual.debug_bounds("flow-edge-holding-control").unwrap();
            assert!(holding_control.top() >= edge.bottom());

            visual.simulate_click(holding_control.center(), Modifiers::default());
            visual.update(|window, cx| {
                window.draw(cx).clear(cx);
                let state = AppState::try_read(cx).unwrap();
                assert_eq!(state.flow_config().require_modifier, !read_only);
            });
            visual.simulate_click(edge.center(), Modifiers::default());
            visual.update(|window, cx| {
                window.draw(cx).clear(cx);
                let state = AppState::try_read(cx).unwrap();
                assert!(!state.flow_config().require_modifier);
                window.remove_window();
            });
        }
    }
}
