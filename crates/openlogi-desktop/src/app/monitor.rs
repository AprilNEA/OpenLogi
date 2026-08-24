use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::{
    Disableable, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};
use openlogi_core::device::DeviceKind;

use super::AppView;
use super::widgets::back_button;
use crate::state::{
    AppState, HostSwitchKeyboardDevice, HostSwitchTargetDevice, MonitorDiscovery, StateEvent,
};
use crate::ui::theme::{self, Palette, SCREEN_PAD, Typography as _};

pub(super) fn monitor_header(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let view = cx.entity();
    h_flex()
        .h(px(crate::ui::theme::HEADER_H))
        .w_full()
        .px_5()
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(pal.border)
        .child(back_button(cx))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(
                    Icon::empty()
                        .path("action-icons/monitor.svg")
                        .size_5()
                        .text_color(theme::accent()),
                )
                .child(div().text_heading().child("显示器联动")),
        )
        .child(
            Button::new("monitor-refresh")
                .icon(Icon::empty().path("action-icons/refresh-cw.svg"))
                .label("重新扫描")
                .on_click(move |_, _, cx| {
                    view.update(cx, |_this, cx| AppView::refresh_monitors(cx));
                }),
        )
}

pub(super) fn monitor_content(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let discovery = AppState::try_read(cx).map_or(MonitorDiscovery::Idle, |state| {
        state.monitor_discovery().clone()
    });
    let warning = AppState::try_read(cx).and_then(|state| {
        state
            .host_switch_warning()
            .map(std::string::ToString::to_string)
    });
    v_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scrollbar()
        .items_center()
        .p(px(SCREEN_PAD))
        .child(
            v_flex()
                .w_full()
                .max_w(px(980.))
                .gap_4()
                .when_some(warning, |this, warning| {
                    this.child(warning_banner(warning, pal))
                })
                .child(easy_switch_panel(pal, cx))
                .child(monitor_link_panel(pal, cx))
                .child(match discovery {
                    MonitorDiscovery::Idle => empty_card(pal).into_any_element(),
                    MonitorDiscovery::Loading => loading_card(pal).into_any_element(),
                    MonitorDiscovery::Failed(error) => error_card(error, pal).into_any_element(),
                    MonitorDiscovery::Ready(monitors) => monitors_card(&monitors, pal, cx),
                }),
        )
}

fn panel_card(
    title: String,
    subtitle: impl Into<Option<String>>,
    icon: Icon,
    pal: Palette,
    content: AnyElement,
) -> impl IntoElement {
    v_flex()
        .gap_4()
        .border_1()
        .border_color(pal.border)
        .rounded(pal.card_radius)
        .bg(pal.panel)
        .p_4()
        .child(
            h_flex()
                .items_center()
                .gap_3()
                .child(icon.size_5().text_color(theme::accent()))
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_subheading()
                                .text_color(pal.text_primary)
                                .child(title),
                        )
                        .when_some(subtitle.into(), |this, subtitle| {
                            this.child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(subtitle),
                            )
                        }),
                ),
        )
        .child(content)
}

fn easy_switch_panel(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let keyboard_name = AppState::try_read(cx)
        .and_then(AppState::host_switch_keyboard_name)
        .unwrap_or_else(|| "未选择发起键盘".to_string());
    panel_card(
        "Easy-Switch 设备跟随".into(),
        Some(format!(
            "发起设备：{keyboard_name}。按这把键盘的 1 / 2 / 3 时，开启的鼠标或指针设备会跟随到同一个电脑。"
        )),
        Icon::empty().path("action-icons/keyboard.svg"),
        pal,
        v_flex()
            .gap_4()
            .child(keyboard_selector(pal, cx))
            .child(follow_devices_panel(pal, cx))
            .into_any_element(),
    )
}

