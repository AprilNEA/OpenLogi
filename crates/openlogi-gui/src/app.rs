use gpui::{
    AnyElement, AppContext as _, BorrowAppContext as _, BoxShadow, Context, Div, Entity,
    FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div, img, point,
    prelude::FluentBuilder as _, px, relative, rgb,
};
use gpui_component::{
    Icon, IconName,
    description_list::{DescriptionItem, DescriptionList},
    h_flex,
    scroll::ScrollableElement as _,
    tab::TabBar,
    tooltip::Tooltip,
    v_flex,
};
use openlogi_core::config::Config;
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory, DeviceKind,
};
use openlogi_hid::DeviceRoute;
use tracing::{info, warn};

use crate::app_menu::{Minimize, Zoom};
use crate::asset::AssetResolver;
use crate::components::carousel::Carousel;
use crate::components::dpi_panel::DpiPanel;
use crate::components::lighting_panel::LightingPanel;
use crate::components::smartshift_panel::SmartShiftPanel;
use crate::mouse_model::view::MouseModelView;
use crate::state::{AppState, DeviceRecord};
use crate::theme::{self, FOOTER_H, HEADER_H, Palette};

/// Which screen the root view is showing.
///
/// GPUI has no router, so navigation is a tiny view-local enum that selects
/// which subtree [`AppView::render`] builds. It is deliberately *not* in
/// [`AppState`]: the route is pure UI presentation, whereas
/// [`AppState::current_device`] is functional (it drives the hook bindings,
/// DPI, and persisted selection). The detail route is keyed by `config_key`
/// rather than an index so a hot-plug that reorders or drops the device list
/// can't silently swap the user onto a different device's settings — render
/// validates the key against the live selection and pops back to [`Route::Home`]
/// when it no longer matches.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    /// The device gallery.
    Home,
    /// A single device's settings, identified by its stable config key.
    Device { config_key: String },
}

/// The active section of the device-detail screen. Backs the detail `TabBar`;
/// reset to the device's first tab whenever a device is opened.
///
/// The tab *set* depends on the device kind — see [`DetailTab::tabs_for`]. A
/// mouse gets button-mapping + pointer tuning; a wired keyboard gets RGB
/// lighting; every device gets the info tab. Tailoring the tabs is what keeps a
/// keyboard from rendering a mouse silhouette and an irrelevant DPI panel
/// (issue #19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    /// The mouse model with clickable button hotspots.
    Buttons,
    /// Pointer tuning — DPI and presets.
    Pointer,
    /// RGB lighting — color, brightness, on/off.
    Lighting,
    /// Device info and configuration.
    Device,
}

impl DetailTab {
    /// The detail sections shown for `record`, in tab order. Always non-empty:
    /// every device gets at least the info tab.
    fn tabs_for(record: &DeviceRecord) -> Vec<Self> {
        let mut tabs = Vec::new();
        if is_configurable_pointer(record.kind) {
            tabs.push(Self::Buttons);
            tabs.push(Self::Pointer);
        }
        if supports_lighting(record) {
            tabs.push(Self::Lighting);
        }
        tabs.push(Self::Device);
        tabs
    }

    /// The first (default) tab for `record` — what a freshly opened device shows.
    fn default_for(record: &DeviceRecord) -> Self {
        Self::tabs_for(record)
            .first()
            .copied()
            .unwrap_or(Self::Device)
    }

    fn label(self) -> SharedString {
        match self {
            Self::Buttons => tr!("Buttons"),
            Self::Pointer => tr!("Pointer"),
            Self::Lighting => tr!("Lighting"),
            Self::Device => tr!("Device"),
        }
    }
}

/// Whether a device drives the mouse model + DPI panel. Other kinds (keyboards,
/// numpads, headsets…) don't get a mouse silhouette that doesn't describe them
/// (issue #19); they fall back to the info tab — and, for wired keyboards, the
/// lighting tab.
fn is_configurable_pointer(kind: DeviceKind) -> bool {
    matches!(kind, DeviceKind::Mouse | DeviceKind::Trackball)
}

