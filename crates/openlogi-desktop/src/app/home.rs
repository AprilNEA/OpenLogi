//! The Home (device gallery) screen: its top bar, switchable grid/list/carousel
//! layouts, and the loading/empty states shown before the agent reports an
//! inventory.

mod views;

pub(super) use views::device_gallery;
#[cfg(test)]
pub(super) use views::ordered_device_indices;

use std::collections::HashMap;
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
use openlogi_core::device::{DeviceKind, DeviceModelInfo, ModelTransport};
use openlogi_core::hid::{DeviceRoute, ReceiverProtocol};
use openlogi_ipc::PairingFailure;

use super::AppView;
use super::status::{loading_body, notice_body};
use super::widgets::{
    add_device_button, connection_summary, connectivity_dot, kind_label, receiver_slot_label,
    settings_button,
};
use crate::features::lighting::visual as light_visual;
use crate::services::assets::GlowGeometry;
use crate::services::ipc::UnpairFailure;
use crate::state::{AppState, DeviceRecord, ObservedConnection};
use crate::ui::battery::{BatteryIndicator, glance_hint};
use crate::ui::components::{control_input, save_cancel_footer};
use crate::ui::theme::{self, ContentWidth, HEADER_H, Palette, Typography as _};
use crate::windows::add_device::pairing_failure_text;

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

/// A list-thumbnail character covers about a third of one device pixel. Gain
/// brings that pixel up to full strength; a border pixel that only clips the
/// stroke stays dim.
const COVERAGE_GAIN: f32 = 3.;

/// Which render the glow has to sit on. The holes are measured on that file,
/// so a colour variant does not reuse another picture's mask.
#[derive(Clone, Copy)]
pub(crate) enum GlowImage {
    /// Home grid, list, and carousel. They draw the hero render.
    Gallery,
    /// Device detail. It draws [`crate::services::assets::ResolvedAsset::image_path`].
    Detail,
}

/// The inter-key glow geometry and tinted colour for `record`, or `None` unless
/// it's a keyboard with lighting enabled and a render that has enclosed holes.
/// The geometry is painted live by [`glow_canvas`] — no pre-rendered PNG, so a
/// colour change costs no new texture.
pub(crate) fn keyboard_glow(
    state: &AppState,
    record: &DeviceRecord,
    image: GlowImage,
) -> Option<(Arc<GlowGeometry>, Hsla)> {
    if record.kind != DeviceKind::Keyboard {
        return None;
    }
    let lighting = state
        .lighting_for(&record.config_key, &record.route_key)
        .filter(|l| l.enabled)?;
    let asset = record.asset.as_ref()?;
    let geom = match image {
        GlowImage::Gallery => asset.hero_glow.as_ref().or(asset.glow.as_ref()),
        GlowImage::Detail => asset.glow.as_ref(),
    }?
    .clone();
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
            let scale = window.scale_factor();
            let scale = if scale.is_finite() && scale > 0. {
                scale
            } else {
                1.
            };
            // One quad per device pixel the legend actually covers, dimmed by
            // that coverage. A full-bright quad on a pixel that only clips the
            // character is the red dot on the key border in the list thumbnail.
            let px_size = 1. / scale;
            for splat in glow_coverage(
                &geom.holes,
                geom.image_width,
                geom.image_height,
                GlowPlacement {
                    ox,
                    oy,
                    rw,
                    rh,
                    scale,
                },
            ) {
                let mut tint = color;
                tint.a *= (splat.fraction * COVERAGE_GAIN).min(1.);
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "device pixel index of a window coord fits in f32"
                )]
                let quad = gpui::Bounds {
                    origin: point(px(splat.x as f32 / scale), px(splat.y as f32 / scale)),
                    size: gpui::size(px(px_size), px(px_size)),
                };
                window.paint_quad(fill(quad, tint));
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
    saved_transport: Option<ObservedConnection>,
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
                        .child(transport_glance(record, pal, saved_transport)),
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
fn transport_glance(
    record: &DeviceRecord,
    pal: Palette,
    saved_transport: Option<ObservedConnection>,
) -> impl IntoElement {
    let path = if matches!(record.kind, DeviceKind::Camera) {
        "action-icons/usb.svg"
    } else {
        connection_icon_path(
            record.route.as_ref(),
            record.model_info.as_ref(),
            saved_transport,
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
    h_flex()
        .id((ElementId::from("transport-glance"), record.record_key()))
        .flex_none()
        .items_center()
        .gap_1()
        .child(svg().path(path).size_4().flex_none().text_color(color))
        .when_some(receiver_slot_label(record), |glance, slot| {
            glance.child(div().text_caption().text_color(pal.text_muted).child(slot))
        })
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
    let has_stale_pairings = !record.stale_pairings.is_empty();
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
        let menu = if has_stale_pairings {
            menu.item(
                PopupMenuItem::new(tr!("device.remove_old_pairings_dialog"))
                    .icon(IconName::Delete)
                    .on_click({
                        let record_key = record_key.clone();
                        let display_name = display_name.clone();
                        move |_, window, cx| {
                            open_stale_pairings_confirmation(
                                window,
                                cx,
                                record_key.clone(),
                                display_name.clone(),
                            );
                        }
                    }),
            )
        } else {
            menu
        };
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
                    AppState::apply(cx, |state| state.forget_device(&record_key));
                    true
                }
            })
    });
}