fn monitor_link_panel(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let enabled = AppState::try_read(cx).is_none_or(AppState::host_monitor_enabled);
    panel_card(
        "Easy-Switch 显示器联动".into(),
        Some("把键盘 Easy-Switch 1 / 2 / 3 和下面的显示器输入源绑定起来；键鼠切换成功后才执行显示器切换。".into()),
        Icon::empty().path("action-icons/monitor.svg"),
        pal,
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap_4()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_subheading()
                                    .text_color(pal.text_primary)
                                    .child(if enabled {
                        "显示器输入源会跟随切换"
                    } else {
                        "显示器输入源不会自动切换"
                    }),
                            )
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child("关闭后保留所有显示器绑定，但按键盘 Easy-Switch 时不再切显示器。测试只会立即切到目标输入源，不会自动切回。"),
                            ),
                    )
                    .child(
                        Switch::new("host-monitor-enabled")
                            .checked(enabled)
                            .on_click(|checked, _window, cx| {
                                let enabled = *checked;
                                AppState::update(cx, |state, cx| {
                                    state.set_host_monitor_enabled(enabled);
                                    cx.emit(StateEvent::MonitorChanged);
                                });
                            }),
                    ),
            )
            .child(logic_diagram(enabled, pal))
            .into_any_element(),
    )
}

fn keyboard_selector(pal: Palette, cx: &mut Context<AppView>) -> AnyElement {
    let keyboards =
        AppState::try_read(cx).map_or_else(Vec::new, AppState::host_switch_keyboard_devices);
    section_block(pal)
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_body()
                                .text_color(pal.text_primary)
                                .child("选择发起键盘"),
                        )
                        .child(
                            div()
                                .text_caption()
                                .text_color(pal.text_muted)
                                .child("多键盘时必须明确选择哪一把键盘的 Easy-Switch 负责这些绑定；离线键盘不可编辑，避免写到旧设备。"),
                        ),
                )
                .child(status_pill(format!("{} 把键盘", keyboards.len()), pal)),
        )
        .child(if keyboards.is_empty() {
            div()
                .p_3()
                .rounded(pal.control_radius)
                .border_1()
                .border_color(pal.border)
                .text_caption()
                .text_color(pal.text_muted)
                .child("当前没有发现可配置的键盘。请先连接支持 Easy-Switch 的 Logitech 键盘。")
                .into_any_element()
        } else {
            h_flex()
                .gap_2()
                .flex_wrap()
                .children(
                    keyboards
                        .iter()
                        .map(|keyboard| keyboard_choice(keyboard, pal).into_any_element()),
                )
                .into_any_element()
        })
        .into_any_element()
}

fn keyboard_choice(keyboard: &HostSwitchKeyboardDevice, pal: Palette) -> impl IntoElement {
    let key = keyboard.config_key.clone();
    let button = Button::new(format!("host-switch-keyboard-{key}"))
        .small()
        .icon(Icon::empty().path("action-icons/keyboard.svg").size_3())
        .label(format!(
            "{} · {}",
            keyboard.display_name,
            if keyboard.online {
                "已连接"
            } else {
                "离线"
            }
        ))
        .tooltip("选择这把已连接键盘作为 Easy-Switch 联动的发起设备；离线键盘不能写入新绑定")
        .disabled(!keyboard.online)
        .on_click(move |_, _, cx| {
            AppState::update(cx, |state, cx| {
                state.set_host_switch_keyboard_key(key.clone());
                cx.emit(StateEvent::MonitorChanged);
            });
        });
    if keyboard.selected {
        button.primary()
    } else {
        button.outline().text_color(pal.text_muted)
    }
}

fn follow_devices_panel(pal: Palette, cx: &mut Context<AppView>) -> AnyElement {
    let devices =
        AppState::try_read(cx).map_or_else(Vec::new, AppState::host_switch_target_devices);
    section_block(pal)
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_body()
                                .text_color(pal.text_primary)
                                .child("鼠标/指针设备跟随键盘"),
                        )
                        .child(
                            div()
                                .text_caption()
                                .text_color(pal.text_muted)
                                .child("支持多个设备和不同型号；只要设备支持 Logitech 多主机切换，就可以跟随这把键盘切到同一个 Easy-Switch 序号。"),
                        ),
                )
                .child(status_pill(format!("{} 个可选设备", devices.len()), pal)),
        )
        .child(if devices.is_empty() {
            div()
                .p_3()
                .rounded(pal.control_radius)
                .border_1()
                .border_color(pal.border)
                .text_caption()
                .text_color(pal.text_muted)
                .child("当前没有发现可跟随的鼠标、轨迹球或触控板。设备需要先出现在 OpenLogi 首页里。")
                .into_any_element()
        } else {
            v_flex()
                .gap_2()
                .children(
                    devices
                        .into_iter()
                        .map(|device| follow_device_row(device, pal).into_any_element()),
                )
                .into_any_element()
        })
        .into_any_element()
}