/// Whether to offer the RGB lighting tab. A `Keyboard` is always a keyboard;
/// wired G-series keyboards, though, enumerate as `Unknown` over the direct
/// (USB) path (it carries no codename or kind), so a direct-attached `Unknown`
/// counts too.
///
/// [`openlogi_hid::set_keyboard_color`] only drives the *wired* path today (it
/// opens a raw USB writer), so a Bolt/Unifying-paired keyboard's writes no-op
/// until a wireless lighting path lands — the tab still persists the config.
fn supports_lighting(record: &DeviceRecord) -> bool {
    matches!(record.kind, DeviceKind::Keyboard)
        || (matches!(record.kind, DeviceKind::Unknown)
            && matches!(record.route, Some(DeviceRoute::Direct { .. })))
}

/// Root application view.
pub struct AppView {
    route: Route,
    mouse_model: Entity<MouseModelView>,
    dpi_panel: Entity<DpiPanel>,
    smartshift_panel: Entity<SmartShiftPanel>,
    lighting_panel: Entity<LightingPanel>,
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
    /// Re-renders the root when the device list changes so the empty state
    /// swaps to the device UI (and back) on hot-plug, without a restart.
    #[allow(dead_code, reason = "held to keep the AppState observer alive")]
    state_obs: Subscription,
    accessibility_dismissed: bool,
    /// Which section of the device-detail screen is showing.
    active_tab: DetailTab,
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

        let mouse_model = cx.new(MouseModelView::new);
        let dpi_panel = cx.new(DpiPanel::new);
        let smartshift_panel = cx.new(SmartShiftPanel::new);
        let lighting_panel = cx.new(LightingPanel::new);
        let state_obs = cx.observe_global::<AppState>(|_, cx| cx.notify());
        Self {
            route: Route::Home,
            mouse_model,
            dpi_panel,
            smartshift_panel,
            lighting_panel,
            appearance_obs: None,
            state_obs,
            accessibility_dismissed: false,
            active_tab: DetailTab::Buttons,
        }
    }

    /// Keep the OS-appearance observer alive.
    pub fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }

    /// Drill into a device's settings from the gallery. Makes it the
    /// functionally active device too (hook bindings, DPI, and the persisted
    /// selection follow [`AppState::set_current_device`]) and switches the
    /// route to its detail screen.
    fn open_device(&mut self, config_key: String, cx: &mut Context<Self>) {
        cx.update_global::<AppState, _>(|state, _| {
            if let Some(idx) = state
                .device_list
                .iter()
                .position(|r| r.config_key == config_key)
            {
                state.set_current_device(idx);
            }
        });
        self.route = Route::Device { config_key };
        // Land on the device's first relevant tab — Buttons for a mouse,
        // Lighting for a wired keyboard, Device for everything else.
        self.active_tab = cx
            .try_global::<AppState>()
            .and_then(AppState::current_record)
            .map_or(DetailTab::Device, DetailTab::default_for);
        cx.notify();
    }

    /// Return to the device gallery. Leaves the active-device selection
    /// untouched — the route is purely presentational.
    fn go_home(&mut self, cx: &mut Context<Self>) {
        self.route = Route::Home;
        cx.notify();
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);

        let granted = cx
            .try_global::<AppState>()
            .is_none_or(|s| s.accessibility_granted);
        if !granted && !self.accessibility_dismissed {
            window.set_window_title("OpenLogi");
            return Self::accessibility_gate(pal, cx);
        }

        let has_device = cx
            .try_global::<AppState>()
            .is_some_and(|s| !s.device_list.is_empty());
        let scanning = cx.try_global::<AppState>().is_some_and(|s| s.scanning);

        // Resolve the route. A detail route lives only while its device is
        // still the live selection; if a hot-plug dropped or reordered it (or
        // the selection fell back to another device) pop quietly back to the
        // gallery rather than render a different device under the same screen.
        let show_device = match &self.route {
            Route::Home => false,
            Route::Device { config_key } => {
                cx.try_global::<AppState>()
                    .and_then(AppState::current_record)
                    .map(|r| r.config_key.as_str())
                    == Some(config_key.as_str())
            }
        };
        if !show_device {
            self.route = Route::Home;
        }

        window.set_window_title(&main_window_title(show_device, cx));

        let (header_el, content_el) = if show_device {
            (
                detail_header(pal, cx).into_any_element(),
                detail_content(
                    &self.mouse_model,
                    &self.dpi_panel,
                    &self.smartshift_panel,
                    &self.lighting_panel,
                    self.active_tab,
                    pal,
                    cx,
                )
                .into_any_element(),
            )
        } else {
            (
                home_header(pal).into_any_element(),
                if has_device {
                    device_gallery(cx).into_any_element()
                } else {
                    device_empty_state(pal, scanning)
                },
            )
        };

        v_flex()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .child(header_el)
            .child(content_el)
            .child(footer(pal, granted))
            .into_any_element()
    }
}

