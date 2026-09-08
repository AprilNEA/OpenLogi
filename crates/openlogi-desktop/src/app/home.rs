//! The Home (device gallery) screen: its top bar, switchable grid/list/carousel
//! layouts, and the loading/empty states shown before the agent reports an
//! inventory.

mod views;

pub(super) use views::device_gallery;
#[cfg(test)]
pub(super) use views::ordered_device_indices;

use std::{rc::Rc, sync::Arc};

use gpui::{
    Anchor, AnyElement, App, AppContext as _, ClickEvent, Context, Div, ElementId, FocusHandle,
    Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Role,
    SharedString, StatefulInteractiveElement as _, Styled, WeakEntity, Window, canvas, div, fill,
    img, point, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::DialogButtonProps,
    h_flex,
    input::InputState,
    popover::{Popover, PopoverState},
    tooltip::Tooltip,
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
use crate::features::mouse::picker::compact_panel;
use crate::services::assets::GlowGeometry;
use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::battery::{BatteryIndicator, glance_hint};
use crate::ui::components::{MenuRow, control_input};
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
            record.model_info.as_ref().map(|model| &model.transports),
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

#[derive(Clone)]
struct DeviceMenuData {
    record_key: String,
    custom_name: String,
    model_name: String,
    display_name: String,
    deletable: bool,
}

impl DeviceMenuData {
    fn from_record(record: &DeviceRecord) -> Self {
        Self {
            record_key: record.record_key(),
            custom_name: if record.display_name == record.model_name {
                String::new()
            } else {
                record.display_name.clone()
            },
            model_name: record.model_name.clone(),
            display_name: record.display_name.clone(),
            // A live device would simply re-register on the next inventory
            // snapshot, so deletion is only offered once it is offline.
            deletable: record.persistent && !record.online,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeviceMenuAction {
    Rename,
    Delete,
}

type DeviceMenuActionHandler = Rc<dyn Fn(DeviceMenuAction, &mut Window, &mut App)>;

fn menu_row_action(
    row: MenuRow,
    popover: WeakEntity<PopoverState>,
    action: DeviceMenuAction,
    on_action: DeviceMenuActionHandler,
) -> MenuRow {
    let mouse_popover = popover.clone();
    let mouse_action = on_action.clone();
    let keyboard_popover = popover;
    row.capture_any_mouse_down(move |event, window, cx| {
        if event.button == MouseButton::Left {
            cx.stop_propagation();
            dismiss_device_menu(&mouse_popover, window, cx);
            mouse_action(action, window, cx);
        }
    })
    .on_click(move |event, window, cx| {
        if matches!(event, ClickEvent::Keyboard(_)) {
            dismiss_device_menu(&keyboard_popover, window, cx);
            on_action(action, window, cx);
        }
    })
}

fn device_menu_action_handler(data: &DeviceMenuData) -> DeviceMenuActionHandler {
    let data = data.clone();
    Rc::new(move |action, window, cx| match action {
        DeviceMenuAction::Rename => open_rename_dialog(
            window,
            cx,
            data.record_key.clone(),
            data.custom_name.clone(),
            data.model_name.clone(),
        ),
        DeviceMenuAction::Delete => open_delete_confirmation(
            window,
            cx,
            data.record_key.clone(),
            data.display_name.clone(),
        ),
    })
}

fn dismiss_device_menu(popover: &WeakEntity<PopoverState>, window: &mut Window, cx: &mut App) {
    if let Some(popover) = popover.upgrade() {
        popover.update(cx, |state, cx| state.dismiss(window, cx));
    }
}

fn device_menu_content(
    data: DeviceMenuData,
    pal: Palette,
    popover: WeakEntity<PopoverState>,
    on_action: DeviceMenuActionHandler,
    rename_focus: &FocusHandle,
) -> impl IntoElement {
    let rename_row = menu_row_action(
        MenuRow::new((
            ElementId::from("device-menu-rename"),
            data.record_key.clone(),
        ))
        .role(Role::MenuItem)
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(Icon::empty().path("action-icons/pencil.svg").size_4())
                .child(tr!("common.rename_dialog")),
        )
        .track_focus(rename_focus),
        popover.clone(),
        DeviceMenuAction::Rename,
        on_action.clone(),
    );

    let mut panel = compact_panel(pal).w(px(224.)).child(
        div()
            .debug_selector(|| "device-menu-rename-row".into())
            .child(rename_row),
    );

    if data.deletable {
        let delete_row = menu_row_action(
            MenuRow::new((ElementId::from("device-menu-delete"), data.record_key))
                .role(Role::MenuItem)
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(Icon::new(IconName::Delete).size_4())
                        .child(tr!("device.delete_device_dialog")),
                ),
            popover,
            DeviceMenuAction::Delete,
            on_action,
        );
        panel = panel
            .child(
                div()
                    .debug_selector(|| "device-menu-separator".into())
                    .h(px(1.))
                    .w_full()
                    .my_1()
                    .bg(pal.border),
            )
            .child(
                div()
                    .debug_selector(|| "device-menu-delete-row".into())
                    .child(delete_row),
            );
    }

    panel
}

fn device_menu_popover<T>(
    id: impl Into<ElementId>,
    trigger: T,
    anchor: Anchor,
    mouse_button: MouseButton,
    data: DeviceMenuData,
    pal: Palette,
    on_action: DeviceMenuActionHandler,
) -> impl IntoElement
where
    T: gpui_component::Selectable + IntoElement + 'static,
{
    let id = id.into();
    Popover::new(id.clone())
        .anchor(anchor)
        .mouse_button(mouse_button)
        .appearance(false)
        .trigger(trigger)
        .content(move |state, window, cx| {
            let popover = cx.entity().downgrade();
            let rename_focus = window
                .use_keyed_state((id.clone(), "rename-focus"), cx, |_, cx| cx.focus_handle())
                .read(cx)
                .clone();
            if state.is_open() {
                rename_focus.focus(window, cx);
            }
            let keyboard_popover = popover.clone();
            let keyboard_action = on_action.clone();
            div()
                .track_focus(&rename_focus)
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        window.prevent_default();
                        cx.stop_propagation();
                        dismiss_device_menu(&keyboard_popover, window, cx);
                        keyboard_action(DeviceMenuAction::Rename, window, cx);
                    }
                })
                .child(device_menu_content(
                    data.clone(),
                    pal,
                    popover,
                    on_action.clone(),
                    &rename_focus,
                ))
        })
}

