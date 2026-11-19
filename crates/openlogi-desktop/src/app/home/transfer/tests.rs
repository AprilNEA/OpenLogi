//! The transfer dialogs at the main window's minimum size and the largest
//! interface scale: with more sources and changes than fit on screen, and
//! with a copy that changes while its preview is open.

use gpui::{
    AppContext as _, Bounds, Modifiers, Pixels, Render, ScrollDelta, ScrollEvent, TestAppContext,
    VisualTestContext, point,
};
use openlogi_core::binding::{Action, Binding, ButtonId};
use openlogi_core::config::{Config, DeviceConfig, UiScale};
use openlogi_core::device::DeviceInventory;
use openlogi_core::hid::Dpi;

use super::*;
use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
use crate::state::Sources;
use crate::state::tests::{KNOWN_MOUSE_KEY, direct_inventory};
use crate::windows::main_window::MIN_SIZE;

const SOURCES: usize = 20;
const PROFILES: usize = 40;

/// A file with [`SOURCES`] mice, each overriding Back in [`PROFILES`] apps.
fn crowded_file() -> Config {
    let mut file = Config::ephemeral();
    for mouse in 0..SOURCES {
        let mut device = DeviceConfig::default();
        device.custom_name = Some(format!("Mouse {mouse}"));
        for app in 0..PROFILES {
            device
                .per_app_bindings
                .entry(format!("com.example.app{app}"))
                .or_default()
                .insert(ButtonId::Back, Action::Copy);
        }
        file.devices.insert(format!("serial:mouse-{mouse}"), device);
    }
    file
}

/// The main window's root at the largest interface scale.
struct LargestScale;

impl Render for LargestScale {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        theme::apply_scale(window, UiScale::ExtraLarge);
        div().size_full()
    }
}

fn window_at_minimum_size(
    cx: &mut TestAppContext,
    inventory: DeviceInventory,
) -> &mut VisualTestContext {
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::register_builtin_themes(cx);
        cx.set_reduce_motion(true);
        let mut config = Config::ephemeral();
        config.app_settings.ui_scale = UiScale::ExtraLarge;
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let resolver = AssetResolver::new();
        let state = cx.new(|_| {
            AppState::new(Sources {
                inventories: &[inventory],
                ..Sources::in_memory(config, &resolver, commands)
            })
        });
        AppState::set_global(state, cx);
    });
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| LargestScale);
        gpui_kit::component::Root::new(view, window, cx)
    });
    cx.simulate_resize(MIN_SIZE);
    redraw(cx);
    cx
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn viewport() -> Bounds<Pixels> {
    Bounds::new(point(px(0.), px(0.)), MIN_SIZE)
}

/// `selector`'s bounds, asserting they lie in the window.
fn on_screen(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} renders"));
    assert!(
        viewport().contains(&bounds.origin) && viewport().contains(&bounds.bottom_right()),
        "{selector} at {bounds:?} must fit in {:?}",
        viewport()
    );
    bounds
}

/// Scroll the dialog body under `anchor` to its end.
fn scroll_to_end(cx: &mut VisualTestContext, anchor: Bounds<Pixels>) {
    cx.simulate_event(ScrollEvent {
        position: anchor.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
        ..Default::default()
    });
    redraw(cx);
}