fn section_block(pal: Palette) -> gpui::Div {
    v_flex()
        .p_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.control)
}

fn follow_device_row(device: HostSwitchTargetDevice, pal: Palette) -> impl IntoElement {
    let key = device.config_key.clone();
    h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .p_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(if device.selected {
            theme::accent()
        } else {
            pal.border
        })
        .bg(pal.control)
        .child(
            h_flex()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(
                    Icon::empty()
                        .path("action-icons/mouse.svg")
                        .size_5()
                        .text_color(if device.selected {
                            theme::accent()
                        } else {
                            pal.text_muted
                        }),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .text_body()
                                .text_color(pal.text_primary)
                                .child(device.display_name),
                        )
                        .child(
                            div()
                                .text_caption()
                                .text_color(pal.text_muted)
                                .child(format!(
                                    "{} · {}",
                                    follow_kind_label(device.kind),
                                    if device.online { "已连接" } else { "离线" }
                                )),
                        ),
                ),
        )
        .child(
            Switch::new(format!("host-switch-target-{key}"))
                .checked(device.selected)
                .on_click(move |checked, _window, cx| {
                    let enabled = *checked;
                    AppState::update(cx, |state, cx| {
                        state.set_host_switch_target_enabled(&key, enabled);
                        cx.emit(StateEvent::MonitorChanged);
                    });
                }),
        )
}

fn logic_diagram(enabled: bool, pal: Palette) -> impl IntoElement {
    section_block(pal)
        .gap_3()
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .items_center()
                .child(step_card("1", "按键盘", "MX Keys 上的 Easy-Switch", pal))
                .child(flow_arrow(pal))
                .child(keyboard_switch_keys(pal))
                .child(flow_arrow(pal))
                .child(step_card("2", "OpenLogi Agent", "识别切到几号电脑", pal))
                .child(flow_arrow(pal))
                .child(result_card(enabled, pal)),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child("这里的 1 / 2 / 3 对应键盘实体切换键，不是普通快捷键。OpenLogi 会先检查键盘和跟随设备是否能切到目标序号，再切鼠标/指针设备，最后切键盘；设备成功后才会连续切显示器，不再额外等待。若设备切换失败，会尝试把已切换的设备恢复，并在本页提示用户检查。"),
        )
}

fn warning_banner(message: String, pal: Palette) -> impl IntoElement {
    h_flex()
        .items_start()
        .gap_2()
        .p_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(rgb(0x00f9_7316))
        .bg(rgb(0x00ff_f7ed))
        .child(
            Icon::new(IconName::TriangleAlert)
                .size_4()
                .text_color(rgb(0x00c2_410c)),
        )
        .child(
            div()
                .flex_1()
                .text_caption()
                .text_color(rgb(0x009a_3412))
                .child(message),
        )
}

fn step_card(index: &str, title: &str, subtitle: &str, pal: Palette) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_center()
        .min_w(px(170.))
        .p_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.control)
        .child(
            div()
                .size(px(24.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::accent())
                .text_caption()
                .text_color(rgb(0x00ff_ffff))
                .child(index.to_string()),
        )
        .child(
            v_flex()
                .min_w_0()
                .child(
                    div()
                        .text_body()
                        .text_color(pal.text_primary)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(subtitle.to_string()),
                ),
        )
}

fn keyboard_switch_keys(pal: Palette) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_center()
        .p_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.control)
        .child(keycap(1, pal))
        .child(keycap(2, pal))
        .child(keycap(3, pal))
}

fn keycap(number: u8, pal: Palette) -> impl IntoElement {
    v_flex()
        .size(px(46.))
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .border_1()
        .border_color(pal.border)
        .bg(pal.panel)
        .child(
            Icon::empty()
                .path("action-icons/keyboard.svg")
                .size_3()
                .text_color(pal.text_muted),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_primary)
                .child(number.to_string()),
        )
}

fn result_card(enabled: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_center()
        .min_w(px(210.))
        .p_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(if enabled { theme::accent() } else { pal.border })
        .bg(pal.control)
        .child(
            Icon::empty()
                .path("action-icons/monitor.svg")
                .size_5()
                .text_color(if enabled {
                    theme::accent()
                } else {
                    pal.text_muted
                }),
        )
        .child(
            v_flex()
                .child(
                    div()
                        .text_body()
                        .text_color(pal.text_primary)
                        .child("跟随设备 + 显示器"),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(if enabled {
                            "开启的设备跟随，显示器切输入源"
                        } else {
                            "设备跟随照常，显示器不切换"
                        }),
                ),
        )
}

