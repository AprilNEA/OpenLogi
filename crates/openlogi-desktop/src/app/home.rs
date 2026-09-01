//! The Home (device gallery) screen: its top bar, switchable grid/list/carousel
//! layouts, and the loading/empty states shown before the agent reports an
//! inventory.

mod views;

pub(super) use views::device_gallery;
#[cfg(test)]
pub(super) use views::ordered_device_indices;

use std::sync::Arc;

use gpui::{
    Anchor, AnyElement, App, AppContext as _, Context, Div, ElementId, Hsla, InteractiveElement,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement as _, Styled, Window,
    canvas, div, fill, img, point, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::DialogButtonProps,
    h_flex,
    input::InputState,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    tooltip::Tooltip,
    v_flex,
};
use openlogi_core::config::{DeviceViewMode, LightSettings};
use openlogi_core::device::{DeviceKind, DeviceModelInfo};
use openlogi_core::diagnostics::ConnectionKind;
use openlogi_core::hid::{DeviceRoute, ReceiverBrand};

use super::AppView;
use super::status::{loading_body, notice_body};
use super::widgets::{
    add_device_button, connectivity_dot, kind_label, route_label, settings_button,
};
use crate::features::lighting::visual as light_visual;
use crate::services::assets::GlowGeometry;
use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::battery::{BatteryIndicator, glance_hint};
use crate::ui::components::control_input;
use crate::ui::theme::{self, ContentWidth, HEADER_H, Palette, Typography as _};

/// Home (gallery) top bar: title/count, the persisted layout switcher, Settings,
/// and Add Device.
pub(super) fn home_header(cx: &mut Context<AppView>) -> impl IntoElement {
    let pal = theme::palette(cx);
    let device_count = AppState::try_read(cx).map_or(0, |state| state.devices().len());
    let current_mode = AppState::try_read(cx).map_or(DeviceViewMode::Grid, |state| {
        state.app_settings().device_view_mode
    });
    let view = cx.entity();
    let device_count_label = if device_count == 1 {
        tr!("device.device_count_singular", count => device_count)
    } else {
        tr!("device.device_count_plural", count => device_count)
    };
    h_flex()
        .h(px(HEADER_H))
        .w_full()
        .px_5()
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(pal.border)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_heading().child(tr!("device.devices")))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(device_count_label),
                ),
        )
        .child(views::device_view_switcher(current_mode, view))
        .child(settings_button())
        .child(add_device_button())
}

/// Opacity the lighting colour is painted at over the device image, in both the
/// home gallery and the device-detail model.
const GLOW_OPACITY: f32 = 0.6;

/// The inter-key glow geometry and tinted colour for `record`, or `None` unless
/// it's a keyboard with lighting enabled and a depot that ships a baked mask.
/// The geometry is painted live by [`glow_canvas`] — no pre-rendered PNG, so a
/// colour change costs no new texture.
pub(crate) fn keyboard_glow(
    state: &AppState,
    record: &DeviceRecord,
) -> Option<(Arc<GlowGeometry>, Hsla)> {
    if record.kind != DeviceKind::Keyboard {
        return None;
    }
    let lighting = state
        .lighting_for(&record.config_key, &record.route_key)
        .filter(|l| l.enabled)?;
    let geom = record.asset.as_ref()?.glow.clone()?;
    let (r, g, b) = lighting.color.components();
    let color = gpui::Rgba {
        r: f32::from(r) / 255.,
        g: f32::from(g) / 255.,
        b: f32::from(b) / 255.,
        a: GLOW_OPACITY,
    };
    Some((geom, color.into()))
}