/// Home (gallery) top bar: the "Devices" title, a Settings gear, and the
/// Add-Device button — the entry points the old carousel header used to carry.
fn home_header(pal: Palette) -> impl IntoElement {
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
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr!("Devices")),
        )
        .child(settings_button(pal))
        .child(add_device_button(pal))
}

/// Device-detail top bar: a back affordance returning to the gallery, the
/// active device's name, its connection status, and the Add-Device button.
fn detail_header(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    let record = cx
        .try_global::<AppState>()
        .and_then(AppState::current_record)
        .cloned();
    h_flex()
        .h(px(HEADER_H))
        .w_full()
        .px_5()
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(pal.border)
        .child(back_button(pal, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(
                    record
                        .as_ref()
                        .map_or_else(|| tr!("Device").to_string(), |r| r.display_name.clone()),
                ),
        )
        .when_some(record, |this, r| this.child(status_badge(r.online, pal)))
        .child(add_device_button(pal))
}

/// "← Back" affordance on the detail screen; returns to the gallery without
/// changing the active-device selection.
fn back_button(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    h_flex()
        .id("detail-back")
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .text_color(pal.text_muted)
        .cursor_pointer()
        .hover(|s| s.bg(pal.surface_hover).text_color(pal.text_primary))
        .child(Icon::new(IconName::ChevronLeft).size_4())
        .child(tr!("Back"))
        .on_click(cx.listener(|this, _, _, cx| this.go_home(cx)))
}

/// Square Settings gear in the Home header: opens the Settings window.
fn settings_button(pal: Palette) -> impl IntoElement {
    h_flex()
        .id("home-settings")
        .flex_shrink_0()
        .size(px(36.))
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .text_color(pal.text_muted)
        .cursor_pointer()
        .hover(|s| s.bg(pal.surface_hover).text_color(pal.text_primary))
        .tooltip(|window, cx| Tooltip::new(tr!("Settings")).build(window, cx))
        .child(Icon::new(IconName::Settings).size_4())
        .on_click(|_, _, cx| crate::windows::settings::open(cx))
}

/// Horizontal gap between gallery cards, in pixels.
const GALLERY_GAP: f32 = 24.;

/// The Home device list: an equal-size, horizontally scrollable row of device
/// cards (Logi Options+ style), via [`Carousel`]'s `uniform` mode. Each card
/// floats the device photo on the window background above its name and battery;
/// the row centres while the cards fit the viewport and scrolls once they don't.
/// Clicking a card opens its detail screen and makes it the active device (whose
/// bindings the hook uses); the active card wears a faint accent ring.
fn device_gallery(cx: &mut Context<AppView>) -> impl IntoElement {
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
                let view = view.clone();
                device_card(&record, focused, pal)
                    .id(("device-card", idx))
                    .cursor_pointer()
                    .hover(move |s| s.bg(pal.surface))
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

/// A device card in the Home gallery: the device photo floating on the window
/// background above the name, connectivity dot, kind/slot, and battery. Fixed
/// width so cards stay equal in the scrollable row. The active device wears a
/// faint accent ring; inactive cards reserve the same 1px border in a
/// transparent colour so selection never nudges the layout. Returns a bare
/// [`Div`] so the gallery can wire the click handler.
fn device_card(record: &DeviceRecord, active: bool, pal: Palette) -> Div {
    let ring = if active {
        rgb(theme::ACCENT_BLUE).into()
    } else {
        gpui::transparent_black()
    };
    v_flex()
        .w(px(theme::GALLERY_CARD_W))
        .flex_shrink_0()
        .items_center()
        .gap_3()
        .p_3()
        .rounded_xl()
        .border_1()
        .border_color(ring)
        .child(
            div()
                .w_full()
                .h(px(theme::GALLERY_PHOTO_H))
                .flex()
                .items_center()
                .justify_center()
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
                                .font_weight(FontWeight::SEMIBOLD)
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
                        .when_some(record.battery.as_ref(), |this, b| {
                            this.child(battery_view(b, pal))
                        }),
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

/// Trailing "+" button that opens the pairing window. Present in both screen
/// headers; the empty state carries its own primary "Add Device" CTA, so this
/// never floats alone in an empty header.
fn add_device_button(pal: Palette) -> impl IntoElement {
    h_flex()
        .id("header-add-device")
        .flex_shrink_0()
        .size(px(36.))
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .text_color(pal.text_muted)
        .cursor_pointer()
        .hover(|s| s.bg(pal.surface_hover).text_color(pal.text_primary))
        .tooltip(|window, cx| Tooltip::new(tr!("Add Device")).build(window, cx))
        .child(Icon::new(IconName::Plus).size_4())
        .on_click(|_, _, cx| crate::windows::add_device::open(cx))
}

fn main_window_title(show_device: bool, cx: &Context<AppView>) -> SharedString {
    if !show_device {
        return SharedString::from("OpenLogi");
    }
    cx.try_global::<AppState>()
        .and_then(AppState::current_record)
        .map_or_else(
            || SharedString::from("OpenLogi"),
            |record| SharedString::from(format!("OpenLogi - {}", record.display_name)),
        )
}

/// The device-detail body: a tab bar over the active device's sections (which
/// vary by kind — see [`DetailTab::tabs_for`]), with the active section filling
/// the rest of the screen.
fn detail_content(
    mouse_model: &Entity<MouseModelView>,
    dpi_panel: &Entity<DpiPanel>,
    smartshift_panel: &Entity<SmartShiftPanel>,
    lighting_panel: &Entity<LightingPanel>,
    active: DetailTab,
    pal: Palette,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let tabs = cx
        .try_global::<AppState>()
        .and_then(AppState::current_record)
        .map_or_else(|| vec![DetailTab::Device], DetailTab::tabs_for);
    // The stored tab may not belong to this device — e.g. it lingered across a
    // hot-plug onto a different kind. Fall back to the device's first tab.
    let active = if tabs.contains(&active) {
        active
    } else {
        tabs.first().copied().unwrap_or(DetailTab::Device)
    };
    let content = match active {
        DetailTab::Buttons => buttons_tab(mouse_model).into_any_element(),
        DetailTab::Pointer => pointer_tab(dpi_panel, smartshift_panel, pal).into_any_element(),
        DetailTab::Lighting => lighting_tab(lighting_panel, pal).into_any_element(),
        DetailTab::Device => device_tab(pal, cx).into_any_element(),
    };
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .child(detail_tab_bar(&tabs, active, cx))
        .child(content)
}

/// The detail screen's tab bar, built from the active device's tab set. Clicking
/// a tab swaps the active section.
fn detail_tab_bar(
    tabs: &[DetailTab],
    active: DetailTab,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let active_ix = tabs.iter().position(|t| *t == active).unwrap_or(0);
    // Owned copy so the click handler can map a clicked index back to its tab
    // without borrowing the caller's slice.
    let order = tabs.to_vec();
    div().w_full().px_5().pt_3().child(
        TabBar::new("detail-tabs")
            .underline()
            .w_full()
            .selected_index(active_ix)
            .children(tabs.iter().map(|t| t.label()))
            .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                this.active_tab = order.get(*ix).copied().unwrap_or(DetailTab::Device);
                cx.notify();
            })),
    )
}

/// Buttons tab: the mouse model with clickable hotspots, centred with a max
/// width so it doesn't stretch across a wide window.
fn buttons_tab(mouse_model: &Entity<MouseModelView>) -> impl IntoElement {
    h_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .justify_center()
        .p_6()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .max_w(px(760.))
                .child(mouse_model.clone()),
        )
}

