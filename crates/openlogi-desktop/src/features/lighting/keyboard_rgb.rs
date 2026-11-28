//! RGB keyboard lighting controls.
//!
//! A palette of color swatches, an on/off toggle, and a brightness slider,
//! persisted per device via [`AppState::commit_lighting`] and pushed to the
//! keyboard through `openlogi_agent_core::hardware::set_lighting_in_background`
//! (the agent, over IPC — the GUI has no device I/O of its own).

use crate::ui::theme::FocusRingStyle as _;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, Role,
    StatefulInteractiveElement as _, Styled, Subscription, Toggled, Window, div, px, rgb,
};
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{Selectable as _, h_flex, slider::Slider, v_flex};
use openlogi_core::color::Rgb;
use openlogi_core::config::Lighting;

use crate::state::{AppState, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange};
use crate::ui::components::{Toggle, toggle_row};
use crate::ui::slider_row::SliderRow;
use crate::ui::theme::{self, Palette};

/// The coloured face of a swatch.
const SWATCH: f32 = 24.;
/// The selection ring around a swatch, and the clear gap between the two.
const SWATCH_RING: f32 = 2.;
const SWATCH_RING_GAP: f32 = 2.;

/// Preset colors. Deliberately small — covering the common keyboard accent
/// colors.
const PALETTE: &[Rgb] = &[
    Rgb::new(0xff, 0x3b, 0x30),
    Rgb::new(0xff, 0x95, 0x00),
    Rgb::new(0xff, 0xcc, 0x00),
    Rgb::new(0x34, 0xc7, 0x59),
    Rgb::new(0x00, 0xc7, 0xbe),
    Rgb::new(0x00, 0x7a, 0xff),
    Rgb::new(0x58, 0x56, 0xd6),
    Rgb::new(0xaf, 0x52, 0xde),
    Rgb::WHITE,
];

pub struct LightingPanel {
    brightness: CommitSlider<u8>,
    _state_obs: Subscription,
}

impl LightingPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let initial = AppState::try_read(cx).map_or(100, |s| s.lighting().brightness);
        // The slider drives the device only on release, to avoid streaming a
        // frame burst to the keyboard for every intermediate drag value.
        let brightness = CommitSlider::new(
            SliderRange::new(0, 100).step(5.),
            initial,
            cx,
            |_, pct, cx| {
                AppState::apply(cx, |state| {
                    let mut lighting = state.lighting();
                    lighting.enabled = true;
                    lighting.brightness = pct;
                    state.commit_lighting(lighting)
                });
            },
        );
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::LightingChanged(_)));
        Self {
            brightness,
            _state_obs: state_obs,
        }
    }
}

impl Render for LightingPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let lighting = AppState::try_read(cx).map_or_else(Default::default, AppState::lighting);

        // Pull the slider thumb to the active device's brightness whenever it
        // changed in `AppState` (device switch / external edit), without
        // disturbing an in-progress drag.
        self.brightness.sync(lighting.brightness, window, cx);

        let swatches: Vec<_> = PALETTE
            .iter()
            .map(|&color| swatch(color, &lighting, pal))
            .collect();

        v_flex()
            .gap_3()
            .w_full()
            // The tab and the card already say "Lighting"; the row names what
            // the switch does to the keyboard instead.
            .child(toggle_row(
                "light-row",
                pal,
                tr!("device.light_the_keys"),
                tr!("lighting.keyboard_lighting_description"),
                Toggle::new("light-toggle")
                    .selected(lighting.enabled)
                    .on_change(|enabled, _window, cx| {
                        AppState::apply(cx, |state| {
                            let mut next = state.lighting();
                            next.enabled = *enabled;
                            state.commit_lighting(next)
                        });
                    }),
            ))
            // The rings' clear gaps already space the faces; a full gap on top
            // would spread the row.
            .child(h_flex().gap_1().flex_wrap().children(swatches))
            .child(
                SliderRow::new(
                    tr!("camera.brightness"),
                    Slider::new(self.brightness.slider()).horizontal(),
                )
                .value(
                    format!("{}%", self.brightness.shown(lighting.brightness)),
                    ["100%"],
                ),
            )
    }
}