/// Confirm before removing the leftover slots folded into a card. Only the
/// receiver's old pairings go; the device, its card, and its settings stay.
fn open_stale_pairings_confirmation(
    window: &mut Window,
    cx: &mut App,
    record_key: String,
    name: String,
) {
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .title(tr!("device.remove_old_pairings_confirmation", name => name.clone()))
            .description(tr!("device.remove_old_pairings_description", name => name.clone()))
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("device.remove_old_pairings"))
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text(tr!("common.cancel"))
                    .show_cancel(true),
            )
            .on_ok({
                let record_key = record_key.clone();
                move |_event, _window, cx| {
                    AppState::apply(cx, |state| state.remove_stale_pairings(&record_key));
                    true
                }
            })
    });
}

/// Say why a device the user asked to forget is still here: its receiver kept
/// the pairing, so deleting only its settings would bring the card back.
pub(super) fn open_removal_failed(
    window: &mut Window,
    cx: &mut App,
    name: &str,
    failure: &UnpairFailure,
) {
    let title = tr!("device.delete_device_failed", name => name.to_string());
    open_unpair_failed(window, cx, title, failure);
}

/// Say why a leftover slot the user asked to remove is still paired.
pub(super) fn open_stale_pairing_removal_failed(
    window: &mut Window,
    cx: &mut App,
    name: &str,
    failure: &UnpairFailure,
) {
    let title = tr!("device.remove_old_pairings_failed", name => name.to_string());
    open_unpair_failed(window, cx, title, failure);
}

fn open_unpair_failed(
    window: &mut Window,
    cx: &mut App,
    title: SharedString,
    failure: &UnpairFailure,
) {
    let description = match failure {
        UnpairFailure::AgentUnreachable => tr!("agent.cant_reach_the_background_service"),
        UnpairFailure::Refused(PairingFailure::ReceiverBusy) => {
            tr!("device.delete_device_receiver_busy")
        }
        UnpairFailure::Refused(PairingFailure::ReceiverNotFound) => {
            tr!("device.delete_device_receiver_missing")
        }
        UnpairFailure::Refused(other) => pairing_failure_text(other).into(),
    };
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert.title(title.clone()).description(description.clone())
    });
}

pub(super) fn open_rename_dialog(
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
            .footer(save_cancel_footer())
            .on_ok({
                let input = input.clone();
                let record_key = record_key.clone();
                move |_, _, cx| {
                    let custom_name = input.read(cx).value().to_string();
                    AppState::apply(cx, |state| {
                        state.commit_device_custom_name(&record_key, &custom_name)
                    });
                    true
                }
            })
    });
}

