//! The device-detail screen: the header (back + name + section tabs), and the
//! section bodies (Buttons, Pointer, Lighting, Camera, Device).

use gpui::{
    AnyElement, BorrowAppContext as _, Context, IntoElement, ParentElement, SharedString, Styled,
    div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Icon, IconName, Selectable as _,
    button::{Button, ButtonGroup},
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
use crate::components::camera_controls::CameraControlsPanel;
use crate::components::camera_preview::CameraPreview;
use crate::components::dpi_panel::DpiPanel;
use crate::components::lighting_panel::LightingPanel;
use crate::components::smartshift_panel::SmartShiftPanel;
use crate::mouse_model::view::MouseModelView;
use crate::state::{AppState, DeviceRecord};
use crate::theme::{HEADER_H, Palette, SCREEN_PAD, Typography as _};

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
                .max_w(px(220.))
                .truncate()
                .text_heading()
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
// One entity per detail panel plus the active tab, palette, and context — a flat
// dispatcher rather than a context struct that would only ever be built here.
#[allow(clippy::too_many_arguments)]
pub(super) fn detail_content(
    mouse_model: &gpui::Entity<MouseModelView>,
    dpi_panel: &gpui::Entity<DpiPanel>,
    smartshift_panel: &gpui::Entity<SmartShiftPanel>,
    lighting_panel: &gpui::Entity<LightingPanel>,
    camera_preview: &gpui::Entity<CameraPreview>,
    camera_controls: &gpui::Entity<CameraControlsPanel>,
    active: DetailTab,
    pal: Palette,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let online = cx
        .try_global::<AppState>()
        .and_then(AppState::current_record)
        .is_some_and(|record| record.online);
    let content = match active {
        DetailTab::Buttons => buttons_tab(mouse_model).into_any_element(),
        DetailTab::Pointer => pointer_tab(dpi_panel, smartshift_panel, pal, cx).into_any_element(),
        DetailTab::Lighting => lighting_tab(lighting_panel, pal).into_any_element(),
        DetailTab::Camera => camera_tab(camera_preview, camera_controls, pal).into_any_element(),
        DetailTab::Device => device_tab(pal, cx).into_any_element(),
    };
    v_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .when(!online, |this| {
            this.child(
                h_flex()
                    .flex_shrink_0()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(pal.border)
                    .bg(pal.surface)
                    .px_5()
                    .py_2()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(Icon::new(IconName::Info).size_4())
                    .child(tr!(
                        "Device offline — changes will apply when it reconnects."
                    )),
            )
        })
        .child(content)
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
        .p(px(SCREEN_PAD))
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
        .p(px(SCREEN_PAD))
        .child(
            h_flex()
                .w_full()
                .max_w(px(920.))
                .items_stretch()
                .gap_4()
                .flex_wrap()
                .child(pointer_grid_card(panel_card_fill(
                    tr!("Pointer tuning"),
                    Icon::empty().path("action-icons/gauge.svg"),
                    pal,
                    dpi_panel.clone().into_any_element(),
                )))
                .child(pointer_grid_card(panel_card_fill(
                    tr!("SmartShift"),
                    Icon::empty().path("action-icons/refresh-cw.svg"),
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
    // after this tab's `SCREEN_PAD` (20 px) side inset, while still leaving a
    // usable slider: 332·2 + 16 + 20·2 = 720.
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
                        .text_body()
                        .text_color(pal.text_primary)
                        .child(tr!("Invert scroll direction")),
                )
                .child(
                    div()
                        .text_caption()
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
                        .text_body()
                        .text_color(pal.text_primary)
                        .child(tr!("Wheel resolution")),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(resolution_description),
                ),
        )
        .child(wheel_resolution_control(resolution, hires_supported, pal));
    panel_card(
        tr!("Scrolling"),
        Icon::empty().path("action-icons/mouse.svg"),
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
    _pal: Palette,
) -> AnyElement {
    let values = [
        None,
        Some(ScrollResolution::Low),
        Some(ScrollResolution::High),
    ];
    ButtonGroup::new("wheel-resolution")
        .w_full()
        .outline()
        .disabled(!enabled)
        .child(
            Button::new("wheel-resolution-default")
                .flex_1()
                .label(tr!("Device default"))
                .selected(selected.is_none()),
        )
        .child(
            Button::new("wheel-resolution-low")
                .flex_1()
                .label(tr!("Standard"))
                .selected(selected == Some(ScrollResolution::Low)),
        )
        .child(
            Button::new("wheel-resolution-high")
                .flex_1()
                .label(tr!("High resolution"))
                .selected(selected == Some(ScrollResolution::High)),
        )
        .on_click(move |indices, _window, cx| {
            let Some(value) = indices.first().and_then(|index| values.get(*index)) else {
                return;
            };
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_scroll_resolution(*value);
            });
            cx.refresh_windows();
        })
        .into_any_element()
}

/// On/Off pill that flips the active device's scroll-wheel inversion, mirroring
/// the SmartShift permanent-ratchet toggle.
fn invert_scroll_toggle(on: bool, enabled: bool, pal: Palette) -> AnyElement {
    let label: SharedString = if on { tr!("On") } else { tr!("Off") };
    if !enabled {
        return div()
            .px_2()
            .py_1()
            .rounded(pal.control_radius)
            .border_1()
            .border_color(pal.border)
            .text_caption()
            .text_color(pal.text_muted)
            .child(tr!("Unavailable"))
            .into_any_element();
    }
    Button::new("invert-scroll-toggle")
        .compact()
        .label(label)
        .selected(on)
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
        .p(px(SCREEN_PAD))
        .child(div().w_full().max_w(px(560.)).child(panel_card(
            tr!("Lighting"),
            Icon::new(IconName::Palette),
            pal,
            lighting_panel.clone().into_any_element(),
        )))
}

/// Camera tab: the live webcam preview beside the device-level image controls,
/// each in a titled card. Side by side at the default window width so every
/// control is visible without scrolling; the cards wrap to a stacked column
/// when the window is too narrow. The preview drives the capture session via
/// [`CameraPreview::set_target`] (called from [`AppView::render`]); the controls
/// panel reads/writes UVC settings directly on the device.
fn camera_tab(
    camera_preview: &gpui::Entity<CameraPreview>,
    camera_controls: &gpui::Entity<CameraControlsPanel>,
    pal: Palette,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p_6()
        .child(
            h_flex()
                .w_full()
                .flex_wrap()
                .justify_center()
                .items_start()
                .gap_3()
                .child(div().w(px(514.)).flex_shrink_0().child(panel_card(
                    tr!("Camera"),
                    Icon::new(IconName::Eye),
                    pal,
                    camera_preview.clone().into_any_element(),
                )))
                .child(div().w(px(500.)).flex_shrink_0().child(panel_card(
                    tr!("Camera controls"),
                    Icon::new(IconName::Settings),
                    pal,
                    camera_controls.clone().into_any_element(),
                ))),
        )
}

/// Device tab: device details and configuration cards stacked.
fn device_tab(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p(px(SCREEN_PAD))
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
                    .text_body()
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

    panel_card(
        tr!("Device details"),
        Icon::new(IconName::Info),
        pal,
        content,
    )
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

    panel_card(
        tr!("Configuration"),
        Icon::new(IconName::Folder),
        pal,
        content,
    )
}

fn device_summary(name: &str, kind: DeviceKind, online: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .gap_1()
                .min_w_0()
                .child(div().text_subheading().child(name.to_string()))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(kind_label(kind)),
                ),
        )
        .child(status_badge(online, pal))
}