#[gpui::test]
fn the_last_source_and_the_copy_controls_are_reachable_at_minimum_size(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let cx = window_at_minimum_size(cx, direct_inventory([0xa3, 0x93, 0xca, 0xe0]));
    let sources = cx.update(|_, cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .file_transfer_sources(KNOWN_MOUSE_KEY, &crowded_file())
    });
    assert_eq!(sources.len(), SOURCES);

    cx.update(|window, cx| {
        open_source_choice(window, cx, KNOWN_MOUSE_KEY.to_owned(), sources);
    });
    redraw(cx);
    // Whether the list overflows depends on the platform's text metrics, so
    // the last source counts as reachable on screen or after scrolling.
    let first = on_screen(cx, "import-settings-source-0");
    scroll_to_end(cx, first);
    let last = on_screen(cx, "import-settings-source-19");
    cx.simulate_click(last.center(), Modifiers::none());
    redraw(cx);

    let cancel = on_screen(cx, "dialog-cancel");
    let confirm = on_screen(cx, "copy-settings-confirm");
    let changes = cx
        .debug_bounds("copy-settings-preview")
        .expect("the preview lists the changes");
    scroll_to_end(
        cx,
        Bounds::new(changes.origin + point(px(20.), px(20.)), confirm.size),
    );
    let scrolled = cx
        .debug_bounds("copy-settings-preview")
        .expect("the preview lists the changes");
    assert!(
        scrolled.origin.y <= changes.origin.y && scrolled.bottom() <= cancel.top(),
        "the last change is reachable above the buttons: {changes:?} -> {scrolled:?}"
    );
    assert_eq!(
        cx.debug_bounds("copy-settings-confirm"),
        Some(confirm),
        "the buttons stay put while the changes scroll"
    );
    assert_eq!(cx.debug_bounds("dialog-cancel"), Some(cancel));
    cx.simulate_click(confirm.center(), Modifiers::none());
    redraw(cx);

    let copied = cx.update(|_, cx| {
        AppState::try_read(cx)
            .expect("state installed")
            .device_export(KNOWN_MOUSE_KEY)
            .and_then(|export| export.devices.into_values().next())
            .map_or(0, |device| device.per_app_bindings.len())
    });
    assert_eq!(copied, PROFILES, "the last source's settings were copied");
    assert!(
        cx.debug_bounds("copy-settings-confirm").is_none(),
        "copying closes the preview"
    );
}

/// A preview opened before the mouse was read leaves its DPI out. Confirming
/// it after the mouse is read copies nothing and shows the preview again,
/// with the DPI in, which copies once confirmed.
#[gpui::test]
fn a_copy_that_changed_while_its_preview_was_open_is_shown_again(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let read = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    let mut unread = read.clone();
    unread.paired[0].capabilities = None;
    let cx = window_at_minimum_size(cx, unread);
    let mut file = Config::ephemeral();
    let mut mouse = DeviceConfig::default();
    mouse.custom_name = Some("Old mouse".to_owned());
    mouse.dpi = Some(Dpi::new(2400));
    mouse
        .bindings
        .insert(ButtonId::Back, Binding::Single(Action::Copy));
    file.devices.insert("serial:old-mouse".to_owned(), mouse);
    let back = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            AppState::try_read(cx)
                .expect("state installed")
                .device_export(KNOWN_MOUSE_KEY)
                .and_then(|export| export.devices.into_values().next())
                .and_then(|device| device.bindings.get(&ButtonId::Back).cloned())
        })
    };
    cx.update(|window, cx| {
        let sources = AppState::try_read(cx)
            .expect("state installed")
            .file_transfer_sources(KNOWN_MOUSE_KEY, &file);
        open_transfer_preview(window, cx, KNOWN_MOUSE_KEY, &sources[0]);
    });
    redraw(cx);
    assert!(cx.debug_bounds("copy-settings-changed").is_none());

    cx.update(|_, cx| {
        AppState::update(cx, |state, _| {
            let _ = state.refresh_inventories(&[read], &[], &AssetResolver::new(), &[]);
        });
    });
    let confirm = on_screen(cx, "copy-settings-confirm");
    cx.simulate_click(confirm.center(), Modifiers::none());
    redraw(cx);

    assert_eq!(back(cx), None, "nothing is copied");
    on_screen(cx, "copy-settings-changed");
    let confirm = on_screen(cx, "copy-settings-confirm");
    cx.simulate_click(confirm.center(), Modifiers::none());
    redraw(cx);

    assert_eq!(back(cx), Some(Binding::Single(Action::Copy)));
    assert!(
        cx.debug_bounds("copy-settings-confirm").is_none(),
        "copying closes the preview"
    );
}
