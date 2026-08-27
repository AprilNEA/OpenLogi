//! Easy-Switch follower selection for a keyboard's physical host keys.

use gpui::{Context, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder as _};
use gpui_component::{Icon, IconName, Selectable as _, h_flex, v_flex};

use crate::app::{AppView, kind_label, status_badge};
use crate::state::{AppState, HostSwitchTargetUpdate, StateEvent};
use crate::ui::components::{PanelCard, Toggle};
use crate::ui::theme::{self, Typography as _};

/// Settings content for choosing which devices follow the selected keyboard.
pub(crate) fn easy_switch_panel(cx: &mut Context<AppView>) -> impl IntoElement {
    let pal = theme::palette(cx);
    let targets =
        AppState::try_read(cx).map_or_else(Vec::new, AppState::host_switch_target_devices);
    let target_rows = targets.into_iter().map(|target| {
        let target_key = target.config_key.clone();
        h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .gap_4()
            .py_2()
            .child(
                h_flex()
                    .min_w_0()
                    .gap_3()
                    .child(status_badge(target.online, pal))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_body().truncate().child(target.display_name))
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(kind_label(target.kind)),
                            ),
                    ),
            )
            .child(
                Toggle::new(format!("easy-switch-target-{}", target.config_key))
                    .selected(target.selected)
                    .on_change(move |enabled, _window, cx| {
                        AppState::update(cx, |state, cx| {
                            match state.set_host_switch_target_enabled(&target_key, *enabled) {
                                HostSwitchTargetUpdate::Unchanged => {}
                                HostSwitchTargetUpdate::Persisted(key) => {
                                    cx.emit(StateEvent::DeviceConfigChanged(key));
                                }
                                HostSwitchTargetUpdate::RolledBack => {
                                    cx.emit(StateEvent::SettingsChanged);
                                }
                            }
                        });
                    }),
            )
    });
    let has_targets = target_rows.len() > 0;

    v_flex()
        .w_full()
        .gap_4()
        .child(PanelCard::new(
            tr!("easy_switch.linked_devices"),
            Icon::empty().path("action-icons/refresh-cw.svg"),
            v_flex()
                .gap_3()
                .child(
                    div()
                        .text_body()
                        .child(tr!("easy_switch.press_host_key_description")),
                )
                .when(!has_targets, |this| {
                    this.child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("easy_switch.no_compatible_devices")),
                    )
                })
                .children(target_rows),
        ))
        .child(PanelCard::new(
            tr!("easy_switch.before_switching"),
            Icon::new(IconName::Info),
            v_flex()
                .gap_2()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("easy_switch.pair_channels_description"))
                .child(tr!("easy_switch.enable_same_links_description")),
        ))
}
