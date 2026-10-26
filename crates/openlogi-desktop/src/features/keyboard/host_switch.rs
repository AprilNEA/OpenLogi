//! The Device tab's "Switch together" card: the Easy-Switch pointing devices
//! that follow the selected keyboard to the same computer.

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement, Styled, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Selectable as _, Sizable as _,
    table::{Table, TableBody, TableCell, TableRow},
    v_flex,
};

use crate::state::AppState;
use crate::ui::components::Toggle;
use crate::ui::theme::{self, Typography as _};

/// Render the controlled table. AppState remains the sole owner of the
/// selection; each row sends only the requested boolean value.
pub(crate) fn target_table(cx: &mut App) -> impl IntoElement {
    let pal = theme::palette(cx);
    let targets = AppState::try_read(cx)
        .map(AppState::current_host_switch_targets)
        .unwrap_or_default();
    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_body()
                .text_color(pal.text_muted)
                .child(tr!("device.switch_description")),
        )
        .when(targets.is_empty(), |column| {
            column.child(
                div()
                    .debug_selector(|| "host-switch-empty".into())
                    .py_4()
                    .text_body()
                    .text_color(pal.text_muted)
                    .child(tr!("device.no_switch_targets")),
            )
        })
        .when(!targets.is_empty(), |column| {
            column.child(
                Table::new()
                    .small()
                    .accessibility_label(tr!("device.switch_together"))
                    .border_1()
                    .border_color(pal.border)
                    .rounded(pal.control_radius)
                    .child(TableBody::new().children(targets.into_iter().map(|target| {
                        let target_key = target.key.clone();
                        let toggle_id = format!("host-switch-target-{}", target.key);
                        TableRow::new()
                            .child(
                                TableCell::new().child(
                                    v_flex()
                                        .child(
                                            div()
                                                .text_body()
                                                .text_color(pal.text_primary)
                                                .child(target.name),
                                        )
                                        .child(
                                            div().text_caption().text_color(pal.text_muted).child(
                                                if target.online {
                                                    tr!("device.connected")
                                                } else {
                                                    tr!("device.offline")
                                                },
                                            ),
                                        ),
                                ),
                            )
                            .child(
                                TableCell::new().text_right().child(
                                    div()
                                        .debug_selector({
                                            let toggle_id = toggle_id.clone();
                                            move || toggle_id.clone()
                                        })
                                        .child(
                                            Toggle::new(toggle_id)
                                                .selected(target.selected)
                                                .on_change(move |selected, _window, cx| {
                                                    let selected = *selected;
                                                    let target_key = target_key.clone();
                                                    AppState::apply(cx, move |state| {
                                                        state.commit_host_switch_target(
                                                            &target_key,
                                                            selected,
                                                        )
                                                    });
                                                }),
                                        ),
                                ),
                            )
                    }))),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, Context, Modifiers, Render, TestAppContext};
    use openlogi_core::config::Config;
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    };

    use crate::services::assets::AssetResolver;
    use crate::services::i18n::LOCALE_LOCK;
    use crate::state::{AppState, Sources};

    struct Harness;

    impl Render for Harness {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            cx: &mut Context<Self>,
        ) -> impl IntoElement {
            target_table(cx)
        }
    }

    fn inventory(name: &str, kind: DeviceKind, unit_id: [u8; 4]) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: format!("{name} receiver"),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some(format!("receiver-{name}")),
            },
            paired: vec![PairedDevice {
                slot: 1,
                codename: Some(name.to_string()),
                wpid: None,
                kind,
                online: true,
                battery: None,
                model_info: Some(DeviceModelInfo {
                    entity_count: 1,
                    serial_number: None,
                    unit_id,
                    transports: DeviceTransports::default(),
                    model_ids: [0, 0, 0],
                    extended_model_id: 0,
                }),
                capabilities: Some(Capabilities {
                    easy_switch: true,
                    ..Capabilities::presumed_from_kind(kind)
                }),
                settings_revision: 0,
            }],
        }
    }

    fn install_state(cx: &mut TestAppContext, with_target: bool) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::theme::register_builtin_themes(cx);
            let keyboard = inventory("MX Keys", DeviceKind::Keyboard, [1, 2, 3, 4]);
            let mouse = inventory("MX Master", DeviceKind::Mouse, [5, 6, 7, 8]);
            let inventories = if with_target {
                vec![keyboard, mouse]
            } else {
                vec![keyboard]
            };
            let mut config = Config::ephemeral();
            config.set_selected_device(Some("unit:01020304".to_string()));
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let resolver = AssetResolver::new();
            let state = cx.new(|_| {
                AppState::new(Sources {
                    inventories: &inventories,
                    ..Sources::in_memory(config, &resolver, commands)
                })
            });
            AppState::set_global(state, cx);
        });
    }

    #[gpui::test]
    fn target_toggle_updates_the_app_state(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        install_state(cx, true);
        let (_, cx) = cx.add_window_view(|_, _| Harness);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let toggle = cx
            .debug_bounds("host-switch-target-unit:05060708")
            .expect("the supported mouse has a switch row");

        cx.simulate_click(toggle.center(), Modifiers::default());

        cx.update(|_, cx| {
            assert!(
                AppState::try_read(cx)
                    .unwrap()
                    .current_host_switch_targets()[0]
                    .selected
            );
        });
    }

    #[gpui::test]
    fn empty_target_table_has_a_visible_empty_state(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        install_state(cx, false);
        let (_, cx) = cx.add_window_view(|_, _| Harness);
        cx.update(|window, cx| window.draw(cx).clear(cx));

        assert!(cx.debug_bounds("host-switch-empty").is_some());
    }
}
