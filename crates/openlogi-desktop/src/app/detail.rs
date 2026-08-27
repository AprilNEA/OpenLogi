//! The device-detail screen: the header (back + name + section tabs), and the
//! section bodies (Buttons, Keys, Pointer, Lighting, Camera, Device).

use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Rems, Role,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rems,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Disableable as _, Icon, IconName, Selectable as _,
    button::{Button, ButtonGroup},
    description_list::{DescriptionItem, DescriptionList},
    h_flex,
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};
use openlogi_core::config::ScrollResolution;
use openlogi_core::device::DeviceKind;
use openlogi_core::hid::DeviceRoute;

use super::widgets::{back_button, kind_label, route_label, sidebar_action, status_badge};
use super::{AppView, DetailTab};
use crate::app::menu::file_url;
use crate::features::action_ring::ActionRingPanel;
use crate::features::camera::controls::CameraControlsPanel;
use crate::features::camera::preview::CameraPreview;
use crate::features::keyboard::function_row::FunctionRowView;
use crate::features::lighting::device::LightingPanel;
use crate::features::lighting::standalone::LightPanel;
use crate::features::lighting::visual as light_visual;
use crate::features::mouse::view::MouseModelView;
use crate::features::pointer::dpi::DpiPanel;
use crate::features::pointer::smartshift::SmartShiftPanel;
use crate::features::profile_scope::{AppCatalogPicker, ProfileIconCache, profile_scope_bar};
use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::battery::BatteryIndicator;
use crate::ui::components::{PanelCard, Toggle};
use crate::ui::theme::{
    self, ContentWidth, DETAIL_RAIL_W, HEADER_H, Palette, SCREEN_PAD, Typography as _,
};

const CAMERA_PREVIEW_W: Rems = rems(32.125);
const CAMERA_CONTROLS_W: Rems = rems(31.25);
const LIGHT_CONTROLS_W: Rems = rems(25.);
const LIGHT_CONTROLS_MIN_W: Rems = rems(22.5);
const POINTER_CARD_MIN_W: Rems = rems(20.75);

/// Compact device identity bar. Section navigation belongs to the workspace
/// rail below; pairing belongs to the Devices screen, so neither competes with
/// the device name and status here.
pub(super) fn detail_header(
    record: Option<&DeviceRecord>,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let pal = theme::palette(cx);
    let name = record.map_or_else(|| tr!("Device").to_string(), |r| r.display_name.clone());
    let online = record.map(|r| r.online);
    let battery = record
        .and_then(|r| r.battery.as_ref())
        .map(BatteryIndicator::inline);
    h_flex()
        .h(px(HEADER_H))
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
        .child(div().flex_1())
        .children(battery)
        .when_some(online, |this, online| this.child(status_badge(online, pal)))
}

/// Long-lived child panels rendered by the device workspace.
pub(super) struct DetailPanels<'a> {
    pub mouse_model: &'a gpui::Entity<MouseModelView>,
    pub action_ring: &'a gpui::Entity<ActionRingPanel>,
    pub keyboard_model: &'a gpui::Entity<FunctionRowView>,
    pub dpi_panel: &'a gpui::Entity<DpiPanel>,
    pub smartshift_panel: &'a gpui::Entity<SmartShiftPanel>,
    pub lighting_panel: &'a gpui::Entity<LightingPanel>,
    pub camera_preview: &'a gpui::Entity<CameraPreview>,
    pub camera_controls: &'a gpui::Entity<CameraControlsPanel>,
    pub light_panel: &'a gpui::Entity<LightPanel>,
}

