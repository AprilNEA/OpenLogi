//! Switchable grid, list, and carousel layouts for the Home device gallery.

use gpui::{
    AnyElement, App, Context, ElementId, InteractiveElement, IntoElement, ParentElement, Pixels,
    Rems, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px, rems, rgb,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonGroup},
    h_flex,
    menu::ContextMenuExt as _,
    scroll::ScrollableElement as _,
    v_flex,
};
use openlogi_core::config::DeviceViewMode;
use openlogi_core::device::DeviceKind;

use super::{
    AppView, connection_summary, connection_view, custom_model_subtitle, device_card, device_image,
    device_menu, device_menu_button, device_ring, keyboard_glow, kind_badge,
};
use crate::state::{AppState, DeviceRecord};
use crate::ui::battery::{BatteryIndicator, battery_charging_no_reading, value_label};
use crate::ui::carousel::Carousel;
use crate::ui::theme::{self, ContentWidth, Palette, SelectableStyle as _, Typography as _};

pub(super) fn device_view_switcher(
    current: DeviceViewMode,
    view: gpui::Entity<AppView>,
) -> impl IntoElement {
    /// Grid / list / carousel as one segmented control. The chosen segment
    /// takes `SelectableStyle::selected_segment` like the app's other
    /// segmented controls, which reads the theme at render time. Not a stock
    /// `Toggle`: it hovers with the fill it marks its checked state with, so a
    /// hovered mode would look selected too.
    #[derive(IntoElement)]
    struct Switcher {
        current: DeviceViewMode,
        view: gpui::Entity<AppView>,
    }

    impl RenderOnce for Switcher {
        fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
            const MODES: [DeviceViewMode; 3] = [
                DeviceViewMode::Grid,
                DeviceViewMode::List,
                DeviceViewMode::Carousel,
            ];
            let Self { current, view } = self;
            let segment = |mode: DeviceViewMode| {
                let (id, icon, tooltip) = match mode {
                    DeviceViewMode::Grid => (
                        "device-view-grid",
                        IconName::LayoutDashboard,
                        tr!("common.grid"),
                    ),
                    DeviceViewMode::List => {
                        ("device-view-list", IconName::Menu, tr!("common.list"))
                    }
                    DeviceViewMode::Carousel => (
                        "device-view-carousel",
                        IconName::GalleryVerticalEnd,
                        tr!("common.carousel"),
                    ),
                };
                let selected = current == mode;
                Button::new(id)
                    .icon(icon)
                    .accessibility_label(tooltip.clone())
                    .tooltip(tooltip)
                    .selected(selected)
                    .selected_segment(selected, cx)
            };

            ButtonGroup::new("device-view")
                .outline()
                .small()
                .children(MODES.map(segment))
                .on_click(move |clicks, _, cx| {
                    let Some(&mode) = clicks.first().and_then(|&index| MODES.get(index)) else {
                        return;
                    };
                    if mode == current {
                        return;
                    }
                    AppState::apply(cx, |state| state.commit_device_view_mode(mode));
                    view.update(cx, |_, cx| cx.notify());
                })
        }
    }

    Switcher { current, view }
}

/// Gap between gallery cards.
const GALLERY_GAP: Rems = rems(1.);
/// Inset of the grid from the window edges.
const GALLERY_PAD: Rems = rems(1.5);
/// Most cards a grid row holds; a wider window centres the grid instead.
const GALLERY_MAX_COLUMNS: u16 = 3;
/// Fixed carousel width: three cards plus navigation fit the default window.
const CAROUSEL_CARD_W: Rems = rems(21.25);

/// Render the persisted Home layout. All three modes share ordering, metadata,
/// accessibility, and active-device semantics; only spatial density changes.
pub(in crate::app) fn device_gallery(window: &Window, cx: &mut Context<AppView>) -> AnyElement {
    let mode = AppState::try_read(cx).map_or(DeviceViewMode::Grid, |state| {
        state.app_settings().device_view_mode
    });
    match mode {
        DeviceViewMode::Grid => device_grid(window, cx).into_any_element(),
        DeviceViewMode::List => device_list(cx).into_any_element(),
        DeviceViewMode::Carousel => device_carousel(cx).into_any_element(),
    }
}

/// Connected devices first, preserving AppState's deterministic route order
/// within the connected and offline groups.
pub(in crate::app) fn ordered_device_indices(records: &[DeviceRecord]) -> Vec<usize> {
    let mut indices: Vec<_> = (0..records.len()).collect();
    indices.sort_by_key(|index| !records[*index].online);
    indices
}

/// Columns of the Home grid across `width`: as many minimum-width cards as
/// fit, capped at [`GALLERY_MAX_COLUMNS`], and never none.
fn grid_columns(width: Pixels, rem: Pixels) -> u16 {
    let card = theme::GALLERY_CARD_MIN_W.to_pixels(rem);
    let gap = GALLERY_GAP.to_pixels(rem);
    (2..=GALLERY_MAX_COLUMNS)
        .rev()
        .find(|&n| card * f32::from(n) + gap * f32::from(n - 1) <= width)
        .unwrap_or(1)
}