/// Paint a keyboard's baked inter-key holes in its lighting colour, scaled with
/// a contain-fit so the holes register with the keys at any render size. A
/// `canvas` of tinted quads — no pre-rendered PNG and no per-colour texture, so
/// the runtime footprint is just the depot's small segment list (#272).
pub(crate) fn glow_canvas(geom: Arc<GlowGeometry>, color: Hsla) -> impl IntoElement {
    canvas(
        move |_, _, _| (geom, color),
        move |bounds, (geom, color), window, _| {
            let bw = f32::from(bounds.size.width);
            let bh = f32::from(bounds.size.height);
            if bw <= 0. || bh <= 0. {
                return;
            }
            // Contain-fit a `geom.aspect` box inside the bounds, matching the
            // device image's object-fit so the holes line up with the keys.
            let (rw, rh) = if bw / bh > geom.aspect {
                (bh * geom.aspect, bh)
            } else {
                (bw, bw / geom.aspect)
            };
            let ox = f32::from(bounds.origin.x) + (bw - rw) / 2.;
            let oy = f32::from(bounds.origin.y) + (bh - rh) / 2.;
            for s in &geom.segments {
                let quad = gpui::Bounds {
                    origin: point(px(ox + s.x * rw), px(oy + s.y * rh)),
                    size: gpui::size(px((s.w * rw).max(1.)), px((s.h * rh).max(1.))),
                };
                window.paint_quad(fill(quad, color));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// A device card in the Home grid and carousel: product image with the
/// connectivity-and-battery glance in its corner, then the identity line and
/// the transport it is reachable over.
/// The `active` device keeps a persistent accent ring and faint fill; inactive
/// cards gain the same ring on hover or keyboard focus.
/// Returns an unstyled semantic button so the gallery can add its activation
/// handler without giving up keyboard behavior.
fn device_card(
    record: &DeviceRecord,
    enabled: bool,
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
    light_enabled: bool,
    light_settings: LightSettings,
    pal: Palette,
) -> BaseButton {
    BaseButton::new((ElementId::from("device-card"), record.record_key()))
        .w_full()
        .flex()
        .flex_col()
        .items_stretch()
        .gap_3()
        .p_4()
        .rounded(pal.card_radius)
        .border_1()
        .border_color(device_ring(enabled))
        .bg(pal.panel)
        .shadow_xs()
        .child(
            div()
                .relative()
                .w_full()
                .h(px(theme::GALLERY_PHOTO_H))
                .child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .overflow_hidden()
                        // The green hardware LED is baked into several product
                        // renders. Dimming the complete render is the only
                        // truthful treatment available for an offline card
                        // without generating a second asset that edits the
                        // manufacturer's artwork.
                        .opacity(if record.online { 1. } else { 0.38 })
                        .when_some(glow, |this, (geom, color)| {
                            this.child(glow_canvas(geom, color))
                        })
                        .child(device_image(record, light_enabled, light_settings, pal)),
                )
                // Outside the dimmed render, so an offline card's status
                // stays legible.
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .child(transport_glance(record, pal)),
                )
                .when_some(record.battery.as_ref(), |image, battery| {
                    image.child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .child(battery_glance(battery, record)),
                    )
                }),
        )
        .child(
            v_flex().w_full().gap_2().child(
                h_flex()
                    .w_full()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
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
                            }),
                    )
                    .when(record.persistent, |row| {
                        row.child(device_menu_button(record, pal))
                    }),
            ),
        )
}

/// Transport glyph in the card corner, colored by the transport itself —
/// Bluetooth blue, receiver green, cable muted; the connectivity words ride
/// its hover tip and the dimmed render already marks an offline card.
fn transport_glance(record: &DeviceRecord, pal: Palette) -> impl IntoElement {
    let path = if matches!(record.kind, DeviceKind::Camera) {
        "action-icons/usb.svg"
    } else {
        connection_icon_path(
            record.route.as_ref(),
            record.receiver_brand,
            record.model_info.as_ref(),
        )
    };
    let color: Hsla = match path {
        "action-icons/bluetooth.svg" => rgb(theme::ACCENT_BLUE).into(),
        "action-icons/bolt.svg" | "action-icons/unifying.svg" => {
            rgb(theme::STATUS_CONNECTED).into()
        }
        _ => pal.text_muted,
    };
    let hint: SharedString = format!(
        "{} · {}",
        if record.online {
            tr!("device.connected")
        } else {
            tr!("device.offline")
        },
        connection_summary(record)
    )
    .into();
    div()
        .id((ElementId::from("transport-glance"), record.record_key()))
        .flex_none()
        .child(svg().path(path).size_4().flex_none().text_color(color))
        .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
}

