//! The Home (device gallery) screen: the top bar, the scrollable device-card
//! row, and the loading/empty states shown in place of the gallery before the
//! agent has reported an inventory.

use std::sync::Arc;

use gpui::{
    AnyElement, BorrowAppContext as _, BoxShadow, Context, Div, Hsla, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement as _, Styled, canvas, div, fill, img,
    point, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{
    Icon, IconName,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, DeviceKind, DeviceTransports,
};
use openlogi_hid::DeviceRoute;

use super::AppView;
use super::status::{loading_body, notice_body};
use super::widgets::{add_device_button, kind_label, settings_button};
use crate::asset::GlowGeometry;
use crate::components::carousel::Carousel;
use crate::state::{AppState, DeviceRecord};
use crate::theme::{self, HEADER_H, Palette, SelectableStyle as _};

/// Home (gallery) top bar: the "Devices" title, a Settings gear, and the
/// Add-Device button — the entry points the old carousel header used to carry.
pub(super) fn home_header(pal: Palette) -> impl IntoElement {
    h_flex()
        .h(px(HEADER_H))
        .w_full()
        .px_5()
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(pal.border)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(tr!("Devices")),
        )
        .child(settings_button())
        .child(add_device_button())
}

/// Horizontal gap between gallery cards, in pixels.
const GALLERY_GAP: f32 = 24.;

/// The Home device list: an equal-size, horizontally scrollable row of device
/// cards (Logi Options+ style), via [`Carousel`]'s `uniform` mode. Each card
/// floats the device photo on the window background above its name and battery;
/// the row centres while the cards fit the viewport and scrolls once they don't.
/// Clicking a card opens its detail screen and makes it the active device (whose
/// bindings the hook uses); the active card wears a faint accent ring.
pub(super) fn device_gallery(cx: &mut Context<AppView>) -> impl IntoElement {
    let (len, active_idx) = cx.try_global::<AppState>().map_or((0, 0), |s| {
        let len = s.device_list.len();
        (len, s.current_device.min(len.saturating_sub(1)))
    });
    let view = cx.entity();

    v_flex().flex_1().w_full().min_h_0().child(
        Carousel::new("device-carousel")
            .len(len)
            .selected(active_idx)
            .uniform(px(theme::GALLERY_CARD_W))
            .gap(px(GALLERY_GAP))
            .accent(rgb(theme::ACCENT_BLUE).into())
            .render_item(move |idx, focused, _window, cx| {
                let pal = theme::palette(cx);
                let Some(record) = cx
                    .try_global::<AppState>()
                    .and_then(|s| s.device_list.get(idx).cloned())
                else {
                    return div().into_any_element();
                };
                let key = record.config_key.clone();
                let glow = cx
                    .try_global::<AppState>()
                    .and_then(|s| keyboard_glow(s, &record));
                let view = view.clone();
                device_card(&record, focused, glow, pal)
                    .id(("device-card", idx))
                    .cursor_pointer()
                    .hover(move |s| s.border_color(rgb(theme::ACCENT_BLUE)))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.open_device(key.clone(), cx));
                    })
                    .into_any_element()
            })
            .on_select(cx.listener(|_, ix: &usize, _, cx| {
                cx.update_global::<AppState, _>(|state, _| state.set_current_device(*ix));
                cx.notify();
            })),
    )
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

/// A device card in the Home gallery: the device photo floating on the window
/// background above the name, connectivity dot, kind/slot, and battery. Fixed
/// width so cards stay equal in the scrollable row. No card shows a border at
/// rest — the accent ring appears only on hover (wired by the gallery). The
/// `active` device (whose bindings and DPI are live) keeps a persistent but
/// borderless marker: a faint accent fill, which reads whether or not the
/// carousel centres it. The 1px border is always reserved in a transparent
/// colour so the hover ring never nudges the layout. Returns a bare [`Div`] so
/// the gallery can wire the hover and click handlers.
fn device_card(
    record: &DeviceRecord,
    active: bool,
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
    pal: Palette,
) -> Div {
    v_flex()
        .w(px(theme::GALLERY_CARD_W))
        .flex_shrink_0()
        .items_center()
        .gap_3()
        .p_3()
        .rounded(pal.card_radius)
        .border_1()
        .border_color(gpui::transparent_black())
        .selected_fill(active)
        .child(
            div()
                .relative()
                .w_full()
                .h(px(theme::GALLERY_PHOTO_H))
                .flex()
                .items_center()
                .justify_center()
                .when_some(glow, |this, (geom, color)| {
                    this.child(glow_canvas(geom, color))
                })
                .child(device_image(record, pal)),
        )
        .child(
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(record.display_name.clone()),
                        )
                        .child(status_dot(record.online)),
                )
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(pal.text_muted)
                                .child(format!(
                                    "{} · slot {}",
                                    kind_label(record.kind),
                                    record.slot
                                )),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    svg()
                                        .path(connection_icon_path(
                                            record.route.as_ref(),
                                            record.model_info.as_ref().map(|m| &m.transports),
                                        ))
                                        .size_3()
                                        .flex_none()
                                        .text_color(pal.text_muted),
                                )
                                .when_some(record.battery.as_ref(), |this, b| {
                                    this.child(battery_view(b, pal))
                                }),
                        ),
                ),
        )
}