/// The card corner's ellipsis button, opening the same menu the card offers
/// on right-click.
pub(super) fn device_menu_button(record: &DeviceRecord, pal: Palette) -> impl IntoElement {
    let data = DeviceMenuData::from_record(record);
    let action_handler = device_menu_action_handler(&data);
    device_menu_button_with_handler(data, pal, action_handler)
}

fn device_menu_button_with_handler(
    data: DeviceMenuData,
    pal: Palette,
    action_handler: DeviceMenuActionHandler,
) -> impl IntoElement {
    device_menu_popover(
        (
            ElementId::from("device-menu-popover"),
            data.record_key.clone(),
        ),
        Button::new((ElementId::from("device-menu"), data.record_key.clone()))
            .ghost()
            .xsmall()
            .text_color(pal.text_muted)
            .icon(IconName::Ellipsis),
        Anchor::TopRight,
        MouseButton::Left,
        data,
        pal,
        action_handler,
    )
}

/// The device card's right-click action menu, anchored to the card edge and
/// sharing the same direct-action rows as the corner button.
pub(super) fn device_context_menu(
    trigger: BaseButton,
    record: &DeviceRecord,
    pal: Palette,
) -> impl IntoElement {
    let data = DeviceMenuData::from_record(record);
    let action_handler = device_menu_action_handler(&data);
    device_context_menu_with_handler(trigger, data, pal, action_handler)
}

fn device_context_menu_with_handler(
    trigger: BaseButton,
    data: DeviceMenuData,
    pal: Palette,
    action_handler: DeviceMenuActionHandler,
) -> impl IntoElement {
    device_menu_popover(
        (
            ElementId::from("device-context-menu"),
            data.record_key.clone(),
        ),
        trigger,
        Anchor::TopRight,
        MouseButton::Right,
        data,
        pal,
        action_handler,
    )
}

/// Confirm before forgetting a device: the record, its custom name, and its
/// per-device settings all go.
pub(super) fn open_delete_confirmation(
    window: &mut Window,
    cx: &mut App,
    record_key: String,
    name: String,
) {
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
        format!("{route} · {} {}", tr!("device.channel"), record.slot)
    } else {
        route
    }
}