/// Pointer tab: the DPI panel and the SmartShift wheel controls, each in a
/// titled card, stacked.
fn pointer_tab(
    dpi_panel: &Entity<DpiPanel>,
    smartshift_panel: &Entity<SmartShiftPanel>,
    pal: Palette,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p_6()
        .gap_4()
        .child(div().w_full().max_w(px(560.)).child(panel_card(
            tr!("Pointer tuning"),
            IconName::Settings,
            pal,
            dpi_panel.clone().into_any_element(),
        )))
        .child(div().w_full().max_w(px(560.)).child(panel_card(
            tr!("SmartShift"),
            IconName::Settings,
            pal,
            smartshift_panel.clone().into_any_element(),
        )))
}

/// Lighting tab: the RGB controls (swatches, on/off, brightness) in a titled
/// card. Only reached for wired keyboards — see [`supports_lighting`].
fn lighting_tab(lighting_panel: &Entity<LightingPanel>, pal: Palette) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p_6()
        .child(div().w_full().max_w(px(560.)).child(panel_card(
            tr!("Lighting"),
            IconName::Palette,
            pal,
            lighting_panel.clone().into_any_element(),
        )))
}

/// Device tab: device details and configuration cards stacked.
fn device_tab(pal: Palette, cx: &mut Context<AppView>) -> impl IntoElement {
    v_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .overflow_y_scrollbar()
        .p_6()
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
                    .text_sm()
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

    panel_card(tr!("Device details"), IconName::Info, pal, content)
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
                    pal,
                    |_event, _window, cx| crate::windows::settings::open(cx),
                ))
                .child(sidebar_action(
                    "right-panel-config-folder",
                    IconName::Folder,
                    tr!("Config folder"),
                    pal,
                    |_event, _window, cx| {
                        if let Ok(path) = openlogi_core::paths::config_dir() {
                            cx.open_url(&file_url(&path));
                        }
                    },
                )),
        )
        .into_any_element();

    panel_card(tr!("Configuration"), IconName::Folder, pal, content)
}