/// The device photo, scaled to fit its container (object-fit contain), or a
/// neutral placeholder when the depot ships no front render.
///
/// Sized with `max_*` rather than `size_full` so the image is bounded by the
/// container but keeps its intrinsic aspect: `size_full` makes gpui's `img`
/// fall back to the raw pixel dimensions when the box can't fully constrain it,
/// which (with an `overflow_hidden` parent) cropped the device into a zoomed
/// close-up. `object_fit` defaults to `Contain`, so the whole device shows.
fn device_image(record: &DeviceRecord, pal: Palette) -> AnyElement {
    match record
        .asset
        .as_ref()
        .and_then(|a| a.hero_image_path.clone())
    {
        Some(path) => img(path).max_w_full().max_h_full().into_any_element(),
        None => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(IconName::Cpu).size_8().text_color(pal.text_muted))
            .into_any_element(),
    }
}

/// Connectivity dot for a gallery card: a steady grey when offline, a green dot
/// with a static glow when connected. The glow is a fixed `BoxShadow`, not a
/// `.repeat()` animation: an infinite animation keeps GPUI re-rendering every
/// frame for as long as a device is connected, pinning the render loop and
/// burning CPU/battery while the app is idle.
fn status_dot(online: bool) -> AnyElement {
    let color = if online {
        theme::STATUS_CONNECTED
    } else {
        theme::STATUS_OFFLINE
    };
    let base = div().size(px(10.)).rounded_full().bg(rgb(color));
    if !online {
        return base.into_any_element();
    }
    base.shadow(vec![BoxShadow {
        color: gpui::hsla(0.35, 0.7, 0.55, 0.6),
        offset: point(px(0.), px(0.)),
        blur_radius: px(6.),
        spread_radius: px(0.5),
        inset: false,
    }])
    .into_any_element()
}

/// Battery readout for a gallery card: a charge/level glyph plus the
/// percentage, in the muted metadata style.
fn battery_view(b: &BatteryInfo, pal: Palette) -> AnyElement {
    h_flex()
        .gap_1()
        .items_center()
        .text_xs()
        .text_color(pal.text_muted)
        .child(Icon::new(battery_icon(b)).size_3())
        .child(format!("{}%", b.percentage))
        .into_any_element()
}

/// Pick the battery glyph from charge state first (charging / full / error),
/// then fall back to the discrete charge level for a plain discharge.
fn battery_icon(b: &BatteryInfo) -> IconName {
    match b.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => IconName::BatteryCharging,
        BatteryStatus::Full => IconName::BatteryFull,
        BatteryStatus::Error => IconName::BatteryWarning,
        BatteryStatus::Discharging | BatteryStatus::Unknown => match b.level {
            BatteryLevel::Critical => IconName::BatteryWarning,
            BatteryLevel::Low => IconName::BatteryLow,
            BatteryLevel::Good => IconName::BatteryMedium,
            BatteryLevel::Full => IconName::BatteryFull,
            BatteryLevel::Unknown => IconName::Battery,
        },
    }
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
                .text_xl()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(tr!("No devices connected")),
        )
        .child(
            div()
                .max_w(px(440.))
                .text_sm()
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
        .child(div().mt_1().max_w(px(440.)).text_xs().text_center().text_color(pal.text_muted).child(tr!(
            "Using Logi Options+? Quit it first — both apps compete for HID++ access."
        )))
        .into_any_element()
}