/// The device-detail workspace below the identity bar: stable navigation rail
/// beside the active section. `active` arrives pre-resolved against this
/// device's tab set.
pub(super) fn detail_content(
    panels: &DetailPanels<'_>,
    profile_icons: &ProfileIconCache,
    app_catalog: &gpui::Entity<AppCatalogPicker>,
    tabs: &[DetailTab],
    active: DetailTab,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let pal = theme::palette(cx);
    let online = AppState::try_read(cx)
        .and_then(AppState::current_record)
        .is_some_and(|record| record.online);
    let content = match active {
        DetailTab::Buttons => {
            buttons_tab(panels.mouse_model, profile_icons, app_catalog, cx).into_any_element()
        }
        DetailTab::ActionsRing => action_ring_tab(panels.action_ring).into_any_element(),
        DetailTab::Keys => keys_tab(panels.keyboard_model).into_any_element(),
        DetailTab::Pointer => {
            pointer_tab(panels.dpi_panel, panels.smartshift_panel, cx).into_any_element()
        }
        DetailTab::Lighting => lighting_tab(panels.lighting_panel).into_any_element(),
        DetailTab::Camera => {
            camera_tab(panels.camera_preview, panels.camera_controls).into_any_element()
        }
        DetailTab::Light => light_tab(panels.light_panel, cx).into_any_element(),
        DetailTab::Device => device_tab(cx).into_any_element(),
    };
    let navigation = detail_navigation(tabs, active, cx);
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
                    .bg(pal.panel)
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
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_stretch()
                .bg(pal.page)
                .child(navigation)
                .child(content),
        )
}

/// Stable workspace navigation. Keeping the section names visible makes the
/// device page scan like a settings workspace rather than a toolbar full of
/// modes, while the selected fill supplies one strong location signal.
fn detail_navigation(
    tabs: &[DetailTab],
    active: DetailTab,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let pal = theme::palette(cx);
    v_flex()
        .w(px(DETAIL_RAIL_W))
        .h_full()
        .flex_shrink_0()
        .gap_1()
        .border_r_1()
        .border_color(pal.border)
        .bg(pal.panel)
        .p_3()
        .children(tabs.iter().copied().enumerate().map(|(index, tab)| {
            let selected = tab == active;
            BaseButton::new(("detail-navigation", index))
                .role(Role::Tab)
                .selected(selected)
                .accessibility_label(tab.label())
                .aria_selected(selected)
                .w_full()
                .flex()
                .items_center()
                .gap_2p5()
                .px_3()
                .py_2()
                .rounded(pal.control_radius)
                .cursor_pointer()
                .text_body()
                .text_color(if selected {
                    pal.text_primary
                } else {
                    pal.text_muted
                })
                .when(selected, |row| row.bg(crate::ui::theme::accent_tint()))
                .hover(move |row| {
                    row.bg(if selected {
                        crate::ui::theme::accent_tint_hover()
                    } else {
                        pal.control_hover
                    })
                })
                .focus_visible(move |row| {
                    row.bg(if selected {
                        crate::ui::theme::accent_tint_hover()
                    } else {
                        pal.control_hover
                    })
                })
                .child(
                    Icon::empty()
                        .path(detail_tab_icon(tab))
                        .size_4()
                        .flex_none(),
                )
                .child(tab.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.active_tab = tab;
                    cx.notify();
                }))
        }))
}

fn detail_tab_icon(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Buttons => "action-icons/mouse-pointer-click.svg",
        DetailTab::ActionsRing => "action-icons/layout-grid.svg",
        DetailTab::Keys => "action-icons/keyboard.svg",
        DetailTab::Pointer => "action-icons/gauge.svg",
        DetailTab::Lighting | DetailTab::Light => "action-icons/palette.svg",
        DetailTab::Camera => "action-icons/camera.svg",
        DetailTab::Device => "action-icons/settings.svg",
    }
}

/// Buttons tab: profile context above the selectable device canvas and fixed
/// binding inspector.
fn buttons_tab(
    mouse_model: &gpui::Entity<MouseModelView>,
    profile_icons: &ProfileIconCache,
    app_catalog: &gpui::Entity<AppCatalogPicker>,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .children(profile_scope_bar(profile_icons, app_catalog, cx))
        .child(mouse_model.clone())
}

