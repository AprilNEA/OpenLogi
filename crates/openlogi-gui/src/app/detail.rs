//! The device-detail screen: the header (back + name + section tabs), and the
//! four section bodies (Buttons, Pointer, Lighting, Device).

use gpui::{
    AnyElement, BorrowAppContext as _, Context, FontWeight, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    IconName,
    description_list::{DescriptionItem, DescriptionList},
    h_flex,
    scroll::ScrollableElement as _,
    tab::TabBar,
    v_flex,
};
use openlogi_core::config::ScrollResolution;
use openlogi_core::device::DeviceKind;

use super::widgets::{
    add_device_button, back_button, battery_summary, kind_label, panel_card, panel_card_fill,
    route_label, sidebar_action, status_badge,
};
use super::{AppView, DetailTab};
use crate::app_menu::file_url;
use crate::components::dpi_panel::DpiPanel;
use crate::components::lighting_panel::LightingPanel;
use crate::components::smartshift_panel::SmartShiftPanel;
use crate::mouse_model::view::MouseModelView;
use crate::state::{AppState, DeviceRecord};
use crate::theme::{HEADER_H, Palette, SelectableStyle as _};

/// Device-detail top bar, in three zones: a back affordance + device name
/// (leading), the section tabs as a centred segmented control (middle), and the
/// connection status + Add-Device button (trailing). Hoisting the tabs here —
/// rather than a separate row beneath the bar — gives the section body the full
/// remaining height. A device with a single section shows no tab strip.
pub(super) fn detail_header(
    record: Option<&DeviceRecord>,
    tabs: &[DetailTab],
    active: DetailTab,
    pal: Palette,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let name = record.map_or_else(|| tr!("Device").to_string(), |r| r.display_name.clone());
    let online = record.map(|r| r.online);
    // Only a real choice gets a strip; a lone section (e.g. a keyboard with just
    // the info tab) would render a one-segment control, which reads as broken.
    // `into_any_element` here severs the returned element from `cx`'s lifetime
    // (RPIT would otherwise capture it), so the borrow ends with this call and
    // `back_button` below can take `cx` again.
    let tab_strip = (tabs.len() > 1).then(|| detail_tabs(tabs, active, cx).into_any_element());
    h_flex()
        .h(px(HEADER_H))
        // Fixed-height chrome must never shrink: a tab whose body overflows the
        // viewport would otherwise squeeze this shrinkable bar, so the header
        // height would visibly change between tabs. The body (flex_1 + its own
        // scroll) absorbs the overflow instead.
        .flex_shrink_0()
        .w_full()
        .px_5()
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(pal.border)
        .child(back_button(cx))
        .child(
            div()
                .min_w_0()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(name),
        )
        // Flexible spacers on either side centre the segmented tabs in the space
        // left between the leading and trailing zones.
        .child(div().flex_1())
        .children(tab_strip)
        .child(div().flex_1())
        .when_some(online, |this, online| this.child(status_badge(online, pal)))
        .child(add_device_button())
}

/// The device-detail body: the active section, filling the height between the
/// header and the footer. Which sections exist — and the segmented control that
/// switches them — is the header's job (see [`detail_header`] and
/// [`DetailTab::tabs_for`]); `active` arrives pre-resolved against this device's
/// tab set, so this only has to render the chosen section.
pub(super) fn detail_content(
    mouse_model: &gpui::Entity<MouseModelView>,
    dpi_panel: &gpui::Entity<DpiPanel>,
    smartshift_panel: &gpui::Entity<SmartShiftPanel>,
    lighting_panel: &gpui::Entity<LightingPanel>,
    active: DetailTab,
    pal: Palette,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    match active {
        DetailTab::Buttons => buttons_tab(mouse_model).into_any_element(),
        DetailTab::Pointer => pointer_tab(dpi_panel, smartshift_panel, pal, cx).into_any_element(),
        DetailTab::Lighting => lighting_tab(lighting_panel, pal).into_any_element(),
        DetailTab::Device => device_tab(pal, cx).into_any_element(),
    }
}

