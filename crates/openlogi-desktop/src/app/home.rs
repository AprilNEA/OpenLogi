//! The Home (device gallery) screen: its top bar, switchable grid/list/carousel
//! layouts, and the loading/empty states shown before the agent reports an
//! inventory.

mod views;

pub(super) use views::device_gallery;
#[cfg(test)]
pub(super) use views::ordered_device_indices;

use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Hsla, IntoElement, ParentElement,
    SharedString, Styled, Window, canvas, div, fill, img, point, prelude::FluentBuilder as _, px,
    rgb, svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::DialogButtonProps,
    h_flex,
    input::InputState,
    v_flex,
};
use openlogi_core::config::{DeviceViewMode, LightSettings};
use openlogi_core::device::{DeviceKind, DeviceTransports};
use openlogi_core::hid::DeviceRoute;

use super::AppView;
use super::status::{loading_body, notice_body};
use super::widgets::{
    add_device_button, connectivity_dot, kind_label, route_label, settings_button,
};
use crate::features::lighting::visual as light_visual;
use crate::services::assets::GlowGeometry;
use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::battery::BatteryIndicator;
use crate::ui::components::control_input;
use crate::ui::theme::{
    self, ContentWidth, HEADER_H, Palette, SelectableStyle as _, Typography as _,
};

/// Home (gallery) top bar: title/count, the persisted layout switcher, Settings,
/// and Add Device.
pub(super) fn home_header(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let device_count = AppState::try_read(cx).map_or(0, |state| state.devices().len());
    let current_mode = AppState::try_read(cx).map_or(DeviceViewMode::Grid, |state| {
        state.app_settings().device_view_mode
    });
    let view = cx.entity();
    let device_count_label = if device_count == 1 {
        tr!("%{count} device", count => device_count)
    } else {
        tr!("%{count} devices", count => device_count)
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
                .child(div().text_heading().child(tr!("Devices")))
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
        .lighting_for(&record.config_key)
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

/// A device card in the Home grid and carousel: product image, identity, a
/// single explicit connection line, and a consistently placed battery line.
/// The `active` device keeps a persistent accent ring and faint fill; inactive
/// cards gain the same ring on hover or keyboard focus.
/// Returns an unstyled semantic button so the gallery can add its activation
/// handler without giving up keyboard behavior.
fn device_card(
    record: &DeviceRecord,
    enabled: bool,
    active: bool,
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
        .border_color(device_ring(enabled, active))
        .bg(pal.panel)
        .shadow_xs()
        .selected_fill(active)
        .child(
            div()
                .relative()
                .w_full()
                .h(px(theme::GALLERY_PHOTO_H))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                // The green hardware LED is baked into several product
                // renders. Dimming the complete render is the only truthful
                // treatment available for an offline card without generating
                // a second asset that edits the manufacturer's artwork.
                .opacity(if record.online { 1. } else { 0.38 })
                .when_some(glow, |this, (geom, color)| {
                    this.child(glow_canvas(geom, color))
                })
                .child(device_image(record, light_enabled, light_settings, pal)),
        )
        .child(
            v_flex()
                .w_full()
                .gap_2()
                .child(
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
                                    div()
                                        .truncate()
                                        .text_subheading()
                                        .child(record.display_name.clone()),
                                )
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(pal.text_muted)
                                        .child(device_identity_subtitle(record)),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .items_center()
                                .when(active, |this| {
                                    this.child(
                                        div()
                                            .text_caption()
                                            .text_color(theme::accent())
                                            .child(tr!("Active device")),
                                    )
                                })
                                .when(record.persistent, |this| {
                                    this.child(rename_device_button(record, pal))
                                }),
                        ),
                )
                .child(connection_view(record, pal))
                .child(div().w_full().min_h(px(25.)).when_some(
                    record.battery.as_ref(),
                    |footer, battery| {
                        footer
                            .border_t_1()
                            .border_color(pal.border)
                            .pt_2()
                            .child(BatteryIndicator::status(battery, record.online))
                    },
                )),
        )
}

fn device_ring(enabled: bool, active: bool) -> Hsla {
    if !enabled {
        rgb(theme::STATUS_DISABLED).into()
    } else if active {
        theme::accent()
    } else {
        gpui::transparent_black()
    }
}

fn device_identity_subtitle(record: &DeviceRecord) -> SharedString {
    if record.display_name == record.model_name {
        kind_label(record.kind).into()
    } else {
        format!("{} · {}", record.model_name, kind_label(record.kind)).into()
    }
}