fn device_summary(name: &str, kind: DeviceKind, online: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .gap_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(name.to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(pal.text_muted)
                        .child(kind_label(kind)),
                ),
        )
        .child(status_badge(online, pal))
}

fn device_description_list(record: crate::state::DeviceRecord) -> impl IntoElement {
    let mut items = vec![
        DescriptionItem::new(tr!("Connection")).value(route_label(record.route.as_ref())),
        DescriptionItem::new(tr!("Slot")).value(record.slot.to_string()),
        DescriptionItem::new(tr!("Device key")).value(record.config_key),
    ];
    if let Some(serial) = record.serial_number {
        items.push(DescriptionItem::new(tr!("Serial")).value(serial));
    }

    DescriptionList::new()
        .columns(1)
        .label_width(px(100.))
        .bordered(false)
        .children(items)
}

fn panel_card(
    title: SharedString,
    icon: IconName,
    pal: Palette,
    content: AnyElement,
) -> impl IntoElement {
    div()
        .w_full()
        .max_w_full()
        .min_w_0()
        .rounded_lg()
        .border_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .p_4()
        .child(
            v_flex()
                .gap_3()
                .when(!title.is_empty(), |this| {
                    this.child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .text_color(pal.text_primary)
                            .child(Icon::new(icon).size_4().text_color(pal.text_muted))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            ),
                    )
                })
                .child(content),
        )
}

fn status_badge(online: bool, pal: Palette) -> impl IntoElement {
    let (label, color) = if online {
        (tr!("Connected"), theme::STATUS_CONNECTED)
    } else {
        (tr!("Offline"), theme::STATUS_OFFLINE)
    };
    h_flex()
        .gap_1()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(pal.border)
        .px_2()
        .py_1()
        .text_xs()
        .text_color(pal.text_muted)
        .child(div().size_1p5().rounded_full().bg(rgb(color)))
        .child(label)
}

fn battery_summary(battery: &BatteryInfo, pal: Palette) -> impl IntoElement {
    let status = match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => tr!("Charging"),
        BatteryStatus::Full => tr!("Full"),
        BatteryStatus::Error => tr!("Battery error"),
        BatteryStatus::Discharging | BatteryStatus::Unknown => tr!("Battery"),
    };
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .text_xs()
                .text_color(pal.text_muted)
                .child(status)
                .child(format!("{}%", battery.percentage)),
        )
        .child(
            div()
                .h(px(6.))
                .w_full()
                .rounded_full()
                .bg(pal.surface_hover)
                .child(
                    div()
                        .h_full()
                        .w(relative_percent(battery.percentage))
                        .rounded_full()
                        .bg(rgb(battery_color(battery.percentage))),
                ),
        )
}

fn sidebar_action(
    id: &'static str,
    icon: IconName,
    label: SharedString,
    pal: Palette,
    handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    h_flex()
        .id(id)
        .flex_1()
        .justify_center()
        .items_center()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .px_2()
        .py_1()
        .text_xs()
        .text_color(pal.text_primary)
        .cursor_pointer()
        .hover(move |s| s.bg(pal.surface_hover))
        .child(Icon::new(icon).size_3())
        .child(label)
        .on_click(handler)
        .into_any_element()
}