/// [`BatteryIndicator::glance`] under a hover tip carrying the value and
/// words the corner has no room for.
fn battery_glance(
    battery: &openlogi_core::device::BatteryInfo,
    record: &DeviceRecord,
) -> impl IntoElement {
    let hint: SharedString = glance_hint(battery, record.online).into();
    div()
        .id((ElementId::from("battery-glance"), record.record_key()))
        .flex_none()
        .child(BatteryIndicator::glance(battery))
        .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
}

fn device_ring(enabled: bool) -> Hsla {
    if enabled {
        gpui::transparent_black()
    } else {
        rgb(theme::STATUS_DISABLED).into()
    }
}

/// The device class as a small pill riding the name line, in the outlined
/// shape of the header's `status_badge`.
pub(super) fn kind_badge(kind: DeviceKind, pal: Palette) -> impl IntoElement {
    div()
        .flex_none()
        .px_1p5()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(pal.border)
        .bg(pal.muted)
        .text_caption()
        .text_color(pal.text_muted)
        .child(kind_label(kind))
}

/// The model name under a custom display name; `None` when the name already
/// is the model.
pub(super) fn custom_model_subtitle(record: &DeviceRecord) -> Option<SharedString> {
    (record.display_name != record.model_name).then(|| record.model_name.clone().into())
}

/// The card's action menu — rename, and for an offline device, delete. One
/// builder serves both the corner menu button and the card's context menu.
pub(super) fn device_menu(
    record: &DeviceRecord,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let record_key = record.record_key();
    let custom_name = if record.display_name == record.model_name {
        String::new()
    } else {
        record.display_name.clone()
    };
    let model_name = record.model_name.clone();
    let display_name = record.display_name.clone();
    // A live device would simply re-register on the next inventory snapshot,
    // so deletion is only offered once it is offline.
    let deletable = record.persistent && !record.online;
    move |menu, _window, _cx| {
        let menu = menu.item(
            PopupMenuItem::new(tr!("common.rename_dialog"))
                .icon(Icon::empty().path("action-icons/pencil.svg"))
                .on_click({
                    let record_key = record_key.clone();
                    let custom_name = custom_name.clone();
                    let model_name = model_name.clone();
                    move |_, window, cx| {
                        open_rename_dialog(
                            window,
                            cx,
                            record_key.clone(),
                            custom_name.clone(),
                            model_name.clone(),
                        );
                    }
                }),
        );
        if !deletable {
            return menu;
        }
        menu.item(PopupMenuItem::separator()).item(
            PopupMenuItem::new(tr!("device.delete_device_dialog"))
                .icon(IconName::Delete)
                .on_click({
                    let record_key = record_key.clone();
                    let display_name = display_name.clone();
                    move |_, window, cx| {
                        open_delete_confirmation(
                            window,
                            cx,
                            record_key.clone(),
                            display_name.clone(),
                        );
                    }
                }),
        )
    }
}

/// The card corner's ellipsis button, opening the same menu the card offers
/// on right-click.
pub(super) fn device_menu_button(record: &DeviceRecord, pal: Palette) -> impl IntoElement {
    Button::new((ElementId::from("device-menu"), record.record_key()))
        .ghost()
        .xsmall()
        .text_color(pal.text_muted)
        .icon(IconName::Ellipsis)
        .dropdown_menu_with_anchor(Anchor::TopRight, device_menu(record))
}