fn connection_view(
    record: &DeviceRecord,
    pal: Palette,
    saved_transport: Option<ObservedConnection>,
) -> impl IntoElement {
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
                        record.model_info.as_ref(),
                        saved_transport,
                    )
                })
                .size_3()
                .flex_none(),
        )
        .child(div().min_w_0().truncate().child(connection_summary(record)))
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

/// Connection-type glyph for a gallery card: a dongle for receiver-paired
/// devices, a USB mark for wired direct ones, a Bluetooth mark for the rest.
///
/// A `Direct` route carries the vendor/product id of the HID node this
/// session actually enumerated, which — for a multi-transport device —
/// pinpoints the transport that's live *right now* via
/// [`DeviceModelInfo::transport_for_product_id`], rather than guessing from
/// the static, unordered transport flags (which mislabeled a USB-connected
/// multi-transport device as Bluetooth — issue #1218).
/// Offline cards retain the last observed mode, or an unambiguous recorded
/// link after restart; radio support alone does not select Bluetooth.
pub(super) fn connection_icon_path(
    route: Option<&DeviceRoute>,
    model: Option<&DeviceModelInfo>,
    saved_transport: Option<ObservedConnection>,
) -> &'static str {
    let direct_transport = match route {
        Some(DeviceRoute::Direct { product_id, .. }) => {
            model.and_then(|m| m.transport_for_product_id(*product_id))
        }
        None => match saved_transport {
            Some(ObservedConnection::Direct(transport)) => Some(transport),
            Some(ObservedConnection::Receiver(_)) | None => None,
        },
        Some(
            DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. } | DeviceRoute::RawHid { .. },
        ) => None,
    };
    if let Some(transport) = direct_transport {
        return match transport {
            ModelTransport::Equad | ModelTransport::Usb => "action-icons/usb.svg",
            ModelTransport::Bluetooth | ModelTransport::Btle => "action-icons/bluetooth.svg",
        };
    }

    match (route, saved_transport) {
        (Some(DeviceRoute::Bolt { .. }), _)
        | (None, Some(ObservedConnection::Receiver(ReceiverProtocol::Bolt))) => {
            "action-icons/bolt.svg"
        }
        (Some(DeviceRoute::Unifying { .. }), _)
        | (None, Some(ObservedConnection::Receiver(ReceiverProtocol::Unifying))) => {
            "action-icons/unifying.svg"
        }
        // An offline model's radio capabilities do not identify a connection.
        // Preserve the wired-only fallback; ambiguous offline links stay neutral.
        (Some(DeviceRoute::Direct { .. }) | None, _) => match model.map(|m| m.transports) {
            Some(t) if (t.usb || t.equad) && !t.bluetooth && !t.btle => "action-icons/usb.svg",
            _ if route.is_some() => "action-icons/bluetooth.svg",
            _ => "action-icons/circle-dot.svg",
        },
        (Some(DeviceRoute::RawHid { .. }), _) => "action-icons/usb.svg",
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
/// ([`InventoryHealth::Unavailable`](openlogi_ipc::InventoryHealth::Unavailable)): scanning
/// never completed and won't
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

/// Width and height of a `aspect` box contain-fit into `slot_w` × `slot_h`.
pub(super) fn fitted_frame(slot_w: f32, slot_h: f32, aspect: f32) -> (f32, f32) {
    if !(aspect.is_finite() && aspect > 0.) {
        return (slot_w, slot_h);
    }
    if slot_w / slot_h > aspect {
        (slot_h * aspect, slot_h)
    } else {
        (slot_w, slot_w / aspect)
    }
}

#[derive(Clone, Copy)]
struct GlowPlacement {
    ox: f32,
    oy: f32,
    rw: f32,
    rh: f32,
    scale: f32,
}

/// Device pixels to light, with the fraction of each that is legend.
///
/// `holes` are source pixels packed `x | (y << 16)`. They are splatted into
/// the contain-fit rect after the same edge snap as the device image, so the
/// bright pixel is the one that holds the character. A pixel that only clips
/// the stroke comes back with a small fraction and stays dim.
fn glow_coverage(
    holes: &[u32],
    image_width: u32,
    image_height: u32,
    place: GlowPlacement,
) -> Vec<GlowSplat> {
    if holes.is_empty()
        || image_width == 0
        || image_height == 0
        || !(place.rw > 0. && place.rh > 0. && place.scale > 0.)
        || !place.rw.is_finite()
        || !place.rh.is_finite()
        || !place.scale.is_finite()
    {
        return Vec::new();
    }
    let GlowPlacement {
        ox,
        oy,
        rw,
        rh,
        scale,
    } = place;
    let left = round_half_toward_zero(ox * scale);
    let top = round_half_toward_zero(oy * scale);
    let right = round_half_toward_zero((ox + rw) * scale).max(left);
    let bottom = round_half_toward_zero((oy + rh) * scale).max(top);
    let dw = right - left;
    let dh = bottom - top;
    if dw < 1. || dh < 1. {
        return Vec::new();
    }
    let mut counts: HashMap<(i32, i32), u32> = HashMap::new();
    #[expect(
        clippy::cast_precision_loss,
        reason = "image coords are < 8192 px — well within f32 mantissa"
    )]
    let (iw, ih) = (image_width as f32, image_height as f32);
    for &packed in holes {
        #[expect(
            clippy::cast_precision_loss,
            reason = "image coords are < 8192 px — well within f32 mantissa"
        )]
        let (x, y) = ((packed & 0xffff) as f32, (packed >> 16) as f32);
        let dx = left + (x + 0.5) / iw * dw;
        let dy = top + (y + 0.5) / ih * dh;
        let pixel_x = device_pixel(dx);
        let pixel_y = device_pixel(dy);
        *counts.entry((pixel_x, pixel_y)).or_default() += 1;
    }
    let area = (iw / dw) * (ih / dh);
    let mut splats = Vec::with_capacity(counts.len());
    for ((x, y), count) in counts {
        #[expect(
            clippy::cast_precision_loss,
            reason = "legend pixels inside one device pixel fit in f32"
        )]
        let fraction = (count as f32 / area).min(1.);
        // A couple of antialiased source pixels in a list-thumbnail pixel are
        // the key border, not the character.
        if fraction < 0.02 {
            continue;
        }
        splats.push(GlowSplat { x, y, fraction });
    }
    // At list size a legend is one device pixel, and that pixel is the top
    // edge of the key, flush with the gap. If nothing is lit above it, drop
    // the light one pixel so it sits in the key face. Larger renders keep the
    // pixel the legend actually covers.
    if iw / dw >= 16. && ih / dh >= 16. {
        let above: HashMap<(i32, i32), f32> = splats
            .iter()
            .map(|splat| ((splat.x, splat.y), splat.fraction))
            .collect();
        let bottom_i = device_pixel(bottom);
        for splat in &mut splats {
            let clear_above = above
                .get(&(splat.x, splat.y - 1))
                .is_none_or(|fraction| *fraction < 0.02);
            if clear_above && splat.y + 1 < bottom_i {
                splat.y += 1;
            }
        }
    }
    splats.sort_unstable_by_key(|splat| (splat.y, splat.x));
    let mut merged: Vec<GlowSplat> = Vec::with_capacity(splats.len());
    for splat in splats {
        if let Some(prev) = merged.last_mut()
            && prev.x == splat.x
            && prev.y == splat.y
        {
            prev.fraction = prev.fraction.max(splat.fraction);
            continue;
        }
        merged.push(splat);
    }
    merged
}