/// One color swatch. Clicking it turns lighting on and sets that color.
///
/// The selection ring sits a gap outside the colour, the way the keypad's
/// photo keys are ringed: a ring drawn on the swatch's own edge vanishes
/// against a blue or indigo swatch. The ring also shows, muted, under the
/// pointer, so the swatches read as pressable before one is pressed.
fn swatch(color: Rgb, current: &Lighting, pal: Palette) -> impl IntoElement {
    let selected = current.enabled && current.color == color;
    let outer = SWATCH_RING + SWATCH_RING_GAP;
    BaseButton::new(("light-swatch", color.packed()))
        .role(Role::RadioButton)
        .selected(selected)
        .accessibility_label(format!(
            "{} #{:06X}",
            tr!("device.lighting"),
            color.packed()
        ))
        .aria_toggled(if selected {
            Toggled::True
        } else {
            Toggled::False
        })
        .aria_selected(selected)
        .size(px(SWATCH + 2. * outer))
        .p(px(SWATCH_RING_GAP))
        // The ring follows the swatch's corner, one gap further out.
        .rounded(pal.control_radius + px(outer))
        .border(px(SWATCH_RING))
        .border_color(if selected {
            theme::accent()
        } else {
            gpui::transparent_black()
        })
        .hover(move |style| {
            style.border_color(if selected {
                theme::accent()
            } else {
                pal.text_muted
            })
        })
        .cursor_pointer()
        .focus_visible(move |style| style.focus_ring(pal))
        .child(
            // The hairline keeps a white or pale swatch visible on the card.
            div()
                .size_full()
                .rounded(pal.control_radius)
                .border_1()
                .border_color(pal.border)
                .bg(rgb(color.packed())),
        )
        .on_click(move |_event, _window, cx| {
            AppState::apply(cx, |state| {
                let mut next = state.lighting();
                next.enabled = true;
                next.color = color;
                state.commit_lighting(next)
            });
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::state::tests::{KNOWN_MOUSE_KEY, direct_inventory, reload::agent_loaded};
    use crate::state::{ConfigPersistence, Sources};
    use gpui::AppContext as _;
    use openlogi_core::config::{Config, ConfigEdit, ConfigFile};
    use openlogi_core::device::{Capabilities, DeviceKind};

    #[gpui::test]
    fn external_rgb_save_repaints_open_lighting_panel(cx: &mut gpui::TestAppContext) {
        use std::{cell::Cell, rc::Rc};

        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let mut initial = Config::default();
        initial.set_lighting(KNOWN_MOUSE_KEY, Lighting::default());
        initial.save_to_path(&path).unwrap();
        let (config, file) = ConfigFile::load_from_path(&path).unwrap();
        let before = file.revision();
        let resolver = AssetResolver::new();
        let mut keyboard = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
        keyboard.paired[0].kind = DeviceKind::Keyboard;
        keyboard.paired[0].capabilities = Some(Capabilities {
            lighting: true,
            ..Capabilities::default()
        });
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::new(Sources {
            inventories: &[keyboard],
            persistence: ConfigPersistence::UserFile(file),
            ..Sources::in_memory(config, &resolver, commands)
        });
        // Settle connection state before opening the panel: the only later
        // invalidation must be the externally changed lighting itself.
        let _ = state.apply_agent_snapshot(&agent_loaded(before), &resolver, &[]);
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::register_builtin_themes(cx);
            AppState::set_global(cx.new(|_| state), cx);
        });
        let (view, cx) = cx.add_window_view(|_, cx| LightingPanel::new(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.read(|cx| assert_eq!(view.read(cx).brightness.seated(), 100));
        cx.run_until_parked();
        let repainted = Rc::new(Cell::new(false));
        let _watch = cx.update(|_, cx| {
            let repainted = Rc::clone(&repainted);
            cx.observe(&view, move |_, _| repainted.set(true))
        });

        let mut external = ConfigEdit::load(&path).unwrap();
        let changed = Lighting {
            enabled: false,
            color: Rgb::WHITE,
            brightness: 35,
        };
        external
            .config_mut()
            .set_lighting(KNOWN_MOUSE_KEY, changed.clone());
        let revision = external.commit().unwrap();
        cx.update(|_, cx| {
            AppState::apply(cx, |state| {
                state
                    .apply_agent_snapshot(&agent_loaded(revision), &resolver, &[])
                    .events
            });
        });
        cx.run_until_parked();
        assert!(
            repainted.get(),
            "the external save must invalidate the open panel before drawing"
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.read(|cx| {
            assert_eq!(
                view.read(cx).brightness.seated(),
                35,
                "the retained slider follows the external save without reopening the tab"
            );
        });
        cx.read(|cx| assert_eq!(AppState::try_read(cx).unwrap().lighting(), changed));
    }
}
