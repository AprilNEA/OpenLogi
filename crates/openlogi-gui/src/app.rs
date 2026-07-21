use gpui::{
    AnyElement, App, AppContext as _, BorrowAppContext as _, Context, Entity, FocusHandle,
    FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    Icon, IconName, TitleBar,
    button::{Button, ButtonVariants as _},
    v_flex,
};
use openlogi_core::device::{Capabilities, DeviceInventory, DeviceKind};
use tracing::info;

use openlogi_agent_core::ipc::InventoryHealth;

use crate::app_menu::{CloseWindow, Minimize, Zoom};
use crate::asset::AssetResolver;
use crate::components::dpi_panel::DpiPanel;
use crate::components::lighting_panel::LightingPanel;
use crate::components::smartshift_panel::SmartShiftPanel;
use crate::mouse_model::view::MouseModelView;
use crate::state::{AgentLink, AppState, DeviceRecord};
use crate::theme::{self, Palette};

mod detail;
mod home;
mod status;
mod widgets;

// `mouse_model::view` paints the same keyboard-lighting glow as the Home
// gallery card, so it reaches these through the crate-stable `crate::app::…`
// path rather than the internal `app::home` submodule.
pub(crate) use home::{glow_canvas, keyboard_glow};

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
    ///
    /// Each panel is gated on the device's actual [`Capabilities`] — the HID++
    /// features it announced — not on its [`DeviceKind`]. A panel shows iff the
    /// device can do that thing, so a misclassified device can't lose its
    /// panels (issue #127). Devices we never probed (offline at startup) have no
    /// measured capabilities; we presume a set from their kind so a sleeping
    /// mouse still shows its (host-side) button bindings.
    ///
    /// The Buttons panel renders a *mouse-model* silhouette with hotspots. It is
    /// only useful for pointer-type devices (Mouse / Trackball) or when the device
    /// has a resolved asset that provides its own correct layout. A keyboard that
    /// exposes ReprogControls via HID++ but has no asset would get the generic
    /// mouse fallback hotspots — confusing and wrong. Suppress the Buttons tab for
    /// such devices until a proper keyboard-layout UI is available.
    fn tabs_for(record: &DeviceRecord) -> Vec<Self> {
        let caps = record
            .capabilities
            .unwrap_or_else(|| Capabilities::presumed_from_kind(record.kind));
        let can_show_mouse_model = record.asset.is_some()
            || matches!(record.kind, DeviceKind::Mouse | DeviceKind::Trackball);
        let mut tabs = Vec::new();
        if caps.buttons && can_show_mouse_model {
            tabs.push(Self::Buttons);
        }
        if caps.pointer {
            tabs.push(Self::Pointer);
        }
        if caps.lighting {
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

    fn label(self) -> gpui::SharedString {
        match self {
            Self::Buttons => tr!("Buttons"),
            Self::Pointer => tr!("Pointer"),
            Self::Lighting => tr!("Lighting"),
            Self::Device => tr!("Device"),
        }
    }
}

/// Root application view.
pub struct AppView {
    focus_handle: FocusHandle,
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
    pub fn new(
        _inventories: &[DeviceInventory],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let cache = AssetResolver::new();
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        // `AppState` is installed as a global by `main` (with the IPC command
        // sender) before any window opens; downstream reads use `try_global`
        // and tolerate its absence, so there's no fallback construction here.

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
            focus_handle,
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
                    .max_w(px(440.))
                    .text_sm()
                    .text_color(pal.text_muted)
                    .child(tr!(
                        "Enable “OpenLogi Agent” in the Accessibility list — the \
                         background agent owns the mouse hook, not the OpenLogi app. \
                         If it already shows as enabled, remove the stale entry with \
                         the − button and add it back."
                    )),
            )
            .child(
                Button::new("open-accessibility")
                    .primary()
                    .icon(IconName::Settings)
                    .label(tr!("Open System Settings to grant access"))
                    .on_click(|_, _, cx| request_accessibility(cx)),
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

fn request_accessibility(cx: &mut App) {
    use crate::platform::permissions::{self, Permission};
    // Ask the *agent* to fire the prompt (it owns the hook, so the system dialog
    // must name and authorize openlogi-agent — prompting in the GUI would grant
    // the wrong binary), then open the System Settings pane so the user can flip
    // the switch. Shared by the gate button, the footer, and the Settings window.
    if let Some(state) = cx.try_global::<AppState>() {
        state.request_accessibility_prompt();
    }
    permissions::open_pane(Permission::Accessibility);
}

/// Client-side main-window titlebar: window controls (minimize / maximize /
/// close on Linux + Windows), the drag region, and the app name centred.
/// Replaces the native titlebar so Linux — where the compositor declines
/// server-side decorations and gpui falls back to client-side ones it doesn't
/// paint — still gets a titlebar and window controls. On macOS the widget
/// reserves the traffic-light space.
fn app_title_bar(pal: Palette) -> impl IntoElement {
    TitleBar::new().child(
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(pal.text_muted)
            .child("OpenLogi"),
    )
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);

        // Every frame — including the pre-connection and error frames — hangs
        // off this root, so the window actions (⌘W / ⌘M / zoom) work from the
        // first frame on, not only once the full UI is up.
        let root = v_flex()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            // Linux only: a client-side titlebar (window controls + drag region)
            // as the first row of every frame — including the pre-connection and
            // error frames — so the chrome is present from the first frame on.
            // macOS / Windows keep their native titlebar.
            .when(cfg!(target_os = "linux"), |this| {
                this.child(app_title_bar(pal))
            });

        // The agent is the source of truth for both the permission state and
        // the device list; `AgentLink` is everything the GUI knows about it.
        // Until the first snapshot lands, hold a neutral connecting frame:
        // rendering the permission gate (and then the empty state) off
        // assumed-denied defaults flashed both screens at every already-set-up
        // user on launch. A missing global reads the same way — "nothing is
        // known yet".
        let link = cx
            .try_global::<AppState>()
            .map_or(AgentLink::Connecting, |s| s.agent_link().clone());
        let status = match link {
            AgentLink::Connecting => {
                window.set_window_title("OpenLogi");
                return root.child(status::connecting_body(pal)).into_any_element();
            }
            AgentLink::Unreachable => {
                window.set_window_title("OpenLogi");
                return root.child(status::unreachable_body(pal)).into_any_element();
            }
            AgentLink::OutdatedGui => {
                window.set_window_title("OpenLogi");
                return root
                    .child(status::outdated_gui_body(pal))
                    .into_any_element();
            }
            AgentLink::Ready(status) => status,
        };

        let granted = status.accessibility_granted;
        if !granted && !self.accessibility_dismissed {
            window.set_window_title("OpenLogi");
            return root
                .child(Self::accessibility_gate(pal, cx))
                .into_any_element();
        }

        let has_device = cx
            .try_global::<AppState>()
            .is_some_and(|s| !s.device_list.is_empty());

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

        window.set_window_title(&widgets::main_window_title(show_device, cx));

        let (header_el, content_el) = if show_device {
            // Resolve the active section once and share it between the header
            // (which renders the section tabs) and the body, so the two can't
            // disagree about which tab is live. The stored tab may not belong to
            // this device — it can linger across a hot-plug onto a different kind
            // — so fall back to the device's first tab for display, without
            // mutating `active_tab`.
            let record = cx
                .try_global::<AppState>()
                .and_then(AppState::current_record)
                .cloned();
            let tabs = record
                .as_ref()
                .map_or_else(|| vec![DetailTab::Device], DetailTab::tabs_for);
            let active = if tabs.contains(&self.active_tab) {
                self.active_tab
            } else {
                tabs.first().copied().unwrap_or(DetailTab::Device)
            };
            (
                detail::detail_header(record.as_ref(), &tabs, active, pal, cx).into_any_element(),
                detail::detail_content(
                    &self.mouse_model,
                    &self.dpi_panel,
                    &self.smartshift_panel,
                    &self.lighting_panel,
                    active,
                    pal,
                    cx,
                )
                .into_any_element(),
            )
        } else {
            (
                home::home_header(pal).into_any_element(),
                if has_device {
                    home::device_gallery(cx).into_any_element()
                } else {
                    match status.inventory {
                        InventoryHealth::Scanning => home::device_scanning_state(pal),
                        InventoryHealth::Unavailable => home::scanning_unavailable_state(pal),
                        InventoryHealth::Ready => home::device_empty_state(pal),
                    }
                },
            )
        };

        root.child(header_el)
            .child(content_el)
            .child(status::footer(pal, granted))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::home::connection_icon_path;
    use super::{Capabilities, DetailTab, DeviceKind, DeviceRecord};
    use openlogi_core::device::DeviceTransports;
    use openlogi_hid::DeviceRoute;

    #[test]
    fn connection_icon_matches_route() {
        let bolt = DeviceRoute::Bolt {
            receiver_uid: "r".into(),
            slot: 1,
        };
        let uni = DeviceRoute::Unifying {
            receiver_uid: "r".into(),
            slot: 1,
        };
        let direct = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb019,
        };
        // Firmware transport tables (HID++ 0x0003): a wired-only device (G513),
        // a Bluetooth-capable one (MX Master on a cable or BT), and BLE-direct.
        let wired = DeviceTransports {
            usb: true,
            ..DeviceTransports::default()
        };
        let bt = DeviceTransports {
            usb: true,
            bluetooth: true,
            ..DeviceTransports::default()
        };
        let btle = DeviceTransports {
            btle: true,
            ..DeviceTransports::default()
        };
        assert_eq!(
            connection_icon_path(Some(&bolt), None),
            "action-icons/bolt.svg"
        );
        assert_eq!(
            connection_icon_path(Some(&uni), None),
            "action-icons/unifying.svg"
        );
        // Direct + radio-less firmware = the cable is the only possible link.
        assert_eq!(
            connection_icon_path(Some(&direct), Some(&wired)),
            "action-icons/usb.svg"
        );
        // eQuad is receiver-only, so an equad-only table on a *direct* route
        // still means a cable — not Bluetooth.
        let equad_only = DeviceTransports {
            equad: true,
            ..DeviceTransports::default()
        };
        assert_eq!(
            connection_icon_path(Some(&direct), Some(&equad_only)),
            "action-icons/usb.svg"
        );
        // An all-false table is "unknown", not "wired".
        assert_eq!(
            connection_icon_path(Some(&direct), Some(&DeviceTransports::default())),
            "action-icons/bluetooth.svg"
        );
        // Direct + any radio keeps the Bluetooth mark.
        assert_eq!(
            connection_icon_path(Some(&direct), Some(&bt)),
            "action-icons/bluetooth.svg"
        );
        assert_eq!(
            connection_icon_path(Some(&direct), Some(&btle)),
            "action-icons/bluetooth.svg"
        );
        // Unknown transports (no 0x0003 snapshot) keep the old default.
        assert_eq!(
            connection_icon_path(Some(&direct), None),
            "action-icons/bluetooth.svg"
        );
        // No route (e.g. a synthetic/placeholder card) falls back to Bluetooth.
        assert_eq!(
            connection_icon_path(None, None),
            "action-icons/bluetooth.svg"
        );
    }

    fn record(kind: DeviceKind, capabilities: Option<Capabilities>) -> DeviceRecord {
        DeviceRecord {
            config_key: "test".to_string(),
            model_key: "test".to_string(),
            display_name: "Test".to_string(),
            asset: None,
            model_info: None,
            codename: None,
            serial_number: None,
            unit_id: [0; 4],
            route: None,
            kind,
            capabilities,
            slot: 1,
            online: true,
            battery: None,
        }
    }

    /// Tabs follow measured capabilities, not kind — the core of the #127 fix.
    /// A device the Bolt register mislabels as Keyboard but whose 0x0005 probe
    /// returns Mouse ends up with kind=Mouse; measured caps drive the tabs.
    #[test]
    fn tabs_follow_capabilities_not_kind() {
        let caps = Some(Capabilities {
            buttons: true,
            pointer: true,
            lighting: false,
            scroll_inversion: false,
            hires_wheel: false,
        });
        // After 0x0005 kind-correction the record has kind=Mouse, not Keyboard.
        let tabs = DetailTab::tabs_for(&record(DeviceKind::Mouse, caps));
        assert!(tabs.contains(&DetailTab::Buttons));
        assert!(tabs.contains(&DetailTab::Pointer));
        assert!(!tabs.contains(&DetailTab::Lighting));
    }

    /// A keyboard that exposes ReprogControls (buttons=true) but has no resolved
    /// asset should not get the mouse-model Buttons panel — the generic mouse
    /// hotspot layout (Middle Click, DPI Toggle, …) is wrong for a keyboard.
    #[test]
    fn keyboard_without_asset_hides_buttons_tab() {
        let caps = Some(Capabilities {
            buttons: true,
            pointer: false,
            lighting: true,
            scroll_inversion: false,
            hires_wheel: false,
        });
        let tabs = DetailTab::tabs_for(&record(DeviceKind::Keyboard, caps));
        assert!(
            !tabs.contains(&DetailTab::Buttons),
            "mouse model shown for keyboard"
        );
        assert!(tabs.contains(&DetailTab::Lighting));
    }

    /// Each panel is independent: a lighting-only device (e.g. a keyboard with
    /// RGB but no remappable keys yet) shows only Lighting + Device.
    #[test]
    fn lighting_only_device_shows_only_lighting() {
        let caps = Some(Capabilities {
            lighting: true,
            ..Capabilities::default()
        });
        let tabs = DetailTab::tabs_for(&record(DeviceKind::Keyboard, caps));
        assert_eq!(tabs, vec![DetailTab::Lighting, DetailTab::Device]);
    }

    /// An unprobed (offline) device has no measured capabilities and falls back
    /// to a kind presumption, so a sleeping mouse keeps its button/pointer tabs.
    #[test]
    fn unprobed_mouse_falls_back_to_presumed_capabilities() {
        let tabs = DetailTab::tabs_for(&record(DeviceKind::Mouse, None));
        assert!(tabs.contains(&DetailTab::Buttons));
        assert!(tabs.contains(&DetailTab::Pointer));
        assert!(!tabs.contains(&DetailTab::Lighting));
    }

    /// An unprobed, unidentified device presumes nothing — only the info tab,
    /// rather than guessing wrong panels (the old Unknown+Direct→lighting bug).
    #[test]
    fn unprobed_unknown_device_shows_only_device_tab() {
        let tabs = DetailTab::tabs_for(&record(DeviceKind::Unknown, None));
        assert_eq!(tabs, vec![DetailTab::Device]);
    }
}