fn tab_body(
    width: ContentWidth,
    content: impl IntoElement,
) -> gpui_component::scroll::Scrollable<gpui::Div> {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p(SCREEN_PAD.rems())
        .child(div().w_full().max_w(width.rems()).child(content))
}

/// Keys tab: the function-row remapper for a keyboard.
fn keys_tab(keyboard_model: &gpui::Entity<FunctionRowView>) -> impl IntoElement {
    tab_body(ContentWidth::DoubleExtraLarge, keyboard_model.clone()).justify_center()
}

fn action_ring_tab(panel: &gpui::Entity<ActionRingPanel>) -> impl IntoElement {
    tab_body(ContentWidth::Medium, panel.clone())
}

/// Pointer tab: the DPI panel, the SmartShift wheel controls, and the
/// scroll-wheel preferences, each in a titled card. Use a responsive two-column
/// grid that still fits the window's 720 px minimum width, so these short
/// controls don't force a vertical scroll.
fn pointer_tab(
    dpi_panel: &gpui::Entity<DpiPanel>,
    smartshift_panel: &gpui::Entity<SmartShiftPanel>,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let pal = theme::palette(cx);
    tab_body(
        ContentWidth::Large,
        h_flex()
            .w_full()
            .items_stretch()
            .gap_4()
            .flex_wrap()
            .child(pointer_grid_card(
                PanelCard::new(
                    tr!("Pointer tuning"),
                    Icon::empty().path("action-icons/gauge.svg"),
                    dpi_panel.clone().into_any_element(),
                )
                .fill(),
            ))
            .child(pointer_grid_card(
                PanelCard::new(
                    tr!("SmartShift"),
                    Icon::empty().path("action-icons/refresh-cw.svg"),
                    smartshift_panel.clone().into_any_element(),
                )
                .fill(),
            ))
            .child(
                div()
                    .min_w(POINTER_CARD_MIN_W)
                    .flex_1()
                    .child(scrolling_card(pal, cx)),
            ),
    )
}

fn pointer_grid_card(card: impl IntoElement) -> impl IntoElement {
    // At 100%, two cards plus one 16 px gap fit exactly inside the 720 px
    // window minimum after this tab's 20 px side insets, while still leaving a
    // usable slider: 332·2 + 16 + 20·2 = 720. In rems, the whole relationship
    // scales together.
    div()
        .min_w(POINTER_CARD_MIN_W)
        .flex_1()
        .h_full()
        .child(card)
}

/// What the scrolling card shows for the selected device — `Default` is the
/// no-device (or unreadable-state) blank.
#[derive(Default)]
struct ScrollingFacts {
    /// The persisted inversion setting. Independent of
    /// `inversion_supported` — a configured inversion on a link without
    /// support still renders, checked and disabled — so these are two named
    /// fields, not a sum type.
    inverted: bool,
    /// Whether the current link reports HID++ inversion support.
    inversion_supported: bool,
    resolution: Option<openlogi_core::config::ScrollResolution>,
    hires: HiresWheel,
}

/// Where the device offers hi-res wheel control.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum HiresWheel {
    /// On the current link.
    Here,
    /// Only on another of its links.
    Elsewhere,
    /// Nowhere.
    #[default]
    Nowhere,
}