#[cfg(test)]
#[expect(
    clippy::items_after_test_module,
    reason = "keep the focused home-menu tests beside their production seam"
)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{Context, Modifiers, MouseButton, Render, TestAppContext, Window, div, point, px};
    use openlogi_core::device::{Capabilities, DeviceKind};

    use super::*;

    struct DeviceMenuHarness {
        record: DeviceRecord,
        actions: Rc<RefCell<Vec<DeviceMenuAction>>>,
    }

    struct NestedDeviceMenuHarness {
        record: DeviceRecord,
        actions: Rc<RefCell<Vec<DeviceMenuAction>>>,
    }

    impl Render for DeviceMenuHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let actions = self.actions.clone();
            let data = DeviceMenuData::from_record(&self.record);
            let action_handler: DeviceMenuActionHandler =
                Rc::new(move |action, _window, _cx| actions.borrow_mut().push(action));
            div().size(px(100.)).child(device_menu_button_with_handler(
                data,
                theme::palette(cx),
                action_handler,
            ))
        }
    }

    impl Render for NestedDeviceMenuHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let actions = self.actions.clone();
            let data = DeviceMenuData::from_record(&self.record);
            let action_handler: DeviceMenuActionHandler =
                Rc::new(move |action, _window, _cx| actions.borrow_mut().push(action));
            let pal = theme::palette(cx);
            let card = BaseButton::new("device-card-test").size_full().child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .w(px(40.))
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .debug_selector(|| "device-menu-trigger-test".into())
                    .child(device_menu_button_with_handler(
                        data.clone(),
                        pal,
                        action_handler,
                    )),
            );
            div()
                .relative()
                .size(px(240.))
                .child(device_context_menu_with_handler(
                    card,
                    data,
                    pal,
                    Rc::new(|_, _, _| {}),
                ))
        }
    }

    fn test_device_record() -> DeviceRecord {
        DeviceRecord {
            config_key: "receiver:test:slot:1".into(),
            canonical_key: Some("receiver:test:slot:1".into()),
            persistent: true,
            route_key: "receiver:test:slot:1".into(),
            model_key: "test-mouse".into(),
            model_name: "Test Mouse".into(),
            display_name: "Test Mouse".into(),
            asset: None,
            model_info: None,
            codename: None,
            serial_number: None,
            unit_id: [0; 4],
            driver_id: None,
            registry_model_id: None,
            route: None,
            capture_id: None,
            kind: DeviceKind::Mouse,
            capabilities: Some(Capabilities::default()),
            light_capabilities: None,
            slot: 1,
            online: true,
            battery: None,
        }
    }

    fn add_menu_window(
        cx: &mut TestAppContext,
        record: DeviceRecord,
        actions: Rc<RefCell<Vec<DeviceMenuAction>>>,
    ) -> &mut gpui::VisualTestContext {
        cx.add_window_view(move |_, _| DeviceMenuHarness { record, actions })
            .1
    }

    fn add_nested_menu_window(
        cx: &mut TestAppContext,
        record: DeviceRecord,
        actions: Rc<RefCell<Vec<DeviceMenuAction>>>,
    ) -> &mut gpui::VisualTestContext {
        cx.add_window_view(move |_, _| NestedDeviceMenuHarness { record, actions })
            .1
    }

    fn draw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn open_menu(cx: &mut gpui::VisualTestContext) {
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        draw(cx);
        assert!(
            cx.debug_bounds("device-menu-rename-row").is_some(),
            "the device menu opens from the ellipsis button"
        );
    }

    #[gpui::test]
    fn device_menu_rename_responds_to_mouse_click(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let actions = Rc::new(RefCell::new(Vec::new()));
        let visual_cx = add_menu_window(cx, test_device_record(), actions.clone());
        draw(visual_cx);
        open_menu(visual_cx);
        let rename_bounds = visual_cx
            .debug_bounds("device-menu-rename-row")
            .expect("the rename row renders in the device menu");
        visual_cx.simulate_click(rename_bounds.center(), Modifiers::default());
        draw(visual_cx);

        assert!(
            visual_cx.debug_bounds("device-menu-rename-row").is_none(),
            "activating Rename must close the device menu"
        );
        assert!(
            actions.borrow().as_slice() == [DeviceMenuAction::Rename],
            "clicking Rename must dispatch the rename action"
        );
    }

    #[gpui::test]
    fn device_menu_rename_responds_when_only_mouse_down_reaches_the_row(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let actions = Rc::new(RefCell::new(Vec::new()));
        let visual_cx = add_menu_window(cx, test_device_record(), actions.clone());
        draw(visual_cx);
        open_menu(visual_cx);
        let rename_bounds = visual_cx
            .debug_bounds("device-menu-rename-row")
            .expect("the rename row renders in the device menu");

        visual_cx.simulate_mouse_down(
            rename_bounds.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        draw(visual_cx);

        assert_eq!(
            actions.borrow().as_slice(),
            [DeviceMenuAction::Rename],
            "the Rename action must not depend on a later Click event"
        );
        assert!(visual_cx.debug_bounds("device-menu-rename-row").is_none());
    }

    #[gpui::test]
    fn device_menu_rename_supports_keyboard_activation(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let actions = Rc::new(RefCell::new(Vec::new()));
        let visual_cx = add_menu_window(cx, test_device_record(), actions.clone());
        draw(visual_cx);
        open_menu(visual_cx);
        visual_cx.simulate_keystrokes("enter");
        draw(visual_cx);

        assert_eq!(
            actions.borrow().as_slice(),
            [DeviceMenuAction::Rename],
            "the focused Rename row must activate from Enter"
        );
        assert!(visual_cx.debug_bounds("device-menu-rename-row").is_none());
    }

    #[gpui::test]
    fn device_menu_rename_responds_inside_the_real_card_context_menu(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let actions = Rc::new(RefCell::new(Vec::new()));
        let visual_cx = add_nested_menu_window(cx, test_device_record(), actions.clone());
        draw(visual_cx);

        let trigger_bounds = visual_cx
            .debug_bounds("device-menu-trigger-test")
            .expect("the device card exposes the ellipsis trigger");
        visual_cx.simulate_click(trigger_bounds.center(), Modifiers::default());
        draw(visual_cx);
        let rename_bounds = visual_cx
            .debug_bounds("device-menu-rename-row")
            .expect("the device menu opens inside the card context-menu wrapper");
        visual_cx.simulate_click(rename_bounds.center(), Modifiers::default());
        draw(visual_cx);

        assert_eq!(
            actions.borrow().as_slice(),
            [DeviceMenuAction::Rename],
            "clicking Rename must work when the ellipsis is nested in the real card button"
        );
        assert!(visual_cx.debug_bounds("device-menu-rename-row").is_none());
    }

    #[gpui::test]
    fn device_menu_supports_escape_and_outside_dismissal(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let actions = Rc::new(RefCell::new(Vec::new()));
        let visual_cx = add_menu_window(cx, test_device_record(), actions.clone());
        draw(visual_cx);

        open_menu(visual_cx);
        visual_cx.simulate_keystrokes("escape");
        draw(visual_cx);
        assert!(visual_cx.debug_bounds("device-menu-rename-row").is_none());

        open_menu(visual_cx);
        visual_cx.simulate_click(point(px(300.), px(300.)), Modifiers::default());
        draw(visual_cx);
        assert!(visual_cx.debug_bounds("device-menu-rename-row").is_none());
    }

    #[gpui::test]
    fn device_menu_online_devices_hide_delete(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let visual_cx =
            add_menu_window(cx, test_device_record(), Rc::new(RefCell::new(Vec::new())));
        draw(visual_cx);
        open_menu(visual_cx);
        assert!(
            visual_cx.debug_bounds("device-menu-delete-row").is_none(),
            "online devices must not offer deletion"
        );
    }

    #[gpui::test]
    fn device_menu_offline_persistent_devices_show_delete(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut offline_record = test_device_record();
        offline_record.online = false;
        let offline_actions = Rc::new(RefCell::new(Vec::new()));
        let offline_cx = add_menu_window(cx, offline_record, offline_actions.clone());
        draw(offline_cx);
        open_menu(offline_cx);
        let delete_bounds = offline_cx
            .debug_bounds("device-menu-delete-row")
            .expect("offline persistent devices must offer deletion");
        offline_cx.simulate_click(delete_bounds.center(), Modifiers::default());
        draw(offline_cx);
        assert_eq!(
            offline_actions.borrow().as_slice(),
            [DeviceMenuAction::Delete]
        );
        assert!(offline_cx.debug_bounds("device-menu-delete-row").is_none());
    }

    #[gpui::test]
    fn device_menu_nonpersistent_devices_hide_delete(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut transient_record = test_device_record();
        transient_record.online = false;
        transient_record.persistent = false;
        let transient_actions = Rc::new(RefCell::new(Vec::new()));
        let transient_cx = add_menu_window(cx, transient_record, transient_actions);
        draw(transient_cx);
        open_menu(transient_cx);
        assert!(
            transient_cx
                .debug_bounds("device-menu-delete-row")
                .is_none(),
            "non-persistent devices must not offer deletion"
        );
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
