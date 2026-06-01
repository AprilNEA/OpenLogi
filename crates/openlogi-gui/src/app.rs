use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled, Subscription,
    Window, div, px, rgb,
};
use gpui_component::{Icon, IconName, h_flex, v_flex};
use openlogi_core::config::Config;
use openlogi_core::device::DeviceInventory;
use tracing::{info, warn};

use crate::app_menu::{Minimize, Zoom};
use crate::asset::AssetResolver;
use crate::components::device_carousel::DeviceCarousel;
use crate::components::dpi_panel::DpiPanel;
use crate::mouse_model::view::MouseModelView;
use crate::state::AppState;
use crate::theme::{self, FOOTER_H, HEADER_H, Palette};

/// Root application view.
pub struct AppView {
    carousel: Entity<DeviceCarousel>,
    mouse_model: Entity<MouseModelView>,
    dpi_panel: Entity<DpiPanel>,
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
    /// Re-renders the root when the device list changes so the empty state
    /// swaps to the device UI (and back) on hot-plug, without a restart.
    #[allow(dead_code, reason = "held to keep the AppState observer alive")]
    state_obs: Subscription,
    accessibility_dismissed: bool,
}

impl AppView {
    /// Construct the root view and its child entities.
    pub fn new(inventories: &[DeviceInventory], cx: &mut Context<Self>) -> Self {
        let config = match Config::load_or_default() {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "could not load config.toml — starting with defaults");
                Config::default()
            }
        };

        let cache = AssetResolver::new();

        if !cx.has_global::<AppState>() {
            cx.set_global(AppState::with_runtime(config, inventories, &cache));
        }

        if let Some(state) = cx.try_global::<AppState>() {
            if let Some(record) = state.current_record() {
                info!(
                    device_key = %record.config_key,
                    display = %record.display_name,
                    "initial device selected"
                );
            } else {
                info!(
                    root = ?cache.cache_root(),
                    "no devices with HID++ model info — using synthetic silhouette"
                );
            }
        }

        let carousel = cx.new(DeviceCarousel::new);
        let mouse_model = cx.new(MouseModelView::new);
        let dpi_panel = cx.new(DpiPanel::new);
        let state_obs = cx.observe_global::<AppState>(|_, cx| cx.notify());
        Self {
            carousel,
            mouse_model,
            dpi_panel,
            appearance_obs: None,
            state_obs,
            accessibility_dismissed: false,
        }
    }

    /// Keep the OS-appearance observer alive.
    pub fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }

    fn accessibility_gate(pal: Palette, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .items_center()
            .justify_center()
            .gap_4()
            .p_8()
            .child(
                Icon::new(IconName::TriangleAlert)
                    .size_8()
                    .text_color(rgb(theme::STATUS_CONNECTING)),
            )
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr!("Accessibility permission required")),
            )
            .child(
                div()
                    .max_w(px(440.))
                    .text_sm()
                    .text_color(pal.text_muted)
                    .child(tr!(
                        "OpenLogi captures mouse buttons (Back / Forward / gesture button) \
                         through the system Accessibility permission and runs the actions you \
                         bind. Features that talk to the device directly — DPI, SmartShift — \
                         are unaffected."
                    )),
            )
            .child(
                div()
                    .id("open-accessibility")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(theme::ACCENT_BLUE))
                    .text_color(rgb(0x00ff_ffff))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::Settings))
                            .child(tr!("Open System Settings to grant access")),
                    )
                    .on_click(|_, _, _| open_accessibility_settings()),
            )
            .child(div().text_xs().text_color(pal.text_muted).child(tr!(
                "Takes effect automatically once granted — no restart needed."
            )))
            .child(
                div()
                    .id("skip-accessibility")
                    .text_xs()
                    .text_color(pal.text_muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(pal.text_primary))
                    .child(tr!("Not now (use DPI and other features only)"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.accessibility_dismissed = true;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

fn open_accessibility_settings() {
    use crate::platform::permissions::{self, Permission};
    // Single source of the prompt + System Settings deep link, shared with the
    // Settings window's Permissions row.
    permissions::open_pane(Permission::Accessibility);
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);

        let granted = cx
            .try_global::<AppState>()
            .is_none_or(|s| s.accessibility_granted);
        if !granted && !self.accessibility_dismissed {
            return Self::accessibility_gate(pal, cx);
        }

        let has_device = cx
            .try_global::<AppState>()
            .is_some_and(|s| !s.device_list.is_empty());
        let scanning = cx.try_global::<AppState>().is_some_and(|s| s.scanning);
        let body = if has_device {
            body(&self.mouse_model, &self.dpi_panel).into_any_element()
        } else {
            device_empty_state(pal, scanning)
        };

        v_flex()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .child(header(&self.carousel, pal))
            .child(body)
            .child(footer(pal, granted))
            .into_any_element()
    }
}

fn header(carousel: &Entity<DeviceCarousel>, pal: Palette) -> impl IntoElement {
    h_flex()
        .h(px(HEADER_H))
        .w_full()
        .px_5()
        .gap_4()
        .items_center()
        .border_b_1()
        .border_color(pal.border)
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child("OpenLogi"),
        )
        .child(div().flex_1().min_w_0().child(carousel.clone()))
}