/// Confirm before forgetting a device: the record, its custom name, and its
/// per-device settings all go.
fn open_delete_confirmation(window: &mut Window, cx: &mut App, record_key: String, name: String) {
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .title(tr!("device.delete_named_device_confirmation", name => name.clone()))
            .description(tr!("device.delete_device_description"))
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("device.delete_device"))
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text(tr!("common.cancel"))
                    .show_cancel(true),
            )
            .on_ok({
                let record_key = record_key.clone();
                move |_event, _window, cx| {
                    AppState::update(cx, |state, cx| {
                        if state.forget_device(&record_key) {
                            cx.emit(StateEvent::InventoryChanged);
                        }
                    });
                    true
                }
            })
    });
}

fn open_rename_dialog(
    window: &mut Window,
    cx: &mut App,
    record_key: String,
    custom_name: String,
    model_name: String,
) {
    let input = cx.new(|cx| {
        let mut input = InputState::new(window, cx).placeholder(model_name);
        input.set_value(custom_name, window, cx);
        input
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        input.update(cx, |input, cx| input.focus(window, cx));
        dialog
            .w(px(420.))
            .title(tr!("device.rename_device"))
            .child(
                v_flex().gap_2().child(control_input(&input)).child(
                    div()
                        .text_caption()
                        .text_color(theme::palette(cx).text_muted)
                        .child(tr!("device.leave_blank_to_use_the_model_name")),
                ),
            )
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("common.save"))
                    .cancel_text(tr!("common.cancel"))
                    .show_cancel(true),
            )
            .on_ok({
                let input = input.clone();
                let record_key = record_key.clone();
                move |_, _, cx| {
                    let custom_name = input.read(cx).value().to_string();
                    AppState::update(cx, |state, cx| {
                        state.set_device_custom_name(&record_key, &custom_name);
                        cx.emit(StateEvent::InventoryChanged);
                    });
                    true
                }
            })
    });
}

fn connection_view(record: &DeviceRecord, pal: Palette) -> impl IntoElement {
    h_flex()
        .w_full()
        .min_w_0()
        .gap_1p5()
        .items_center()
        .text_caption()
        .text_color(pal.text_muted)
        .child(connectivity_dot(record.online, pal))
        .child(if record.online {
            tr!("device.connected")
        } else {
            tr!("device.offline")
        })
        .child("·")
        .child(
            svg()
                .path(if matches!(record.kind, DeviceKind::Camera) {
                    "action-icons/usb.svg"
                } else {
                    connection_icon_path(
                        record.route.as_ref(),
                        record.receiver_brand,
                        record.model_info.as_ref(),
                    )
                })
                .size_3()
                .flex_none(),
        )
        .child(div().min_w_0().truncate().child(connection_summary(record)))
}

fn connection_summary(record: &DeviceRecord) -> String {
    let route = route_label(record.route.as_ref(), record.receiver_brand);
    if matches!(
        record.route,
        Some(DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. })
    ) {
        format!("{route} · {} {}", tr!("device.channel"), record.slot)
    } else {
        route
    }
}

/// The device photo, scaled to fit its container (object-fit contain), or a
/// neutral placeholder when the depot ships no front render.
///
/// Sized with `max_*` rather than `size_full` so the image is bounded by the
/// container but keeps its intrinsic aspect: `size_full` makes gpui's `img`
/// fall back to the raw pixel dimensions when the box can't fully constrain it,
/// which (with an `overflow_hidden` parent) cropped the device into a zoomed
/// close-up. `object_fit` defaults to `Contain`, so the whole device shows.
fn device_image(
    record: &DeviceRecord,
    light_enabled: bool,
    light_settings: LightSettings,
    pal: Palette,
) -> AnyElement {
    if record.kind == DeviceKind::Light {
        return light_visual::gallery(
            record.asset.as_ref(),
            light_visual::LightView {
                online: record.online,
                enabled: light_enabled,
            },
            light_settings,
            pal,
        )
        .into_any_element();
    }
    if let Some(path) = record
        .asset
        .as_ref()
        .and_then(|a| a.hero_image_path.clone())
    {
        return img(path).max_w_full().max_h_full().into_any_element();
    }
    // Cameras carry no depot asset, so give them a recognisable glyph on their
    // gallery card instead of the generic chip fallback.
    let icon = if matches!(record.kind, DeviceKind::Camera) {
        IconName::Eye
    } else {
        IconName::Cpu
    };
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(Icon::new(icon).size_8().text_color(pal.text_muted))
        .into_any_element()
}

