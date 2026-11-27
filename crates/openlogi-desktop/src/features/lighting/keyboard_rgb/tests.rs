use gpui::{Hsla, rgb};
use openlogi_core::color::Rgb;
use openlogi_core::config::Lighting;

use super::{PALETTE, PickerEdit, PickerReport, custom_color, rgb_of};

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
const TYPED: Rgb = Rgb::new(0x00, 0xff, 0xaa);

fn notified(shown: Rgb) -> PickerReport {
    PickerReport::Notified { shown: Some(shown) }
}

#[test]
fn a_color_reported_by_a_closed_picker_is_written_at_once() {
    // A palette pick or a hex commit closes the picker, then reports.
    for from in [PickerEdit::Closed, PickerEdit::Open, PickerEdit::Typed] {
        assert_eq!(
            from.next(false, PickerReport::Changed(Rgb::WHITE)),
            (PickerEdit::Closed, Some(Rgb::WHITE)),
            "{from:?}"
        );
    }
}

#[test]
fn a_drag_is_written_on_release_and_the_picker_stays_open() {
    let (edit, write) = PickerEdit::Closed.next(true, notified(Rgb::WHITE));
    assert_eq!((edit, write), (PickerEdit::Open, None));
    let (edit, write) = edit.next(true, PickerReport::Changed(Rgb::WHITE));
    assert_eq!((edit, write), (PickerEdit::Open, None));
    let (edit, write) = edit.next(true, PickerReport::Changed(DRAGGED));
    assert_eq!((edit, write), (PickerEdit::Open, None));
    let (edit, write) = edit.next(true, PickerReport::Released(DRAGGED));
    assert_eq!((edit, write), (PickerEdit::Open, Some(DRAGGED)));
    assert_eq!(
        edit.next(false, notified(DRAGGED)),
        (PickerEdit::Closed, None),
        "closing does not write the released drag again"
    );
}

#[test]
fn a_typed_color_is_written_once_when_the_picker_is_dismissed() {
    let (edit, write) = PickerEdit::Open.next(true, PickerReport::Typed);
    assert_eq!((edit, write), (PickerEdit::Typed, None));
    let (edit, write) = edit.next(true, notified(TYPED));
    assert_eq!(
        (edit, write),
        (PickerEdit::Typed, None),
        "a redraw keeps it"
    );
    let (edit, write) = edit.next(false, notified(TYPED));
    assert_eq!((edit, write), (PickerEdit::Closed, Some(TYPED)));
    assert_eq!(
        edit.next(false, notified(TYPED)),
        (PickerEdit::Closed, None)
    );
}

#[test]
fn a_typed_color_is_written_once_when_its_field_is_left() {
    let left = PickerReport::FieldLeft { shown: Some(TYPED) };
    assert_eq!(
        PickerEdit::Typed.next(true, left),
        (PickerEdit::Open, Some(TYPED)),
        "the picker stays open, with nothing left to write"
    );
    assert_eq!(
        PickerEdit::Open.next(false, notified(TYPED)),
        (PickerEdit::Closed, None)
    );
    for from in [PickerEdit::Open, PickerEdit::Closed] {
        assert_eq!(
            from.next(false, left),
            (PickerEdit::Closed, None),
            "{from:?}"
        );
    }
}

#[test]
fn a_drag_after_typing_replaces_the_typed_color() {
    let (edit, _) = PickerEdit::Typed.next(true, PickerReport::Changed(DRAGGED));
    assert_eq!(edit, PickerEdit::Open);
    assert_eq!(
        edit.next(false, notified(DRAGGED)),
        (PickerEdit::Closed, None)
    );
}

#[test]
fn closing_without_an_edit_writes_nothing() {
    for from in [PickerEdit::Open, PickerEdit::Closed] {
        assert_eq!(
            from.next(false, notified(Rgb::WHITE)),
            (PickerEdit::Closed, None),
            "{from:?}"
        );
    }
}

mod picker {
    use std::sync::Arc;