fn route_label(route: Option<&DeviceRoute>) -> String {
    match route {
        Some(DeviceRoute::Bolt { .. }) => tr!("Bolt receiver").to_string(),
        Some(DeviceRoute::Direct { .. }) => tr!("Direct connection").to_string(),
        None => tr!("Unavailable").to_string(),
    }
}

fn kind_label(kind: DeviceKind) -> String {
    match kind {
        DeviceKind::Mouse => tr!("Mouse").to_string(),
        DeviceKind::Keyboard => tr!("Keyboard").to_string(),
        DeviceKind::Numpad => tr!("Numpad").to_string(),
        DeviceKind::Presenter => tr!("Presenter").to_string(),
        DeviceKind::Remote => tr!("Remote").to_string(),
        DeviceKind::Trackball => tr!("Trackball").to_string(),
        DeviceKind::Touchpad => tr!("Touchpad").to_string(),
        DeviceKind::Tablet => tr!("Tablet").to_string(),
        DeviceKind::Gamepad => tr!("Gamepad").to_string(),
        DeviceKind::Joystick => tr!("Joystick").to_string(),
        DeviceKind::Headset => tr!("Headset").to_string(),
        DeviceKind::Unknown => tr!("Device").to_string(),
    }
}

fn battery_color(percentage: u8) -> u32 {
    match percentage {
        0..=20 => 0x00ef_4444,
        21..=50 => theme::STATUS_CONNECTING,
        _ => theme::STATUS_CONNECTED,
    }
}

fn relative_percent(value: u8) -> gpui::DefiniteLength {
    relative(f32::from(value.clamp(1, 100)) / 100.)
}

fn file_url(path: &std::path::Path) -> String {
    format!("file://{}", path.to_string_lossy().replace(' ', "%20"))
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
                    tr!("No devices connected")
                }),
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
            div()
                .id("empty-add-device")
                .mt_1()
                .px_4()
                .py_1()
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
        .child(div().mt_1().max_w(px(440.)).text_xs().text_center().text_color(pal.text_muted).child(tr!(
            "Using Logi Options+? Quit it first — both apps compete for HID++ access."
        )))
        .into_any_element()
}

/// Footer status bar: passive state only. Left — the Accessibility-permission
/// indicator; right — the app version. The former actions (Add Device /
/// Settings / About) moved to where they belong: Add Device to the device
/// header's "+", Settings to the right panel's Configuration card and the menu
/// bar (⌘,), About to the menu bar. Keeping operations out of here leaves a
/// genuine status bar — two quiet readouts at the edges, nothing in the middle.
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
        .child(accessibility_status(pal, granted))
        .child(
            div()
                .text_xs()
                .text_color(pal.text_muted)
                .child(concat!("v", env!("CARGO_PKG_VERSION"))),
        )
}

/// Footer Accessibility-permission indicator. Granted → a muted green-dot
/// status; not granted → an amber-dot affordance that requests the grant on
/// click (the native prompt + System Settings, via [`open_accessibility_settings`]).
fn accessibility_status(pal: Palette, granted: bool) -> AnyElement {
    if granted {
        // Reassurance only — kept deliberately quiet: a small dimmed dot and
        // muted text that recede until something is actually wrong.
        h_flex()
            .gap_1p5()
            .items_center()
            .text_xs()
            .text_color(pal.text_muted)
            .child(
                div()
                    .size_1p5()
                    .rounded_full()
                    .bg(rgb(theme::STATUS_CONNECTED)),
            )
            .child(div().child(tr!("Accessibility granted")))
            .into_any_element()
    } else {
        // The state that needs attention — full-strength text, an amber dot,
        // and a click target that requests the grant.
        h_flex()
            .id("footer-accessibility")
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(pal.text_primary)
            .cursor_pointer()
            .child(
                div()
                    .size_2()
                    .rounded_full()
                    .bg(rgb(theme::STATUS_CONNECTING)),
            )
            .child(div().child(tr!("Accessibility not granted · click to grant")))
            .on_click(|_, _, _| open_accessibility_settings())
            .into_any_element()
    }
}