/// Scrolling card: per-device native inversion and wheel-resolution controls.
/// Pure config — no hardware read — so it is a plain settings block rather than
/// an `Entity` panel like DPI / SmartShift.
fn scrolling_card(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let ScrollingFacts {
        inverted,
        inversion_supported,
        resolution,
        hires,
    } = AppState::try_read(cx).map_or_else(ScrollingFacts::default, |state| ScrollingFacts {
        inverted: state.current_invert_scroll(),
        inversion_supported: state.current_scroll_inversion_supported(),
        resolution: state.current_scroll_resolution(),
        hires: if state.current_hires_wheel_supported() {
            HiresWheel::Here
        } else if state.hires_wheel_supported_on_another_link() {
            HiresWheel::Elsewhere
        } else {
            HiresWheel::Nowhere
        },
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
        .child(
            Toggle::new("invert-scroll-toggle")
                .selected(inverted)
                .disabled(!inversion_supported)
                .label((!inversion_supported).then(|| tr!("Unavailable")))
                .on_change(|inverted, _window, cx| {
                    AppState::update(cx, |state, cx| {
                        let key = state.current_record().map(DeviceRecord::device_key);
                        state.commit_invert_scroll(*inverted);
                        if let Some(key) = key {
                            cx.emit(StateEvent::DeviceConfigChanged(key));
                        }
                    });
                }),
        );
    let resolution_description = match hires {
        HiresWheel::Here => match resolution {
            None => tr!("OpenLogi does not change the wheel resolution."),
            Some(ScrollResolution::Low) => tr!("Scrolls once per physical ratchet step."),
            Some(ScrollResolution::High) => {
                tr!("Detects finer movement between ratchet steps.")
            }
        },
        HiresWheel::Elsewhere => {
            tr!("This device supports wheel resolution on its other connection, but not this one.")
        }
        HiresWheel::Nowhere => tr!("This device does not support wheel resolution control."),
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
        .child(wheel_resolution_control(
            resolution,
            hires == HiresWheel::Here,
        ));
    PanelCard::new(
        tr!("Scrolling"),
        Icon::empty().path("action-icons/mouse.svg"),
        v_flex().gap_4().child(inversion_row).child(resolution_row),
    )
}

fn wheel_resolution_control(selected: Option<ScrollResolution>, enabled: bool) -> impl IntoElement {
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
            AppState::update(cx, |state, cx| {
                let key = state.current_record().map(DeviceRecord::device_key);
                state.commit_scroll_resolution(*value);
                if let Some(key) = key {
                    cx.emit(StateEvent::DeviceConfigChanged(key));
                }
            });
        })
}

/// Lighting tab: the RGB controls (swatches, on/off, brightness) in a titled
/// card. Shown when the device reports a lighting capability — see
/// [`DetailTab::tabs_for`].
fn lighting_tab(lighting_panel: &gpui::Entity<LightingPanel>) -> impl IntoElement {
    tab_body(
        ContentWidth::Small,
        PanelCard::new(
            tr!("Lighting"),
            Icon::new(IconName::Palette),
            lighting_panel.clone().into_any_element(),
        ),
    )
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
) -> impl IntoElement {
    tab_body(
        ContentWidth::DoubleExtraLarge,
        h_flex()
            .w_full()
            .flex_wrap()
            .justify_center()
            .items_start()
            .gap_3()
            .child(
                div()
                    .w(CAMERA_PREVIEW_W)
                    .flex_shrink_0()
                    .child(PanelCard::new(
                        tr!("Camera"),
                        Icon::new(IconName::Eye),
                        camera_preview.clone().into_any_element(),
                    )),
            )
            .child(
                div()
                    .w(CAMERA_CONTROLS_W)
                    .flex_shrink_0()
                    .child(PanelCard::new(
                        tr!("Camera controls"),
                        Icon::new(IconName::Settings),
                        camera_controls.clone().into_any_element(),
                    )),
            ),
    )
}

/// Standalone-light controls in a separate panel from HID++ keyboard RGB.
fn light_tab(
    light_panel: &gpui::Entity<LightPanel>,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let pal = theme::palette(cx);
    let (asset, online, enabled, settings) = AppState::try_read(cx).map_or_else(
        || {
            (
                None,
                false,
                false,
                openlogi_core::config::LightSettings::default(),
            )
        },
        |state| {
            let record = state.current_record();
            (
                record.and_then(|record| record.asset.as_ref()),
                record.is_some_and(|record| record.online),
                state.light_enabled(),
                state.light(),
            )
        },
    );
    tab_body(
        ContentWidth::ExtraLarge,
        h_flex()
            .w_full()
            .gap_4()
            .flex_wrap()
            .items_start()
            .child(light_visual::detail(
                asset,
                light_visual::LightView { online, enabled },
                settings,
                pal,
            ))
            .child(
                div()
                    .w(LIGHT_CONTROLS_W)
                    .min_w(LIGHT_CONTROLS_MIN_W)
                    .child(PanelCard::new(
                        tr!("Lighting"),
                        Icon::new(IconName::Sun),
                        light_panel.clone().into_any_element(),
                    )),
            ),
    )
}

/// Device tab: device details and configuration cards stacked.
fn device_tab(cx: &mut Context<AppView>) -> impl IntoElement {
    let pal = theme::palette(cx);
    tab_body(
        ContentWidth::Small,
        v_flex()
            .w_full()
            .gap_3()
            .child(device_details_card(pal, cx))
            .child(configuration_card(pal, cx)),
    )
}

fn device_details_card(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let content = AppState::try_read(cx)
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
                        &record.model_name,
                        record.kind,
                        record.online,
                        pal,
                    ))
                    .when_some(record.battery.as_ref(), |this, battery| {
                        this.child(BatteryIndicator::summary(battery))
                    })
                    .child(device_description_list(record))
                    .into_any_element()
            },
        );

    PanelCard::new(tr!("Device details"), Icon::new(IconName::Info), content)
}