/// The device's sections as a compact, centred segmented control for the
/// header. Clicking a segment swaps the active section. Only called with more
/// than one tab — see [`detail_header`].
fn detail_tabs(
    tabs: &[DetailTab],
    active: DetailTab,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let active_ix = tabs.iter().position(|t| *t == active).unwrap_or(0);
    // Owned copy so the click handler can map a clicked index back to its tab
    // without borrowing the caller's slice.
    let order = tabs.to_vec();
    TabBar::new("detail-tabs")
        .segmented()
        .selected_index(active_ix)
        .children(tabs.iter().map(|t| t.label()))
        .on_click(cx.listener(move |this, ix: &usize, _, cx| {
            this.active_tab = order.get(*ix).copied().unwrap_or(DetailTab::Device);
            cx.notify();
        }))
}

/// Buttons tab: the mouse model with clickable hotspots, horizontally centred
/// with a max width so it doesn't stretch across a wide window.
///
/// A `v_flex` (top-aligned), like the pointer/device/lighting tabs — *not* an
/// `h_flex`, which carries an implicit `items_center` and would vertically
/// centre the fixed-height model. That left a tall header-to-content gap that
/// collapsed to the top-aligned card tabs on switch — a visible vertical jump.
/// Top-aligning every tab keeps the content's start fixed across switches.
fn buttons_tab(mouse_model: &gpui::Entity<MouseModelView>) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .justify_center()
        .p_6()
        .child(div().w_full().max_w(px(760.)).child(mouse_model.clone()))
}

/// Pointer tab: the DPI panel, the SmartShift wheel controls, and the
/// scroll-wheel preferences, each in a titled card. Use a responsive two-column
/// grid that still fits the window's 720 px minimum width, so these short
/// controls don't force a vertical scroll.
fn pointer_tab(
    dpi_panel: &gpui::Entity<DpiPanel>,
    smartshift_panel: &gpui::Entity<SmartShiftPanel>,
    pal: Palette,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p_5()
        .child(
            h_flex()
                .w_full()
                .max_w(px(920.))
                .items_stretch()
                .gap_4()
                .flex_wrap()
                .child(pointer_grid_card(panel_card_fill(
                    tr!("Pointer tuning"),
                    IconName::Settings,
                    pal,
                    dpi_panel.clone().into_any_element(),
                )))
                .child(pointer_grid_card(panel_card_fill(
                    tr!("SmartShift"),
                    IconName::Settings,
                    pal,
                    smartshift_panel.clone().into_any_element(),
                )))
                .child(
                    div()
                        .min_w(px(332.))
                        .flex_1()
                        .child(scrolling_card(pal, cx)),
                ),
        )
}

fn pointer_grid_card(card: impl IntoElement) -> impl IntoElement {
    // Two cards plus one 16 px gap fit exactly inside the 720 px window minimum
    // after this tab's 20 px side padding, while still leaving a usable slider.
    div().min_w(px(332.)).flex_1().h_full().child(card)
}

