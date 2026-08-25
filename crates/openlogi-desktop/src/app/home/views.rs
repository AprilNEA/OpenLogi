//! Switchable grid, list, and carousel layouts for the Home device gallery.

use gpui::{
    AnyElement, Context, ElementId, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    IconName, Sizable as _,
    button::{Toggle, ToggleVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use openlogi_core::config::DeviceViewMode;
use openlogi_core::device::DeviceKind;

use super::{
    AppView, battery_view, connection_summary, connection_view, device_card,
    device_identity_subtitle, device_image, device_ring, keyboard_glow, rename_device_button,
};
use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::carousel::Carousel;
use crate::ui::theme::{self, ContentWidth, Palette, SelectableStyle as _, Typography as _};

pub(super) fn device_view_switcher(
    current: DeviceViewMode,
    view: gpui::Entity<AppView>,
) -> impl IntoElement {
    let toggle =
        move |id: &'static str, icon: IconName, tooltip: SharedString, mode: DeviceViewMode| {
            let view = view.clone();
            Toggle::new(id)
                .icon(icon)
                .tooltip(tooltip)
                .checked(current == mode)
                .outline()
                .small()
                .on_click(move |checked, _, cx| {
                    if !checked {
                        return;
                    }
                    AppState::update(cx, |state, cx| {
                        state.set_device_view_mode(mode);
                        cx.emit(StateEvent::SettingsChanged);
                    });
                    view.update(cx, |_, cx| cx.notify());
                })
        };

    h_flex().gap_1().children([
        toggle(
            "device-view-grid",
            IconName::LayoutDashboard,
            tr!("Grid"),
            DeviceViewMode::Grid,
        ),
        toggle(
            "device-view-list",
            IconName::Menu,
            tr!("List"),
            DeviceViewMode::List,
        ),
        toggle(
            "device-view-carousel",
            IconName::GalleryVerticalEnd,
            tr!("Carousel"),
            DeviceViewMode::Carousel,
        ),
    ])
}

/// Gap between gallery cards, in pixels.
const GALLERY_GAP: f32 = 16.;
/// Fixed carousel width: three cards plus navigation fit the default window.
const CAROUSEL_CARD_W: f32 = 340.;
/// Maximum width of the grid: three cards at their maximum width plus gaps.
const GALLERY_MAX_W: f32 = theme::GALLERY_CARD_MAX_W * 3. + GALLERY_GAP * 2.;

/// Render the persisted Home layout. All three modes share ordering, metadata,
/// accessibility, and active-device semantics; only spatial density changes.
pub(in crate::app) fn device_gallery(cx: &mut Context<AppView>) -> AnyElement {
    let mode = AppState::try_read(cx).map_or(DeviceViewMode::Grid, |state| {
        state.app_settings().device_view_mode
    });
    match mode {
        DeviceViewMode::Grid => device_grid(cx),
        DeviceViewMode::List => device_list(cx),
        DeviceViewMode::Carousel => device_carousel(cx),
    }
}

/// Connected devices first, preserving AppState's deterministic route order
/// within the connected and offline groups.
pub(in crate::app) fn ordered_device_indices(records: &[DeviceRecord]) -> Vec<usize> {
    let mut indices: Vec<_> = (0..records.len()).collect();
    indices.sort_by_key(|index| !records[*index].online);
    indices
}

fn device_grid(cx: &mut Context<AppView>) -> AnyElement {
    let view = cx.entity();
    let pal = theme::palette(cx);
    let cards = AppState::try_read(cx).map_or_else(Vec::new, |state| {
        let active_idx = state
            .current_device
            .min(state.device_list.len().saturating_sub(1));
        ordered_device_indices(&state.device_list)
            .into_iter()
            .map(|idx| {
                device_card_element(state, idx, active_idx, view.clone(), pal)
                    .min_w(px(theme::GALLERY_CARD_MIN_W))
                    .max_w(px(theme::GALLERY_CARD_MAX_W))
                    .flex_1()
                    .into_any_element()
            })
            .collect()
    });

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
                .max_w(px(GALLERY_MAX_W))
                .items_stretch()
                .flex_wrap()
                .gap(px(GALLERY_GAP))
                .children(cards),
        )
        .into_any_element()
}

fn device_list(cx: &mut Context<AppView>) -> AnyElement {
    let view = cx.entity();
    let pal = theme::palette(cx);
    let rows = AppState::try_read(cx).map_or_else(Vec::new, |state| {
        let active_idx = state
            .current_device
            .min(state.device_list.len().saturating_sub(1));
        ordered_device_indices(&state.device_list)
            .into_iter()
            .map(|idx| device_list_row(state, idx, active_idx, view.clone(), pal))
            .collect()
    });

    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p_4()
        .child(
            v_flex()
                .w_full()
                .max_w(ContentWidth::ExtraLarge.rems())
                .gap_2()
                .children(rows),
        )
        .into_any_element()
}