struct GlowSplat {
    x: i32,
    y: i32,
    fraction: f32,
}

/// Rounds to the nearest integer with 0.5 ties toward zero. Matches gpui's
/// `snap_bounds`, which is what places the device image.
fn round_half_toward_zero(value: f32) -> f32 {
    (value.abs() - 0.5).ceil().copysign(value)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "device pixel index of a window coord fits in i32"
)]
fn device_pixel(value: f32) -> i32 {
    value.floor() as i32
}

#[cfg(test)]
mod glow_paint_tests {
    use super::{GlowPlacement, fitted_frame, glow_coverage};

    fn place(rw: f32, rh: f32, scale: f32) -> GlowPlacement {
        GlowPlacement {
            ox: 0.,
            oy: 0.,
            rw,
            rh,
            scale,
        }
    }

    fn pack_row(xs: std::ops::Range<u32>, y: u32) -> Vec<u32> {
        xs.map(|x| x | (y << 16)).collect()
    }

    #[test]
    fn legend_pixel_lands_on_its_own_device_pixel() {
        // 10×10 image, one clear pixel at (4, 6), drawn at 10×10.
        let holes = vec![4 | (6 << 16)];
        let splats = glow_coverage(&holes, 10, 10, place(10., 10., 1.));
        assert_eq!(splats.len(), 1);
        assert_eq!((splats[0].x, splats[0].y), (4, 6));
        assert!((splats[0].fraction - 1.).abs() < 1e-4);
    }