/// Scrolling card: per-device native inversion and wheel-resolution controls.
/// Pure config — no hardware read — so it is a plain settings block rather than
/// an `Entity` panel like DPI / SmartShift.
fn scrolling_card(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let (inverted, inversion_supported, resolution, hires_supported) = cx
        .try_global::<AppState>()
        .map_or((false, false, None, false), |state| {
            (
                state.current_invert_scroll(),
                state.current_scroll_inversion_supported(),
                state.current_scroll_resolution(),
                state.current_hires_wheel_supported(),
            )
        });
    let inversion_description = if inversion_supported {
        tr!("Reverse this mouse's scroll wheel. Your trackpad keeps the system scroll direction.")
    } else {
        tr!("This device does not report native HID++ scroll inversion support.")
    };
    let inversion_row = h_flex()
        .justify_between()
        .items_center()
        .gap_4()
        .child(
            v_flex()
                .child(
                    div()
                        .text_sm()
                        .text_color(pal.text_primary)
                        .child(tr!("Invert scroll direction")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(pal.text_muted)
                        .child(inversion_description),
                ),
        )
        .child(invert_scroll_toggle(inverted, inversion_supported, pal));
    let resolution_description = if hires_supported {
        match resolution {
            None => tr!("OpenLogi does not change the wheel resolution."),
            Some(ScrollResolution::Low) => tr!("Scrolls once per physical ratchet step."),
            Some(ScrollResolution::High) => {
                tr!("Detects finer movement between ratchet steps.")
            }
        }
    } else {
        tr!("This device does not support wheel resolution control.")
    };
    let resolution_row = v_flex()
        .gap_2()
        .child(
            v_flex()
                .child(
                    div()
                        .text_sm()
                        .text_color(pal.text_primary)
                        .child(tr!("Wheel resolution")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(pal.text_muted)
                        .child(resolution_description),
                ),
        )
        .child(wheel_resolution_control(resolution, hires_supported, pal));
    panel_card(
        tr!("Scrolling"),
        IconName::Settings,
        pal,
        v_flex()
            .gap_4()
            .child(inversion_row)
            .child(resolution_row)
            .into_any_element(),
    )
}

fn wheel_resolution_control(
    selected: Option<ScrollResolution>,
    enabled: bool,
    pal: Palette,
) -> AnyElement {
    h_flex()
        .w_full()
        .p_1()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(pal.border)
        .child(wheel_resolution_segment(
            "wheel-resolution-default",
            tr!("Device default"),
            None,
            selected,
            enabled,
            pal,
        ))
        .child(wheel_resolution_segment(
            "wheel-resolution-low",
            tr!("Standard"),
            Some(ScrollResolution::Low),
            selected,
            enabled,
            pal,
        ))
        .child(wheel_resolution_segment(
            "wheel-resolution-high",
            tr!("High resolution"),
            Some(ScrollResolution::High),
            selected,
            enabled,
            pal,
        ))
        .into_any_element()
}

fn wheel_resolution_segment(
    id: &'static str,
    label: impl IntoElement,
    value: Option<ScrollResolution>,
    selected: Option<ScrollResolution>,
    enabled: bool,
    pal: Palette,
) -> AnyElement {
    let active = value == selected;
    let segment = div()
        .id(id)
        .flex_1()
        .px_2()
        .py_1()
        .rounded_md()
        .text_center()
        .text_xs()
        .selected_fill(active)
        .text_color(if enabled {
            if active {
                pal.text_primary
            } else {
                pal.text_muted
            }
        } else {
            pal.text_muted
        })
        .child(label);
    if !enabled {
        return segment.into_any_element();
    }
    segment
        .cursor_pointer()
        .on_click(move |_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_scroll_resolution(value);
            });
            cx.refresh_windows();
        })
        .into_any_element()
}

/// On/Off pill that flips the active device's scroll-wheel inversion, mirroring
/// the SmartShift permanent-ratchet toggle.
fn invert_scroll_toggle(on: bool, enabled: bool, pal: Palette) -> AnyElement {
    let label = if on { tr!("On") } else { tr!("Off") };
    if !enabled {
        return div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .text_xs()
            .text_color(pal.text_muted)
            .child(tr!("Unavailable"))
            .into_any_element();
    }
    div()
        .id("invert-scroll-toggle")
        .px_2()
        .py_1()
        .rounded_md()
        .selected_border(on, pal)
        .selected_fill(on)
        .text_xs()
        .text_color(if on { pal.text_primary } else { pal.text_muted })
        .cursor_pointer()
        .child(label)
        .on_click(move |_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_invert_scroll(!on);
            });
            cx.refresh_windows();
        })
        .into_any_element()
}