fn device_carousel(cx: &mut Context<AppView>) -> AnyElement {
    let view = cx.entity();
    let (order, selected) = AppState::try_read(cx).map_or_else(
        || (Vec::new(), 0),
        |state| {
            let active_idx = state
                .current_device
                .min(state.device_list.len().saturating_sub(1));
            let order = ordered_device_indices(&state.device_list);
            let selected = order.iter().position(|idx| *idx == active_idx).unwrap_or(0);
            (order, selected)
        },
    );
    let render_order = order.clone();
    let select_order = order.clone();

    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .child(
            Carousel::new("device-carousel", px(CAROUSEL_CARD_W))
                .len(order.len())
                .selected(selected)
                .gap(px(GALLERY_GAP))
                .render_item(move |position, _, _, cx| {
                    let pal = theme::palette(cx);
                    let Some(state) = AppState::try_read(cx) else {
                        return div().into_any_element();
                    };
                    let Some(&idx) = render_order.get(position) else {
                        return div().into_any_element();
                    };
                    let active_idx = state
                        .current_device
                        .min(state.device_list.len().saturating_sub(1));
                    device_card_element(state, idx, active_idx, view.clone(), pal)
                        .into_any_element()
                })
                .on_select(cx.listener(move |_, position: &usize, _, cx| {
                    let Some(&idx) = select_order.get(*position) else {
                        return;
                    };
                    AppState::global(cx).update(cx, |state, cx| {
                        if state.current_device == idx || idx >= state.device_list.len() {
                            return;
                        }
                        state.set_current_device(idx);
                        cx.emit(StateEvent::DeviceSelected(
                            state.device_list[idx].device_key(),
                        ));
                    });
                    AppState::load_current_device_reads(cx);
                })),
        )
        .into_any_element()
}

fn device_card_element(
    state: &AppState,
    idx: usize,
    active_idx: usize,
    view: gpui::Entity<AppView>,
    pal: Palette,
) -> BaseButton {
    let record = &state.device_list[idx];
    let active = idx == active_idx;
    let record_key = record.record_key();
    let enabled = state.device_enabled(&record.config_key);
    let light_enabled =
        record.kind == DeviceKind::Light && state.light_enabled_for(&record.device_key());
    let light_settings = state.light_for(&record.device_key());
    let glow = keyboard_glow(state, record);

    device_card(
        record,
        enabled,
        active,
        glow,
        light_enabled,
        light_settings,
        pal,
    )
    .active(gpui::Styled::shadow_2xs)
    .accessibility_label(record.display_name.clone())
    .aria_description(device_accessibility_description(record))
    .aria_selected(active)
    .cursor_pointer()
    .hover(move |card| card.border_color(rgb(theme::ACCENT_BLUE)).shadow_sm())
    .focus_visible(move |card| card.border_color(rgb(theme::ACCENT_BLUE)).shadow_sm())
    .on_click(move |_, _, cx| {
        view.update(cx, |this, cx| {
            this.open_device(record_key.clone(), cx);
        });
    })
}

fn device_list_row(
    state: &AppState,
    idx: usize,
    active_idx: usize,
    view: gpui::Entity<AppView>,
    pal: Palette,
) -> AnyElement {
    let record = &state.device_list[idx];
    let active = idx == active_idx;
    let enabled = state.device_enabled(&record.config_key);
    let light_enabled =
        record.kind == DeviceKind::Light && state.light_enabled_for(&record.device_key());
    let light_settings = state.light_for(&record.device_key());
    let glow = keyboard_glow(state, record);
    let record_key = record.record_key();

    BaseButton::new((ElementId::from("device-list-row"), record_key.clone()))
        .w_full()
        .min_h(px(96.))
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .rounded(pal.card_radius)
        .border_1()
        .border_color(device_ring(enabled, active))
        .bg(pal.panel)
        .shadow_xs()
        .selected_fill(active)
        .child(
            div()
                .relative()
                .w(px(120.))
                .h(px(68.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .opacity(if record.online { 1. } else { 0.38 })
                .when_some(glow, |this, (geom, color)| {
                    this.child(super::glow_canvas(geom, color))
                })
                .child(device_image(record, light_enabled, light_settings, pal)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1p5()
                .child(
                    div()
                        .truncate()
                        .text_subheading()
                        .child(record.display_name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(device_identity_subtitle(record)),
                )
                .child(connection_view(record, pal)),
        )
        .child(
            v_flex()
                .min_w(px(132.))
                .flex_none()
                .items_end()
                .gap_2()
                .when(active, |this| {
                    this.child(
                        div()
                            .text_caption()
                            .text_color(theme::accent())
                            .child(tr!("Active device")),
                    )
                })
                .when_some(record.battery.as_ref(), |this, battery| {
                    this.child(battery_view(battery, record.online, pal))
                })
                .when(record.persistent, |this| {
                    this.child(rename_device_button(record, pal))
                }),
        )
        .active(gpui::Styled::shadow_2xs)
        .accessibility_label(record.display_name.clone())
        .aria_description(device_accessibility_description(record))
        .aria_selected(active)
        .cursor_pointer()
        .hover(move |row| row.border_color(rgb(theme::ACCENT_BLUE)).shadow_sm())
        .focus_visible(move |row| row.border_color(rgb(theme::ACCENT_BLUE)).shadow_sm())
        .on_click(move |_, _, cx| {
            view.update(cx, |this, cx| {
                this.open_device(record_key.clone(), cx);
            });
        })
        .into_any_element()
}

fn device_accessibility_description(record: &DeviceRecord) -> SharedString {
    let status = if record.online {
        tr!("Connected")
    } else {
        tr!("Offline")
    };
    let identity = if record.display_name == record.model_name {
        super::kind_label(record.kind)
    } else {
        format!("{}. {}", record.model_name, super::kind_label(record.kind))
    };
    let metadata = format!("{status}. {identity}. {}.", connection_summary(record));
    if let Some(battery) = record.battery.as_ref() {
        format!("{metadata} {} {}%.", tr!("Battery"), battery.percentage).into()
    } else {
        metadata.into()
    }
}
