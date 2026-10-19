//! Opt-in native Metal screenshots using normal configuration and request/reply state.
#[cfg(feature = "native-ui-qa")]
use std::{path::PathBuf, sync::Arc};

#[cfg(feature = "native-ui-qa")]
use anyhow::Context as _;
use gpui::AppContext as _;
#[cfg(feature = "native-ui-qa")]
use gpui::{AnyWindowHandle, Entity, HeadlessAppContext, Render, Window, px, size};
#[cfg(feature = "native-ui-qa")]
use gpui_component::{Root, Theme, ThemeMode};
use openlogi_core::{
    binding::{Action, ButtonId, GestureDirection},
    config::Config,
    device::{
        Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    },
};
use tokio::sync::mpsc;

use crate::{
    services::{assets::AssetResolver, ipc::Command},
    state::{AppState, ConfigPersistence, Sources},
};

pub(crate) const KEY: &str = "serial:qa-mx4";
pub(crate) const APP: &str = "com.mitchellh.ghostty";

#[cfg(feature = "native-ui-qa")]
#[derive(Default)]
struct Measurements(std::collections::BTreeMap<String, gpui::Bounds<gpui::Pixels>>);
#[cfg(feature = "native-ui-qa")]
impl gpui::Global for Measurements {}

#[cfg(feature = "native-ui-qa")]
pub(crate) fn observe(id: String) -> gpui::AnyElement {
    use gpui::{IntoElement as _, Styled as _};
    gpui::canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| {
            let visible = bounds.intersect(&window.content_mask().bounds);
            cx.global_mut::<Measurements>().0.insert(id, visible);
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

#[cfg(feature = "native-ui-qa")]
pub(crate) fn measured(cx: &gpui::App, id: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    cx.global::<Measurements>().0.get(id).copied()
}

#[cfg(feature = "native-ui-qa")]
pub(crate) fn output() -> Option<PathBuf> {
    std::env::var_os("OPENLOGI_QA_OUTPUT").map(PathBuf::from)
}

#[cfg(feature = "native-ui-qa")]
pub(crate) fn context(mode: ThemeMode) -> HeadlessAppContext {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(crate::app_assets::AppAssets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(gpui_component::init);
    cx.update(|cx| cx.set_global(Measurements::default()));
    cx.update(|cx| Theme::change(mode, None, cx));
    cx
}

#[cfg(feature = "native-ui-qa")]
pub(crate) fn install_fixture(
    cx: &mut HeadlessAppContext,
) -> anyhow::Result<mpsc::UnboundedReceiver<Command>> {
    cx.update(install_on_app)
}

pub(crate) fn install_on_app(
    cx: &mut gpui::App,
) -> anyhow::Result<mpsc::UnboundedReceiver<Command>> {
    let (commands, receiver) = mpsc::unbounded_channel();
    let mut config = Config::ephemeral();
    config.set_binding(
        KEY,
        ButtonId::HapticPanel,
        openlogi_core::binding::Binding::Single(Action::ShowActionsRing),
    );
    config.set_per_app_gesture_mode(KEY, APP, ButtonId::HapticPanel, true);
    for (direction, shortcut) in [
        (GestureDirection::Click, "Enter"),
        (GestureDirection::Up, "2"),
        (GestureDirection::Down, "3"),
        (GestureDirection::Left, "Cmd+Left"),
        (GestureDirection::Right, "Cmd+Right"),
    ] {
        config.set_per_app_gesture_direction(
            KEY,
            APP,
            ButtonId::HapticPanel,
            direction,
            Action::CustomShortcut(shortcut.parse()?),
        );
    }
    let mut capabilities = Capabilities::presumed_from_kind(DeviceKind::Mouse);
    capabilities.haptic_feedback = true;
    capabilities.haptic_panel = true;
    capabilities.dpi_gestures = true;
    let inventory = DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Master 4".into(),
            vendor_id: 0x046d,
            product_id: 0xb042,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
            codename: Some("MX Master 4".into()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            settings_revision: 0,
            capabilities: Some(capabilities),
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: Some("QA-MX4".into()),
                unit_id: [1, 2, 3, 4],
                transports: DeviceTransports::default(),
                model_ids: [0xb042, 0, 0],
                extended_model_id: 2,
            }),
        }],
    };
    {
        let resolver = AssetResolver::fixture();
        let state = cx.new(|_| {
            AppState::new(Sources {
                inventories: &[inventory],
                config,
                resolver: &resolver,
                ipc_commands: commands,
                standalone: &[],
                cameras: &[],
                persistence: ConfigPersistence::MemoryOnly,
            })
        });
        AppState::set_global(state, cx);
    }
    Ok(receiver)
}

#[cfg(feature = "native-ui-qa")]
struct ViewportFrame<V: Render + 'static> {
    view: Entity<V>,
}

#[cfg(feature = "native-ui-qa")]
impl<V: Render + 'static> Render for ViewportFrame<V> {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _};
        gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .items_stretch()
            .child(self.view.clone())
    }
}

#[cfg(feature = "native-ui-qa")]
pub(crate) fn open<V: Render + 'static>(
    cx: &mut HeadlessAppContext,
    width: f32,
    height: f32,
    build: impl FnOnce(&mut Window, &mut gpui::Context<V>) -> V,
) -> anyhow::Result<(AnyWindowHandle, Entity<V>)> {
    let mut entity = None;
    let window = cx
        .open_window(size(px(width), px(height)), |window, cx| {
            let view = cx.new(|cx| build(window, cx));
            entity = Some(view.clone());
            let frame = cx.new(|_| ViewportFrame { view });
            cx.new(|cx| Root::new(frame, window, cx))
        })
        .context("Open offscreen QA window")?;
    Ok((
        window.into(),
        entity.context("QA view constructor did not initialize")?,
    ))
}

#[cfg(feature = "native-ui-qa")]
pub(crate) fn render(cx: &mut HeadlessAppContext, window: AnyWindowHandle) -> anyhow::Result<()> {
    cx.run_until_parked();
    cx.update(|cx| cx.set_global(Measurements::default()));
    cx.update_window(window, |_, window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    })?;
    Ok(())
}

#[cfg(feature = "native-ui-qa")]
pub(crate) fn capture(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    name: &str,
) -> anyhow::Result<()> {
    render(cx, window)?;
    let image = cx
        .capture_screenshot(window)
        .context("Native Metal renderer required for QA")?;
    let directory = output().context("Set OPENLOGI_QA_OUTPUT")?;
    std::fs::create_dir_all(&directory)?;
    image.save(directory.join(name))?;
    Ok(())
}

#[cfg(feature = "native-ui-qa")]
pub(crate) fn run() -> anyhow::Result<()> {
    rust_i18n::set_locale("en");
    let directory = output().context("Set OPENLOGI_QA_OUTPUT for native UI QA")?;
    std::fs::create_dir_all(directory)?;
    crate::features::mouse::view::render_native_profile_qa()?;
    Ok(())
}