fn flow_arrow(pal: Palette) -> impl IntoElement {
    div().text_body().text_color(pal.text_muted).child("->")
}

fn follow_kind_label(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Mouse => "鼠标",
        DeviceKind::Trackball => "轨迹球",
        DeviceKind::Touchpad => "触控板",
        _ => "指针设备",
    }
}

fn empty_card(pal: Palette) -> impl IntoElement {
    panel_card(
        "显示器输入源".into(),
        None,
        Icon::empty().path("action-icons/monitor.svg"),
        pal,
        v_flex()
            .gap_2()
            .child(div().text_body().child("点击“重新扫描”读取当前电脑能控制的显示器和输入端口。"))
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child("扫描依赖显示器 DDC/CI。它和 ControlMyMonitor 使用的是同类底层能力，但这里由 OpenLogi 直接调用 Windows API。注意：测试切到另一个输入后，当前电脑可能失去这台显示器的 DDC/CI 控制通道，所以软件不能保证切回；请准备好用显示器实体按键或另一台电脑手动切回。"),
            )
            .into_any_element(),
    )
}

fn loading_card(pal: Palette) -> impl IntoElement {
    panel_card(
        "显示器输入源".into(),
        None,
        Icon::empty().path("action-icons/refresh-cw.svg"),
        pal,
        div()
            .text_body()
            .text_color(pal.text_muted)
            .child("正在扫描显示器和输入端口...")
            .into_any_element(),
    )
}

fn error_card(error: String, pal: Palette) -> impl IntoElement {
    panel_card(
        "显示器输入源".into(),
        None,
        Icon::new(IconName::TriangleAlert),
        pal,
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_body()
                    .text_color(pal.text_primary)
                    .child("显示器扫描或测试失败"),
            )
            .child(div().text_caption().text_color(pal.text_muted).child(error))
            .into_any_element(),
    )
}

fn monitors_card(
    monitors: &[openlogi_monitor::MonitorInfo],
    pal: Palette,
    cx: &mut Context<AppView>,
) -> AnyElement {
    if monitors.is_empty() {
        return panel_card(
            "显示器输入源".into(),
            None,
            Icon::empty().path("action-icons/monitor.svg"),
            pal,
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_body()
                        .child("没有扫描到可通过 DDC/CI 控制的显示器。"),
                )
                .child(
                    div().text_caption().text_color(pal.text_muted).child(
                        "请确认显示器菜单里已开启 DDC/CI，并且当前线材/转接器支持显示器控制。",
                    ),
                )
                .into_any_element(),
        )
        .into_any_element();
    }
    v_flex()
        .gap_3()
        .children(monitors.iter().map(|monitor| monitor_row(monitor, pal, cx)))
        .into_any_element()
}

fn monitor_row(
    monitor: &openlogi_monitor::MonitorInfo,
    pal: Palette,
    cx: &mut Context<AppView>,
) -> AnyElement {
    let current = monitor
        .current_input
        .map_or_else(|| "未知".to_string(), readable_input);
    let display = readable_display_name(&monitor.display_name);
    v_flex()
        .gap_3()
        .border_1()
        .border_color(pal.border)
        .rounded(pal.card_radius)
        .bg(pal.panel)
        .p_4()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_4()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_3()
                        .items_center()
                        .child(
                            Icon::empty()
                                .path("action-icons/monitor.svg")
                                .size_6()
                                .text_color(theme::accent()),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_subheading()
                                        .text_color(pal.text_primary)
                                        .child(monitor.friendly_name.clone()),
                                )
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(pal.text_muted)
                                        .child(format!("{display} · 配置标识：{}", monitor.id)),
                                ),
                        ),
                )
                .child(status_pill(format!("当前：{current}"), pal)),
        )
        .child(if monitor.inputs.is_empty() {
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child("这台显示器没有返回可切换输入源；可能是 DDC/CI 关闭、线材不支持，或当前输入不允许读取。")
                .into_any_element()
        } else {
            v_flex()
                .gap_2()
                .child(input_header(pal))
                .children(
                    monitor
                        .inputs
                        .iter()
                        .map(|input| input_row(&monitor.id, input, pal, cx)),
                )
                .into_any_element()
        })
        .into_any_element()
}