/// Lighting tab: the RGB controls (swatches, on/off, brightness) in a titled
/// card. Shown when the device reports a lighting capability — see
/// [`DetailTab::tabs_for`].
fn lighting_tab(lighting_panel: &gpui::Entity<LightingPanel>, pal: Palette) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p_6()
        .child(div().w_full().max_w(px(560.)).child(panel_card(
            tr!("Lighting"),
            IconName::Palette,
            pal,
            lighting_panel.clone().into_any_element(),
        )))
}

/// Device tab: device details and configuration cards stacked.
fn device_tab(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p_6()
        .child(
            v_flex()
                .w_full()
                .max_w(px(560.))
                .gap_3()
                .child(device_details_card(pal, cx))
                .child(configuration_card(pal, cx)),
        )
}

fn device_details_card(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let content = cx
        .try_global::<AppState>()
        .and_then(AppState::current_record)
        .cloned()
        .map_or_else(
            || {
                div()
                    .text_sm()
                    .text_color(pal.text_muted)
                    .child(tr!("No active device"))
                    .into_any_element()
            },
            |record| {
                v_flex()
                    .gap_3()
                    .child(device_summary(
                        &record.display_name,
                        record.kind,
                        record.online,
                        pal,
                    ))
                    .when_some(record.battery.as_ref(), |this, battery| {
                        this.child(battery_summary(battery, pal))
                    })
                    .child(device_description_list(record))
                    .into_any_element()
            },
        );

    panel_card(tr!("Device details"), IconName::Info, pal, content)
}

fn configuration_card(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let (binding_count, gesture_count, preset_count, app_profile) = cx
        .try_global::<AppState>()
        .map_or((0, 0, 0, tr!("Default profile").to_string()), |state| {
            (
                state.button_bindings.len(),
                state.gesture_bindings.len(),
                state.dpi_presets().len(),
                state
                    .current_app_bundle
                    .clone()
                    .unwrap_or_else(|| tr!("Default profile").to_string()),
            )
        });

    let content = v_flex()
        .gap_3()
        .child(
            DescriptionList::new()
                .columns(1)
                .label_width(px(118.))
                .bordered(false)
                .child(DescriptionItem::new(tr!("Active profile")).value(app_profile))
                .child(
                    DescriptionItem::new(tr!("Button bindings")).value(binding_count.to_string()),
                )
                .child(
                    DescriptionItem::new(tr!("Gesture bindings")).value(gesture_count.to_string()),
                )
                .child(DescriptionItem::new(tr!("DPI presets")).value(preset_count.to_string())),
        )
        .child(
            h_flex()
                .gap_2()
                .pt_1()
                .child(sidebar_action(
                    "right-panel-settings",
                    IconName::Settings,
                    tr!("Settings"),
                    |_event, _window, cx| crate::windows::settings::open(cx),
                ))
                .child(sidebar_action(
                    "right-panel-config-folder",
                    IconName::Folder,
                    tr!("Config folder"),
                    |_event, _window, cx| {
                        if let Ok(path) = openlogi_core::paths::config_dir()
                            && let Some(url) = file_url(&path)
                        {
                            cx.open_url(&url);
                        }
                    },
                )),
        )
        .into_any_element();

    panel_card(tr!("Configuration"), IconName::Folder, pal, content)
}

fn device_summary(name: &str, kind: DeviceKind, online: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .gap_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(name.to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(pal.text_muted)
                        .child(kind_label(kind)),
                ),
        )
        .child(status_badge(online, pal))
}

fn device_description_list(record: DeviceRecord) -> impl IntoElement {
    let mut items = vec![
        DescriptionItem::new(tr!("Connection")).value(route_label(record.route.as_ref())),
        DescriptionItem::new(tr!("Slot")).value(record.slot.to_string()),
        DescriptionItem::new(tr!("Device key")).value(record.config_key),
    ];
    if let Some(serial) = record.serial_number {
        items.push(DescriptionItem::new(tr!("Serial")).value(serial));
    }

    DescriptionList::new()
        .columns(1)
        .label_width(px(100.))
        .bordered(false)
        .children(items)
}