/// Connection-type glyph for a gallery card, classified from its active route.
pub(super) fn connection_icon_path(
    route: Option<&DeviceRoute>,
    receiver_brand: Option<ReceiverBrand>,
    model: Option<&DeviceModelInfo>,
) -> &'static str {
    match ConnectionKind::for_device(route, receiver_brand, model) {
        ConnectionKind::BoltReceiver => "action-icons/bolt.svg",
        // TODO: Replace the Nano and Lightspeed fallbacks with their appropriate logos.
        ConnectionKind::UnifyingReceiver
        | ConnectionKind::NanoReceiver
        | ConnectionKind::LightspeedReceiver => "action-icons/unifying.svg",
        ConnectionKind::BluetoothDirect => "action-icons/bluetooth.svg",
        ConnectionKind::Wired => "action-icons/usb.svg",
        ConnectionKind::Unknown => "action-icons/circle-dot.svg",
    }
}

/// Home body while the agent's first enumeration is still in flight: the
/// device set is *unknown*, not empty, so this keeps the quiet loading frame
/// rather than flashing the add-device empty state (icon, headline, CTA) at a
/// user whose devices are about to appear. Swaps to the gallery, to
/// [`device_empty_state`], or to [`scanning_unavailable_state`] the moment
/// the agent reports where its enumeration landed.
pub(super) fn device_scanning_state(cx: &App) -> Div {
    loading_body(tr!("agent.scanning_for_devices"), cx)
        .flex_1()
        .w_full()
        .min_h_0()
}

/// Home body when the agent reports enumeration as broken
/// ([`InventoryHealth::Unavailable`]): scanning never completed and won't
/// just by waiting, so showing a spinner (or claiming "no devices") would
/// both be wrong. The agent keeps retrying and a recovery flows back in as a
/// regular snapshot.
pub(super) fn scanning_unavailable_state(cx: &App) -> Div {
    notice_body(
        tr!("agent.device_scanning_is_unavailable"),
        tr!("agent.device_scan_failure_description"),
        cx,
    )
    .flex_1()
    .w_full()
    .min_h_0()
}

/// Body shown when the agent has completed an enumeration and found no
/// devices. The polling keeps running and `AppView`'s `AppState` observer
/// swaps the device UI back in the moment one appears, so this is purely a
/// wait-and-pair placeholder.
pub(super) fn device_empty_state(cx: &App) -> Div {
    let pal = theme::palette(cx);
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_4()
        .p_8()
        .child(
            Icon::new(IconName::Search)
                .size_8()
                .text_color(pal.text_muted),
        )
        .child(div().text_title().child(tr!("device.no_devices_connected")))
        .child(
            div()
                .max_w(ContentWidth::Narrow.rems())
                .text_body()
                .text_center()
                .child(tr!("device.device_connection_help")),
        )
        .child(
            Button::new("empty-add-device")
                .primary()
                .icon(IconName::Plus)
                .label(tr!("pairing.add_device"))
                .on_click(|_, _, cx| crate::windows::add_device::open(cx)),
        )
        .child(
            div()
                .mt_1()
                .max_w(ContentWidth::Narrow.rems())
                .text_caption()
                .text_center()
                .text_color(pal.text_muted)
                .child(tr!("device.quit_logi_options_hid_access")),
        )
}