fn input_header(pal: Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .text_caption()
        .text_color(pal.text_muted)
        .child(div().flex_1().child("端口"))
        .child(div().w(px(112.)).child("测试"))
        .child(div().w(px(168.)).child("绑定到键盘切换键"))
}

fn input_row(
    monitor_id: &str,
    input: &openlogi_monitor::MonitorInput,
    pal: Palette,
    cx: &mut Context<AppView>,
) -> AnyElement {
    let monitor_id = monitor_id.to_string();
    let input_value = input.value;
    let view = cx.entity();
    h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .p_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.control)
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(input_icon(input_value, pal))
                .child(
                    div()
                        .text_body()
                        .text_color(pal.text_primary)
                        .child(readable_input(input_value)),
                ),
        )
        .child(
            Button::new(format!("monitor-test-{monitor_id}-{input_value}"))
                .small()
                .outline()
                .label("测试切到此显示器")
                .tooltip("立即把这台显示器切到这个输入源；只测试当前这一项，不保存到 1/2/3，也不会自动切回来")
                .on_click({
                    let monitor_id = monitor_id.clone();
                    move |_, _, cx| {
                        view.update(cx, |_this, cx| {
                            AppView::test_monitor_input(monitor_id.clone(), input_value, cx);
                        });
                    }
                }),
        )
        .child(
            h_flex()
                .gap_2()
                .children((0_u8..3).map(|host| {
                    host_button(host, monitor_id.clone(), input_value, pal, cx).into_any_element()
                })),
        )
        .into_any_element()
}

fn input_icon(input: u32, pal: Palette) -> impl IntoElement {
    let label = match input {
        0x08 | 0x09 | 0x0f | 0x10 => "DP",
        0x11 | 0x12 => "HDMI",
        0x13 | 0x14 => "C",
        _ => "IN",
    };
    div()
        .w(px(42.))
        .h(px(24.))
        .rounded(px(5.))
        .border_1()
        .border_color(pal.border)
        .flex()
        .items_center()
        .justify_center()
        .text_caption()
        .text_color(pal.text_muted)
        .child(label)
}

fn host_button(
    host: u8,
    monitor_id: String,
    input: u32,
    pal: Palette,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let selected = AppState::try_read(cx)
        .and_then(|state| state.host_monitor_input(host, &monitor_id))
        == Some(input);
    let label = format!("{}", host + 1);
    let button = Button::new(format!("monitor-host-{host}-{monitor_id}-{input}"))
        .small()
        .icon(Icon::empty().path("action-icons/keyboard.svg").size_3())
        .label(label)
        .tooltip(format!(
            "绑定到键盘 Easy-Switch {}：以后按键盘的 {} 号切换键时，这台显示器会切到这个输入源",
            host + 1,
            host + 1
        ))
        .on_click(move |_, _, cx| {
            AppState::update(cx, |state, cx| {
                state.commit_host_monitor_input(host, monitor_id.clone(), input);
                cx.emit(StateEvent::MonitorChanged);
            });
        });
    if selected {
        button.primary()
    } else {
        button.outline().text_color(pal.text_muted)
    }
}

fn status_pill(label: String, pal: Palette) -> impl IntoElement {
    h_flex()
        .flex_none()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(pal.border)
        .px_2()
        .py_1()
        .text_caption()
        .text_color(pal.text_muted)
        .child(label)
}

fn readable_display_name(display_name: &str) -> String {
    display_name
        .trim_start_matches(r"\\.\")
        .trim_start_matches("DISPLAY")
        .parse::<usize>()
        .map_or_else(
            |_| display_name.to_string(),
            |number| format!("显示器 {number}"),
        )
}

fn readable_input(value: u32) -> String {
    let label = match value {
        0x01 => "DVI 1",
        0x02 => "DVI 2",
        0x03 => "VGA 1",
        0x04 => "S-Video 1",
        0x05 => "Composite 1",
        0x06 => "Component 1",
        0x07 => "Component 2",
        0x08 | 0x0f => "DP 1",
        0x09 | 0x10 => "DP 2",
        0x11 => "HDMI 1",
        0x12 => "HDMI 2",
        0x13 => "USB-C 1",
        0x14 => "USB-C 2",
        _ => return format!("输入源 0x{value:02x}"),
    };
    format!("{label} (0x{value:02x})")
}