fn body(mouse_model: &Entity<MouseModelView>, dpi_panel: &Entity<DpiPanel>) -> impl IntoElement {
    h_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_start()
        .justify_center()
        .gap_6()
        .p_6()
        .child(mouse_model.clone())
        .child(dpi_panel.clone())
}

/// Body shown when no device is connected. The inventory watcher keeps polling
/// (every 2 s) and `AppView`'s `AppState` observer swaps the device UI back in
/// the moment one appears, so this is purely a wait-and-pair placeholder.
fn device_empty_state(pal: Palette, scanning: bool) -> AnyElement {
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
                .font_weight(FontWeight::SEMIBOLD)
                .child(if scanning {
                    tr!("Scanning for devices…")
                } else {
                    tr!("No device connected")
                }),
        )
        .child(
            div()
                .max_w(px(440.))
                .text_sm()
                .text_color(pal.text_muted)
                .child(tr!(
                    "Plug in or pair a supported Logitech device — it'll show up here automatically."
                )),
        )
        .child(
            div()
                .id("empty-add-device")
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(theme::ACCENT_BLUE))
                .text_color(rgb(0x00ff_ffff))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::Plus))
                        .child(tr!("Add Device")),
                )
                .on_click(|_, _, cx| crate::windows::add_device::open(cx)),
        )
        .child(div().max_w(px(440.)).text_xs().text_color(pal.text_muted).child(tr!(
            "Using Logi Options+? Quit it first — both apps compete for HID++ access."
        )))
        .into_any_element()
}

fn footer(pal: Palette, granted: bool) -> impl IntoElement {
    h_flex()
        .h(px(FOOTER_H))
        .w_full()
        .px_5()
        .gap_4()
        .items_center()
        .justify_between()
        .border_t_1()
        .border_color(pal.border)
        .child(
            h_flex()
                .gap_2()
                .text_xs()
                .text_color(pal.text_muted)
                .child(
                    div()
                        .id("footer-add-device")
                        .cursor_pointer()
                        .hover(|s| s.text_color(pal.text_primary))
                        .child(footer_link(IconName::Plus, tr!("Add Device")))
                        .on_click(|_, _, cx| crate::windows::add_device::open(cx)),
                )
                .child(div().child("·"))
                .child(
                    div()
                        .id("footer-settings")
                        .cursor_pointer()
                        .hover(|s| s.text_color(pal.text_primary))
                        .child(footer_link(IconName::Settings, tr!("Settings")))
                        .on_click(|_, _, cx| crate::windows::settings::open(cx)),
                )
                .child(div().child("·"))
                .child(
                    div()
                        .id("footer-about")
                        .cursor_pointer()
                        .hover(|s| s.text_color(pal.text_primary))
                        .child(footer_link(IconName::Info, tr!("About")))
                        .on_click(|_, _, cx| crate::windows::about::open(cx)),
                ),
        )
        .child(accessibility_status(pal, granted))
        .child(
            div()
                .text_xs()
                .text_color(pal.text_muted)
                .child(concat!("v", env!("CARGO_PKG_VERSION"))),
        )
}

/// A footer link's content: a small leading icon plus its label, laid out
/// inline. The hover colour is inherited from the clickable wrapper, so this
/// only describes the static row.
fn footer_link(icon: IconName, label: SharedString) -> impl IntoElement {
    h_flex()
        .gap_1()
        .items_center()
        .child(Icon::new(icon))
        .child(label)
}

/// Footer Accessibility-permission indicator. Granted → a muted green-dot
/// status; not granted → an amber-dot affordance that requests the grant on
/// click (the native prompt + System Settings, via [`open_accessibility_settings`]).
fn accessibility_status(pal: Palette, granted: bool) -> AnyElement {
    let dot = |color: u32| div().size_2().rounded_full().bg(rgb(color));
    if granted {
        h_flex()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(pal.text_muted)
            .child(dot(theme::STATUS_CONNECTED))
            .child(div().child(tr!("Accessibility granted")))
            .into_any_element()
    } else {
        h_flex()
            .id("footer-accessibility")
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(pal.text_primary)
            .cursor_pointer()
            .child(dot(theme::STATUS_CONNECTING))
            .child(div().child(tr!("Accessibility not granted · click to grant")))
            .on_click(|_, _, _| open_accessibility_settings())
            .into_any_element()
    }
}