    use gpui::{
        AppContext as _, Context, Entity, Focusable as _, IntoElement, Modifiers,
        ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, Window, div,
        point, prelude::FluentBuilder as _, px, size,
    };
    use gpui_kit::component::color_picker::ColorPickerState;
    use gpui_kit::component::slider::{SliderEvent, SliderState};
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
        let receiver = install_keyboard(cx);
        let (panel, cx) = cx.add_window_view(LightingPanel::new);
        cx.simulate_resize(size(px(800.), px(600.)));
        // Focus events, and so `on_blur`, reach only the active window.
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (panel, receiver, cx)
    }

    /// A page that shows the panel until it is navigated away from.
    struct Page {
        panel: Entity<LightingPanel>,
        shown: bool,
    }

    impl Render for Page {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .when(self.shown, |page| page.child(self.panel.clone()))
        }
    }

    /// A lighting keyboard as the selected device, without a window yet.
    fn install_keyboard(cx: &mut TestAppContext) -> UnboundedReceiver<Command> {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
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
        })
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
        type_hex(&picker, "00ffaa", cx);
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

    /// Type `hex` (without its `#`) into the open picker's hex field.
    fn type_hex(picker: &Entity<ColorPickerState>, hex: &str, cx: &mut VisualTestContext) {
        let field = cx.read(|cx| picker.read(cx).hex_input().clone());
        cx.update(|window, cx| field.update(cx, |input, cx| input.focus(window, cx)));
        // The field holds the current color as `#RRGGBB`; keep the `#`.
        cx.simulate_keystrokes("end backspace backspace backspace backspace backspace backspace");
        cx.simulate_input(hex);
        cx.run_until_parked();
    }

    /// Drag the open picker's lightness slider through `steps`, as its
    /// thumb reports them, without letting go.
    ///
    /// The popover's sliders carry no debug selector in gpui-kit, so this
    /// sends the reports a drag makes from the slider itself, the way the
    /// force-sensing panel's tests do.
    fn drag_lightness(
        picker: &Entity<ColorPickerState>,
        steps: &[f32],
        cx: &mut VisualTestContext,
    ) -> Entity<SliderState> {
        let slider = cx.read(|cx| picker.read(cx).sliders().lightness().clone());
        for &step in steps {
            cx.update(|window, cx| {
                slider.update(cx, |slider, cx| {
                    slider.set_value(step, window, cx);
                    cx.emit(SliderEvent::Change(step.into()));
                });
            });
        }
        cx.run_until_parked();
        slider
    }

    fn release(slider: &Entity<SliderState>, cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            slider.update(cx, |slider, cx| {
                let value = slider.value();
                cx.emit(SliderEvent::Release(value));
            });
        });
        cx.run_until_parked();
    }

    fn picker_color(picker: &Entity<ColorPickerState>, cx: &mut VisualTestContext) -> Rgb {
        cx.read(|cx| super::super::rgb_of(picker.read(cx).value().expect("a color")))
    }

    fn click_outside(cx: &mut VisualTestContext) {
        cx.simulate_click(point(px(790.), px(590.)), Modifiers::none());
        cx.run_until_parked();
    }

    /// Leaving the page by ⌥← never closes the picker, so a drag that waited
    /// for the close was lost; the release itself writes it.
    #[gpui::test]
    fn a_slider_release_writes_the_color_while_the_picker_stays_open(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let before = current(cx);
        let picker = picker_of(&panel, cx);
        click_custom_control(cx);
        let slider = drag_lightness(&picker, &[0.7, 0.4], cx);
        assert_eq!(current(cx), before, "a drag in progress writes nothing");
        assert_eq!(lighting_writes(&mut receiver), 0);

        release(&slider, cx);
        assert!(is_open(&picker, cx));
        let dragged = picker_color(&picker, cx);
        assert_ne!(dragged, before.1);
        assert_eq!(current(cx), (true, dragged));
        assert_eq!(lighting_writes(&mut receiver), 1, "the release writes once");

        click_outside(cx);
        assert_eq!(lighting_writes(&mut receiver), 0, "closing writes no more");
    }

    /// Type a color without Enter, dismiss the picker with `dismiss`, and
    /// check the typed color was written once.
    fn dismissing_writes_the_typed_color(
        cx: &mut TestAppContext,
        dismiss: impl FnOnce(&mut VisualTestContext),
    ) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let picker = picker_of(&panel, cx);
        click_custom_control(cx);
        type_hex(&picker, "00ffaa", cx);
        assert_eq!(lighting_writes(&mut receiver), 0, "typing writes nothing");
        let typed = gpui::Hsla::from(gpui::rgb(CUSTOM.packed()));
        let sliders = cx.read(|cx| {
            let sliders = picker.read(cx).sliders();
            [sliders.hue(), sliders.saturation(), sliders.lightness()]
                .map(|slider| slider.read(cx).value().start())
        });
        for (slider, expected) in sliders.into_iter().zip([typed.h, typed.s, typed.l]) {
            assert!(
                (slider - expected).abs() < 0.01,
                "the sliders preview the typed color: {sliders:?} vs {typed:?}"
            );
        }
        dismiss(cx);
        assert!(!is_open(&picker, cx));
        assert_eq!(current(cx), (true, CUSTOM));
        assert_eq!(lighting_writes(&mut receiver), 1);
    }

    #[gpui::test]
    fn a_typed_hex_color_is_written_when_a_click_outside_dismisses_it(cx: &mut TestAppContext) {
        dismissing_writes_the_typed_color(cx, click_outside);
    }

    #[gpui::test]
    fn a_typed_hex_color_is_written_when_escape_dismisses_it(cx: &mut TestAppContext) {
        dismissing_writes_the_typed_color(cx, |cx| {
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
        });
    }

    /// A keyboard with no saved lighting shows the default, white; picking
    /// white is still the first explicit choice, so it is saved and sent.
    #[gpui::test]
    fn the_first_pick_is_written_even_when_it_matches_the_default(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        assert_eq!(current(cx), (true, Rgb::WHITE), "nothing saved yet");
        let picker = picker_of(&panel, cx);
        click_custom_control(cx);
        type_hex(&picker, "FFFFFF", cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!is_open(&picker, cx));
        // `commit_lighting` saves before it sends, so the send proves both.
        assert_eq!(lighting_writes(&mut receiver), 1);
    }

    /// Leaving the page (⌥←, the back button, another tab or device) stops
    /// drawing the panel without closing the picker; the typed color must
    /// not be lost with it.
    #[gpui::test]
    fn a_typed_hex_color_is_written_when_the_page_is_left(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let mut receiver = install_keyboard(cx);
        let (page, cx) = cx.add_window_view(|window, cx| Page {
            panel: cx.new(|cx| LightingPanel::new(window, cx)),
            shown: true,
        });
        cx.simulate_resize(size(px(800.), px(600.)));
        // Focus events, and so `on_blur`, reach only the active window.
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        lighting_writes(&mut receiver);
        let panel = cx.read(|cx| page.read(cx).panel.clone());
        let picker = picker_of(&panel, cx);
        click_custom_control(cx);
        type_hex(&picker, "00ffaa", cx);
        assert_eq!(lighting_writes(&mut receiver), 0, "typing writes nothing");

        cx.update(|_, cx| {
            page.update(cx, |page, cx| {
                page.shown = false;
                cx.notify();
            });
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        assert_eq!(current(cx), (true, CUSTOM));
        assert_eq!(lighting_writes(&mut receiver), 1);
    }

    /// Clicking into the picker's sliders after typing leaves the hex field
    /// too; the typed color is written once, and a drag from there is its
    /// own write.
    #[gpui::test]
    fn a_typed_color_is_written_once_when_the_field_loses_focus(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let (panel, mut receiver, cx) = panel(cx);
        lighting_writes(&mut receiver);
        let picker = picker_of(&panel, cx);
        click_custom_control(cx);
        type_hex(&picker, "00ffaa", cx);
        cx.update(|window, cx| {
            picker.read(cx).focus_handle(cx).focus(window, cx);
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        assert!(is_open(&picker, cx));
        assert_eq!(current(cx), (true, CUSTOM));
        assert_eq!(lighting_writes(&mut receiver), 1);
        click_outside(cx);
        assert_eq!(lighting_writes(&mut receiver), 0, "closing writes no more");
    }
}
