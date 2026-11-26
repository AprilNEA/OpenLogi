use gpui::{Hsla, rgb};
use openlogi_core::color::Rgb;
use openlogi_core::config::Lighting;

use super::{PALETTE, custom_color, rgb_of};

fn lighting(enabled: bool, color: Rgb) -> Lighting {
    Lighting {
        enabled,
        color,
        ..Lighting::default()
    }
}

#[test]
fn a_preset_color_is_not_a_custom_color() {
    assert_eq!(custom_color(&lighting(true, PALETTE[0])), None);
}

#[test]
fn an_off_palette_color_is_the_custom_color() {
    let color = Rgb::new(0x00, 0xff, 0xaa);
    assert_eq!(custom_color(&lighting(true, color)), Some(color));
}

#[test]
fn lighting_off_selects_no_custom_color() {
    assert_eq!(
        custom_color(&lighting(false, Rgb::new(0x00, 0xff, 0xaa))),
        None
    );
}

#[test]
fn picker_colors_convert_without_alpha() {
    let translucent = Hsla {
        a: 0.3,
        ..Hsla::from(rgb(0x0000_ffaa))
    };
    assert_eq!(rgb_of(translucent), Rgb::new(0x00, 0xff, 0xaa));
    for &color in PALETTE {
        let hsla = Hsla::from(rgb(color.packed()));
        assert_eq!(rgb_of(hsla), color, "#{:06x}", color.packed());
    }
}

mod picker {
    use std::sync::Arc;

    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, rgb};
    use gpui_kit::component::color_picker::ColorPickerState;
    use openlogi_core::color::Rgb;
    use openlogi_core::config::Config;
    use openlogi_core::device::{Capabilities, DeviceKind};
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::super::LightingPanel;
    use crate::services::assets::AssetResolver;
    use crate::services::ipc::Command;
    use crate::state::{AppState, ConfigPersistence, Sources};

    const CUSTOM: Rgb = Rgb::new(0x00, 0xff, 0xaa);

    /// A lighting keyboard as the selected device, and its panel.
    fn panel(
        cx: &mut TestAppContext,
    ) -> (
        Entity<LightingPanel>,
        UnboundedReceiver<Command>,
        &mut VisualTestContext,
    ) {
        cx.update(gpui_kit::init);
        let receiver = cx.update(|cx| {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut inventory = crate::state::tests::direct_inventory([1, 2, 3, 4]);
            inventory.paired[0].kind = DeviceKind::Keyboard;
            inventory.paired[0].capabilities = Some(Capabilities {
                lighting: true,
                ..Capabilities::default()
            });
            let runtime: Arc<dyn swr_core::Runtime> = Arc::new(swr_gpui::GpuiRuntime::new(cx));
            let client = swr_core::SwrClient::builder().build(runtime.clone());
            let state = cx.new(|_| {
                let mut state = AppState::new(Sources {
                    config: Config::ephemeral(),
                    inventories: &[inventory],
                    standalone: &[],
                    resolver: &AssetResolver::new(),
                    cameras: &[],
                    persistence: ConfigPersistence::MemoryOnly,
                    ipc_commands: sender,
                });
                state.connect_device_reads(client, runtime);
                state
            });
            AppState::set_global(state, cx);
            receiver
        });
        let (panel, cx) = cx.add_window_view(LightingPanel::new);
        cx.run_until_parked();
        (panel, receiver, cx)
    }

    fn picker_of(
        panel: &Entity<LightingPanel>,
        cx: &mut VisualTestContext,
    ) -> Entity<ColorPickerState> {
        cx.read(|cx| panel.read(cx).picker.clone())
    }

    fn current(cx: &mut VisualTestContext) -> (bool, Rgb) {
        cx.read(|cx| {
            let lighting = AppState::try_read(cx).unwrap().lighting();
            (lighting.enabled, lighting.color)
        })
    }

    fn lighting_writes(receiver: &mut UnboundedReceiver<Command>) -> usize {
        std::iter::from_fn(|| receiver.try_recv().ok())
            .filter(|command| matches!(command, Command::SetLighting(_)))
            .count()
    }

    #[gpui::test]
    fn a_closed_picker_commits_its_color_at_once(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let picker = picker_of(&panel, cx);
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.select_color(rgb(CUSTOM.packed()).into(), window, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(current(cx), (true, CUSTOM));
        assert_eq!(lighting_writes(&mut receiver), 1);
    }

    #[gpui::test]
    fn an_open_picker_commits_only_when_it_closes(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let before = current(cx);
        let picker = picker_of(&panel, cx);
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.set_open(true, cx);
                picker.update_color(rgb(0x11_2233).into(), window, cx);
                picker.update_color(rgb(CUSTOM.packed()).into(), window, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(current(cx), before, "a drag writes nothing");
        assert_eq!(lighting_writes(&mut receiver), 0);
        cx.update(|_, cx| picker.update(cx, |picker, cx| picker.set_open(false, cx)));
        cx.run_until_parked();
        assert_eq!(current(cx), (true, CUSTOM));
        assert_eq!(
            lighting_writes(&mut receiver),
            1,
            "closing writes the last color once"
        );
    }

    #[gpui::test]
    fn closing_an_unchanged_picker_writes_nothing(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let before = current(cx);
        let picker = picker_of(&panel, cx);
        cx.update(|_, cx| {
            picker.update(cx, |picker, cx| {
                picker.set_open(true, cx);
                picker.set_open(false, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(current(cx), before);
        assert_eq!(lighting_writes(&mut receiver), 0);
    }
}