fn rename_device_button(record: &DeviceRecord, pal: Palette) -> Button {
    let record_key = record.record_key();
    let custom_name = if record.display_name == record.model_name {
        String::new()
    } else {
        record.display_name.clone()
    };
    let model_name = record.model_name.clone();
    Button::new((ElementId::from("rename-device"), record_key.clone()))
        .ghost()
        .xsmall()
        .text_color(pal.text_muted)
        .label(tr!("Rename"))
        .tooltip(tr!("Rename device"))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            open_rename_dialog(
                window,
                cx,
                record_key.clone(),
                custom_name.clone(),
                model_name.clone(),
            );
        })
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
            .title(tr!("Rename device"))
            .child(
                v_flex().gap_2().child(control_input(&input)).child(
                    div()
                        .text_caption()
                        .text_color(theme::palette(cx).text_muted)
                        .child(tr!("Leave blank to use the model name.")),
                ),
            )
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("Save"))
                    .cancel_text(tr!("Cancel"))
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
            tr!("Connected")
        } else {
            tr!("Offline")
        })
        .child("·")
        .child(
            svg()
                .path(if matches!(record.kind, DeviceKind::Camera) {
                    "action-icons/usb.svg"
                } else {
                    connection_icon_path(
                        record.route.as_ref(),
                        record.model_info.as_ref().map(|model| &model.transports),
                    )
                })
                .size_3()
                .flex_none(),
        )
        .child(div().min_w_0().truncate().child(connection_summary(record)))
}

fn connection_summary(record: &DeviceRecord) -> String {
    let route = route_label(record.route.as_ref());
    if matches!(
        record.route,
        Some(DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. })
    ) {
        format!("{route} · {} {}", tr!("Channel"), record.slot)
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
            record.online,
            light_enabled,
            light_settings,
            pal,
        );
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
/// devices, a USB mark for radio-less direct ones (a wired keyboard is only
/// ever on the cable), a Bluetooth mark for the rest.
///
/// The route says how the device is *addressed*, not what medium carries it,
/// so `Direct` alone can't pick a glyph — the firmware transport table
/// (HID++ 0x0003) disambiguates. A radio-capable device on a direct route
/// keeps the Bluetooth mark: it *may* be on a cable right now, but the
/// current link medium isn't reported, and Bluetooth is how such devices are
/// normally attached.
pub(super) fn connection_icon_path(
    route: Option<&DeviceRoute>,
    transports: Option<&DeviceTransports>,
) -> &'static str {
    match route {
        Some(DeviceRoute::Bolt { .. }) => "action-icons/bolt.svg",
        Some(DeviceRoute::Unifying { .. }) => "action-icons/unifying.svg",
        // Explicit arms (not `_`) so a new DeviceRoute variant trips the
        // compiler here, matching the exhaustive sibling `route_label`.
        Some(DeviceRoute::Direct { .. }) | None => match transports {
            // No Bluetooth radio at all ⇒ the direct link can only be the
            // cable. eQuad counts as wired-capable here: eQuad is
            // receiver-only by definition, so it is never the *direct* link —
            // an equad-only table still means this connection is a cable.
            Some(t) if (t.usb || t.equad) && !t.bluetooth && !t.btle => "action-icons/usb.svg",
            // Unknown transports (no 0x0003 snapshot, or an all-false table)
            // keep the old default.
            _ => "action-icons/bluetooth.svg",
        },
        Some(DeviceRoute::RawHid { .. }) => "action-icons/usb.svg",
    }
}

/// Home body while the agent's first enumeration is still in flight: the
/// device set is *unknown*, not empty, so this keeps the quiet loading frame
/// rather than flashing the add-device empty state (icon, headline, CTA) at a
/// user whose devices are about to appear. Swaps to the gallery, to
/// [`device_empty_state`], or to [`scanning_unavailable_state`] the moment
/// the agent reports where its enumeration landed.
pub(super) fn device_scanning_state(pal: Palette) -> AnyElement {
    loading_body(tr!("Scanning for devices…"), pal)
        .flex_1()
        .w_full()
        .min_h_0()
        .into_any_element()
}

/// Home body when the agent reports enumeration as broken
/// ([`InventoryHealth::Unavailable`]): scanning never completed and won't
/// just by waiting, so showing a spinner (or claiming "no devices") would
/// both be wrong. The agent keeps retrying and a recovery flows back in as a
/// regular snapshot.
pub(super) fn scanning_unavailable_state(pal: Palette) -> AnyElement {
    notice_body(
        tr!("Device scanning is unavailable"),
        tr!("The background service couldn't scan for devices — check its log for details."),
        pal,
    )
    .flex_1()
    .w_full()
    .min_h_0()
    .into_any_element()
}

/// Body shown when the agent has completed an enumeration and found no
/// devices. The polling keeps running and `AppView`'s `AppState` observer
/// swaps the device UI back in the moment one appears, so this is purely a
/// wait-and-pair placeholder.
pub(super) fn device_empty_state(pal: Palette) -> AnyElement {
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
        .child(
            div()
                .text_title()
                .child(tr!("No devices connected")),
        )
        .child(
            div()
                .max_w(ContentWidth::Narrow.rems())
                .text_body()
                .text_center()
                .child(tr!(
                    "Plug in or pair a supported Logitech device — it'll show up here automatically. For direct Bluetooth connections, pair in your computer's bluetooth settings."
                )),
        )
        .child(
            Button::new("empty-add-device")
                .primary()
                .icon(IconName::Plus)
                .label(tr!("Add Device"))
                .on_click(|_, _, cx| crate::windows::add_device::open(cx)),
        )
        .child(div().mt_1().max_w(ContentWidth::Narrow.rems()).text_caption().text_center().text_color(pal.text_muted).child(tr!(
            "Using Logi Options+? Quit it first — both apps compete for HID++ access."
        )))
        .into_any_element()
}