fn configuration_card(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let device_enabled = AppState::try_read(cx)
        .and_then(|state| {
            state
                .current_record()
                .map(|r| state.device_enabled(&r.config_key))
        })
        .unwrap_or(true);
    let (binding_count, gesture_count, preset_count, app_profile) = AppState::try_read(cx)
        .map_or_else(
            || (0, 0, 0, tr!("Default profile").to_string()),
            |state| {
                (
                    state.button_bindings().len(),
                    // Device-level, not scope-level: this card describes the
                    // device, and a per-app profile holds no gestures at all.
                    state.device_gesture_binding_count(),
                    state.dpi_presets().len(),
                    state
                        .active_profile_name()
                        .map_or_else(|| tr!("Default profile").to_string(), str::to_owned),
                )
            },
        );

    let content = v_flex()
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    v_flex()
                        .child(div().text_body().child(tr!("Manage this device")))
                        .child(div().text_caption().text_color(pal.text_muted).child(tr!(
                            "Off leaves every control native and stops re-applying settings."
                        ))),
                )
                .child(
                    Switch::new("device-enabled")
                        .checked(device_enabled)
                        .on_click(|checked, _window, cx| {
                            let enabled = *checked;
                            AppState::update(cx, |state, cx| {
                                let record = state
                                    .current_record()
                                    .map(|record| (record.config_key.clone(), record.device_key()));
                                if let Some((config_key, event_key)) = record {
                                    state.set_device_enabled(&config_key, enabled);
                                    cx.emit(StateEvent::DeviceConfigChanged(event_key));
                                }
                            });
                        }),
                ),
        )
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
        );

    PanelCard::new(tr!("Configuration"), Icon::new(IconName::Folder), content)
}

fn device_summary(
    name: &str,
    model_name: &str,
    kind: DeviceKind,
    online: bool,
    pal: Palette,
) -> impl IntoElement {
    let subtitle = if name == model_name {
        kind_label(kind)
    } else {
        format!("{model_name} · {}", kind_label(kind))
    };
    h_flex()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .gap_1()
                .flex_1()
                .min_w_0()
                .child(div().text_subheading().child(name.to_string()))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(subtitle),
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
    if matches!(
        record.route,
        Some(DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. })
    ) {
        items.push(DescriptionItem::new(tr!("Channel")).value(record.slot.to_string()));
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