    #[test]
    fn list_thumbnail_keeps_the_character_and_drops_the_border() {
        // 24×24 source in a 2×1 slot. Each device pixel covers 12×24 source
        // pixels. Forty legend pixels sit in the left pixel; five clip the
        // right one — the key border.
        let mut holes = pack_row(0..10, 0);
        holes.extend(pack_row(0..10, 1));
        holes.extend(pack_row(0..10, 2));
        holes.extend(pack_row(0..10, 3));
        holes.extend(pack_row(12..17, 0));
        let splats = glow_coverage(&holes, 24, 24, place(2., 1., 1.));
        assert_eq!(splats.len(), 1, "the five-pixel fringe is not a light");
        assert_eq!((splats[0].x, splats[0].y), (0, 0));
        assert!(
            splats[0].fraction > 0.1,
            "the character pixel stays lit, fraction {}",
            splats[0].fraction
        );
    }

    #[test]
    fn hidpi_legend_uses_device_pixels() {
        // Logical 10×10 at 2x is 20×20 device pixels. Source pixel (4, 6)
        // has its centre at device (9, 13).
        let holes = vec![4 | (6 << 16)];
        let splats = glow_coverage(&holes, 10, 10, place(10., 10., 2.));
        assert_eq!(splats.len(), 1);
        assert_eq!((splats[0].x, splats[0].y), (9, 13));
    }

    #[test]
    fn list_thumbnail_drops_the_light_off_the_key_gap() {
        // 64×64 source in a 4×4 slot (16 source px per device px). Forty legend
        // pixels sit in device pixel (1, 0), with nothing above them — the gap
        // over the key. The light belongs on the next pixel, in the key.
        let mut holes = pack_row(16..26, 0);
        holes.extend(pack_row(16..26, 1));
        holes.extend(pack_row(16..26, 2));
        holes.extend(pack_row(16..26, 3));
        let splats = glow_coverage(&holes, 64, 64, place(4., 4., 1.));
        assert_eq!(splats.len(), 1, "one light for the legend");
        assert_eq!(
            (splats[0].x, splats[0].y),
            (1, 1),
            "the light leaves the gap pixel"
        );
    }

    #[test]
    fn list_slot_frame_matches_the_render_aspect() {
        let (w, h) = fitted_frame(120., 68., 2760. / 1600.);
        assert!((h - 68.).abs() < 1e-4);
        assert!((w - 68. * 2760. / 1600.).abs() < 1e-3);
        assert!(w < 120.);
    }
}