fn device_description_list(record: DeviceRecord) -> impl IntoElement {
    // Cameras are plain UVC over the cable — no HID++ route, and their slot is
    // a synthetic 0 that would only mislead next to real receiver slots.
    let is_camera = matches!(record.kind, DeviceKind::Camera);
    let connection = if is_camera {
        tr!("USB").to_string()
    } else {
        route_label(record.route.as_ref())
    };
    let mut items = vec![DescriptionItem::new(tr!("Connection")).value(connection)];
    if !is_camera {
        items.push(DescriptionItem::new(tr!("Slot")).value(record.slot.to_string()));
    }
    items.push(DescriptionItem::new(tr!("Device key")).value(elided_key(&record.config_key)));
    if let Some(serial) = record.serial_number {
        items.push(DescriptionItem::new(tr!("Serial")).value(serial));
    }

    DescriptionList::new()
        .columns(1)
        .label_width(px(100.))
        .bordered(false)
        .children(items)
}

/// Show long machine keys (a camera's config key embeds the OS device path)
/// as head…tail instead of wrapping the details card; short HID++ keys pass
/// through whole. The full key stays in the config file for copying.
fn elided_key(key: &str) -> String {
    const HEAD: usize = 40;
    const TAIL: usize = 8;
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= HEAD + TAIL + 1 {
        return key.to_string();
    }
    let head: String = chars[..HEAD].iter().collect();
    let tail: String = chars[chars.len() - TAIL..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::elided_key;

    #[test]
    fn short_hid_keys_pass_through_whole() {
        let key = "direct:046d:b023:unit:a393cae0";
        assert_eq!(elided_key(key), key);
    }

    #[test]
    fn long_camera_keys_show_head_and_tail() {
        let key = r"camera-\?\usb#vid_046d&pid_0893&mi_00#9&56d9c30&0&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";
        let shown = elided_key(key);
        assert!(shown.contains('…'));
        assert!(shown.starts_with(r"camera-\?\usb#vid_046d&pid_0893"));
        assert!(shown.ends_with(r"}\global"));
        assert!(shown.chars().count() < 55);
    }

    #[test]
    fn exactly_at_the_threshold_is_not_elided() {
        let key = "k".repeat(49);
        assert_eq!(elided_key(&key), key);
    }
}