/// Equal-width columns for every row, so a short last row lines up under the
/// rows above instead of stretching its cards. The column count follows the
/// window, which the gallery spans; the grid stops growing once its cards
/// reach their maximum width.
fn device_grid(window: &Window, cx: &mut Context<AppView>) -> impl IntoElement {
    let view = cx.entity();
    let pal = theme::palette(cx);
    let rem = window.rem_size();
    let columns = grid_columns(
        window.viewport_size().width - GALLERY_PAD.to_pixels(rem) * 2.,
        rem,
    );
    let max_w = rems(
        f32::from(columns) * theme::GALLERY_CARD_MAX_W.0 + f32::from(columns - 1) * GALLERY_GAP.0,
    );
    let cards = AppState::try_read(cx).map_or_else(Vec::new, |state| {
        let active_idx = state.selected_device_index().unwrap_or(0);
        ordered_device_indices(state.devices())
            .into_iter()
            .enumerate()
            .map(|(position, idx)| {
                device_card_element(state, idx, active_idx, view.clone(), pal)
                    .debug_selector(move || format!("home-grid-card-{position}"))
                    .context_menu(device_menu(&state.devices()[idx]))
            })
            .collect()
    });

    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p(GALLERY_PAD)
        .child(
            div()
                .grid()
                .grid_cols(columns)
                .w_full()
                .max_w(max_w)
                .gap(GALLERY_GAP)
                .children(cards),
        )
}

fn device_list(cx: &mut Context<AppView>) -> impl IntoElement {
    let view = cx.entity();
    let pal = theme::palette(cx);
    let rows = AppState::try_read(cx).map_or_else(Vec::new, |state| {
        let active_idx = state.selected_device_index().unwrap_or(0);
        ordered_device_indices(state.devices())
            .into_iter()
            .map(|idx| {
                device_list_row(state, idx, active_idx, view.clone(), pal)
                    .context_menu(device_menu(&state.devices()[idx]))
            })
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
}

fn device_carousel(cx: &mut Context<AppView>) -> impl IntoElement {
    let view = cx.entity();
    let (order, selected) = AppState::try_read(cx).map_or_else(
        || (Vec::new(), 0),
        |state| {
            let active_idx = state.selected_device_index().unwrap_or(0);
            let order = ordered_device_indices(state.devices());
            let selected = order.iter().position(|idx| *idx == active_idx).unwrap_or(0);
            (order, selected)
        },
    );
    let render_order = order.clone();
    let select_order = order.clone();

    // Top-aligned like the grid: the row's own vertical padding brings the
    // cards level with the grid's inset.
    v_flex().flex_1().w_full().min_h_0().pt_2().child(
        Carousel::new("device-carousel", CAROUSEL_CARD_W)
            .len(order.len())
            .selected(selected)
            .gap(GALLERY_GAP)
            .render_item(move |position, _, _, cx| {
                let pal = theme::palette(cx);
                let Some(state) = AppState::try_read(cx) else {
                    return div().into_any_element();
                };
                let Some(&idx) = render_order.get(position) else {
                    return div().into_any_element();
                };
                let active_idx = state.selected_device_index().unwrap_or(0);
                device_card_element(state, idx, active_idx, view.clone(), pal)
                    .context_menu(device_menu(&state.devices()[idx]))
                    .into_any_element()
            })
            .on_select(cx.listener(move |_, position: &usize, _, cx| {
                let Some(&idx) = select_order.get(*position) else {
                    return;
                };
                AppState::apply(cx, |state| state.select_device(idx));
                AppState::load_current_device_reads(cx);
            })),
    )
}

fn device_card_element(
    state: &AppState,
    idx: usize,
    active_idx: usize,
    view: gpui::Entity<AppView>,
    pal: Palette,
) -> BaseButton {
    let record = &state.devices()[idx];
    let active = idx == active_idx;
    let record_key = record.record_key();
    let enabled = state.device_enabled(&record.config_key);
    let light_enabled =
        record.kind == DeviceKind::Light && state.light_enabled_for(&record.device_key());
    let light_settings = state.light_for(&record.device_key());
    let glow = keyboard_glow(state, record);

    device_card(
        record,
        state.offline_connection(record),
        enabled,
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
) -> BaseButton {
    let record = &state.devices()[idx];
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
        .border_color(device_ring(enabled))
        .bg(pal.panel)
        .shadow_xs()
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
                    h_flex()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .truncate()
                                .text_subheading()
                                .child(record.display_name.clone()),
                        )
                        .child(kind_badge(record.kind, pal)),
                )
                .when_some(custom_model_subtitle(record), |column, model| {
                    column.child(
                        div()
                            .truncate()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(model),
                    )
                })
                .child(connection_view(
                    record,
                    pal,
                    state.offline_connection(record),
                )),
        )
        .child(
            v_flex()
                .min_w(rems(8.25))
                .flex_none()
                .items_end()
                .gap_2()
                .when_some(record.battery.as_ref(), |this, battery| {
                    this.child(BatteryIndicator::status(battery, record.online))
                })
                .when(record.persistent, |this| {
                    this.child(device_menu_button(record, pal))
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
}

fn device_accessibility_description(record: &DeviceRecord) -> SharedString {
    let status = if record.online {
        tr!("device.connected")
    } else {
        tr!("device.offline")
    };
    let identity = if record.display_name == record.model_name {
        super::kind_label(record.kind)
    } else {
        format!("{}. {}", record.model_name, super::kind_label(record.kind))
    };
    let metadata = format!("{status}. {identity}. {}.", connection_summary(record));
    if let Some(battery) = record.battery.as_ref() {
        let battery = if battery_charging_no_reading(battery) {
            tr!("device.charging").to_string()
        } else if record.online {
            format!("{} {}", tr!("device.battery"), value_label(battery))
        } else {
            format!(
                "{} {}",
                tr!("device.last_known_battery"),
                value_label(battery)
            )
        };
        format!("{metadata} {battery}.").into()
    } else {
        metadata.into()
    }
}
