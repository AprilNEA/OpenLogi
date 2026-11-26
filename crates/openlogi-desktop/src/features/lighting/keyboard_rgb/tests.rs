use gpui::{Hsla, rgb};
use openlogi_core::color::Rgb;
use openlogi_core::config::Lighting;

use super::{PALETTE, PickerEdit, custom_color, rgb_of};

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

const DRAGGED: Rgb = Rgb::new(0x11, 0x22, 0x33);

#[test]
fn a_color_reported_by_a_closed_picker_is_written_at_once() {
    // A palette pick or a hex commit closes the picker, then reports.
    for from in [
        PickerEdit::Closed,
        PickerEdit::Open,
        PickerEdit::Dragged(DRAGGED),
    ] {
        assert_eq!(
            from.next(false, Some(Rgb::WHITE)),
            (PickerEdit::Closed, Some(Rgb::WHITE)),
            "{from:?}"
        );
    }
}

#[test]
fn a_drag_is_held_until_the_picker_closes() {
    let (edit, write) = PickerEdit::Closed.next(true, None);
    assert_eq!((edit, write), (PickerEdit::Open, None));
    let (edit, write) = edit.next(true, Some(Rgb::WHITE));
    assert_eq!((edit, write), (PickerEdit::Dragged(Rgb::WHITE), None));
    let (edit, write) = edit.next(true, Some(DRAGGED));
    assert_eq!((edit, write), (PickerEdit::Dragged(DRAGGED), None));
    assert_eq!(edit.next(false, None), (PickerEdit::Closed, Some(DRAGGED)));
}

#[test]
fn closing_without_a_drag_writes_nothing() {
    assert_eq!(
        PickerEdit::Open.next(false, None),
        (PickerEdit::Closed, None)
    );
    assert_eq!(
        PickerEdit::Closed.next(false, None),
        (PickerEdit::Closed, None)
    );
}

#[test]
fn a_finished_drag_is_not_written_twice() {
    let (edit, write) = PickerEdit::Dragged(DRAGGED).next(false, None);
    assert_eq!(write, Some(DRAGGED));
    assert_eq!(edit.next(false, None), (PickerEdit::Closed, None));
}

mod picker {
    use std::sync::Arc;

    use gpui::{
        AppContext as _, Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext,
        point, px, rgb, size,
    };
    use gpui_kit::component::color_picker::ColorPickerState;
    use openlogi_core::color::Rgb;
    use openlogi_core::config::{Config, UiScale};
    use openlogi_core::device::{Capabilities, DeviceKind};
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::super::LightingPanel;
    use crate::services::assets::AssetResolver;
    use crate::services::ipc::Command;
    use crate::state::{AppState, ConfigPersistence, Sources};
    use crate::ui::theme;

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
        cx.simulate_resize(size(px(800.), px(600.)));
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

    /// Click the custom-color control where a user would.
    fn click_custom_control(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let control = cx
            .debug_bounds("lighting-custom-color")
            .expect("the custom-color control renders");
        cx.simulate_click(control.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn is_open(picker: &Entity<ColorPickerState>, cx: &mut VisualTestContext) -> bool {
        cx.read(|cx| picker.read(cx).is_open())
    }

    #[gpui::test]
    fn clicking_the_custom_control_opens_the_picker_without_a_write(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let picker = picker_of(&panel, cx);
        click_custom_control(cx);
        assert!(is_open(&picker, cx));
        assert_eq!(lighting_writes(&mut receiver), 0);
    }

    #[gpui::test]
    fn a_typed_hex_color_is_written_once_and_closes_the_picker(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let picker = picker_of(&panel, cx);
        click_custom_control(cx);
        let hex = cx.read(|cx| picker.read(cx).hex_input().clone());
        cx.update(|window, cx| hex.update(cx, |input, cx| input.focus(window, cx)));
        // The field holds the current color as `#RRGGBB`; keep the `#`.
        cx.simulate_keystrokes("end backspace backspace backspace backspace backspace backspace");
        cx.simulate_input("00ffaa");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!is_open(&picker, cx));
        assert_eq!(current(cx), (true, CUSTOM));
        assert_eq!(lighting_writes(&mut receiver), 1);
    }

    #[gpui::test]
    fn a_click_outside_closes_the_picker_without_a_write(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let before = current(cx);
        let picker = picker_of(&panel, cx);
        click_custom_control(cx);
        assert!(is_open(&picker, cx));
        cx.simulate_click(point(px(790.), px(590.)), Modifiers::none());
        cx.run_until_parked();
        assert!(!is_open(&picker, cx));
        assert_eq!(current(cx), before);
        assert_eq!(lighting_writes(&mut receiver), 0);
    }

    #[gpui::test]
    fn the_keyboard_reaches_opens_and_closes_the_picker(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let picker = picker_of(&panel, cx);
        let focused = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| picker.read(cx).focus_handle(cx).is_focused(window))
        };
        // Move focus the way Tab does (the test window has no gpui-kit Root
        // to bind the key), past the toggle and the nine swatches.
        for _ in 0..16 {
            if focused(cx) {
                break;
            }
            cx.update(gpui::Window::focus_next);
        }
        assert!(focused(cx), "Tab order reaches the custom-color control");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(is_open(&picker, cx));
        assert!(
            cx.update(|window, _| window.last_input_was_keyboard()),
            "a keyboard-focused control draws the shared focus ring"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!is_open(&picker, cx));
        assert_eq!(lighting_writes(&mut receiver), 0);
    }

    #[gpui::test]
    fn the_custom_control_stays_in_a_narrow_card_at_125_percent(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (_panel, _receiver, cx) = panel(cx);
        // A column narrower than the Lighting card at the main window's
        // minimum width, at the largest interface scale.
        let width = px(360.);
        cx.update(|window, _| theme::apply_scale(window, UiScale::ExtraLarge));
        cx.simulate_resize(size(width, px(600.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let control = cx
            .debug_bounds("lighting-custom-color")
            .expect("the custom-color control renders");
        assert!(
            control.right() <= width,
            "{control:?} must wrap inside the {width:?} column"
        );
    }

    /// The popover's sliders carry no debug selector in gpui-kit, so a test
    /// cannot drag them; this drives the slider's reports through the picker
    /// state the sliders update, then closes the picker by clicking outside.
    #[gpui::test]
    fn a_slider_drag_is_written_once_when_the_picker_closes(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let before = current(cx);
        let picker = picker_of(&panel, cx);
        click_custom_control(cx);
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.update_color(rgb(0x0011_2233).into(), window, cx);
                picker.update_color(rgb(CUSTOM.packed()).into(), window, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(current(cx), before, "a drag writes nothing");
        assert_eq!(lighting_writes(&mut receiver), 0);
        cx.simulate_click(point(px(790.), px(590.)), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(current(cx), (true, CUSTOM));
        assert_eq!(
            lighting_writes(&mut receiver),
            1,
            "closing writes the last color once"
        );
    }
}
