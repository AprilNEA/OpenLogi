//! Compact transient keyboard-backlight indicator.

use gpui::{
    App, Bounds, Context, IntoElement, ParentElement, Render, Size, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, px,
};
use openlogi_core::hid::backlight::max_backlight_level;
use openlogi_ipc::BacklightObservation;
use openlogi_ui::color::{self, neutral};

use crate::platform::IndicatorBackend;

pub(crate) struct BacklightView {
    pub(crate) observation: BacklightObservation,
}

impl BacklightView {
    pub(crate) fn new(observation: BacklightObservation) -> Self {
        Self { observation }
    }
}

impl Render for BacklightView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let max_level = max_backlight_level(self.observation.levels);
        // The settings app's own "Backlight" label, already translated in
        // every catalog; the level and its maximum are plain numbers.
        let level = format!(
            "{} {} / {}",
            rust_i18n::t!("device.backlight"),
            self.observation.current_level,
            max_level
        );
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .w(px(280.0))
            .h(px(88.0))
            .rounded(px(12.0))
            .bg(neutral(0.08, 0.94))
            .text_color(neutral(0.96, 1.0))
            .child(level)
            .child(
                div()
                    .w(px(220.0))
                    .h(px(8.0))
                    .rounded(px(4.0))
                    .bg(neutral(0.25, 1.0))
                    .child(
                        div()
                            .h_full()
                            .w(px(220.0
                                * relative_level(
                                    self.observation.current_level,
                                    max_level,
                                )))
                            .rounded(px(4.0))
                            // The brand accent, as the settings app and the
                            // lock-key glyph paint it.
                            .bg(color::accent()),
                    ),
            )
    }
}

fn relative_level(level: u8, max_level: u8) -> f32 {
    if max_level == 0 {
        0.0
    } else {
        f32::from(level.min(max_level)) / f32::from(max_level)
    }
}

pub(crate) fn window_options(cx: &mut App) -> WindowOptions {
    let bounds = Bounds::centered(None, Size::new(px(280.0), px(88.0)), cx);
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-backlight-osd".to_string()),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

/// Open the backlight indicator with the captured backend's preferred surface,
/// falling back to its ordinary window if the preferred surface cannot open.
pub(crate) fn open_window<H>(
    options: WindowOptions,
    backend: IndicatorBackend,
    open: impl FnMut(WindowOptions) -> anyhow::Result<H>,
) -> Option<H> {
    let alternatives = backend
        .windows(options)
        .into_iter()
        .map(|options| vec![options])
        .collect();
    crate::platform::open_first(alternatives, open)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn backlight_window_selection(cx: &mut gpui::TestAppContext) {
        let cases = [(IndicatorBackend::Plain, false, &["popup"][..])];
        #[cfg(target_os = "linux")]
        let cases = cases.into_iter().chain([
            (IndicatorBackend::WaylandLayerShell, false, &["layer"][..]),
            (
                IndicatorBackend::WaylandLayerShell,
                true,
                &["layer", "popup"][..],
            ),
        ]);

        for (backend, reject_layer, expected) in cases {
            let options = cx.update(window_options);
            let bounds = options.window_bounds;
            let mut attempted = Vec::new();
            let opened = open_window(options, backend, |options| {
                assert_eq!(options.app_id.as_deref(), Some("openlogi-backlight-osd"));
                assert_eq!(options.window_bounds, bounds);
                assert!(!options.focus);
                assert!(!options.is_movable);
                assert!(!options.is_resizable);
                let kind = match options.kind {
                    #[cfg(target_os = "linux")]
                    WindowKind::LayerShell(layer) => {
                        use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer};

                        assert_eq!(layer.namespace, "openlogi-backlight-osd");
                        assert_eq!(layer.layer, Layer::Overlay);
                        assert_eq!(layer.anchor, Anchor::BOTTOM);
                        assert_eq!(layer.exclusive_zone, None);
                        assert_eq!(layer.keyboard_interactivity, KeyboardInteractivity::None);
                        assert_eq!(
                            layer.margin,
                            Some((px(0.0), px(0.0), px(crate::notice::BOTTOM_MARGIN), px(0.0)))
                        );
                        "layer"
                    }
                    WindowKind::PopUp => "popup",
                    other => panic!("unexpected backlight window kind: {other:?}"),
                };
                attempted.push(kind);
                if reject_layer && kind == "layer" {
                    anyhow::bail!("compositor refused the preferred layer surface");
                }
                Ok(attempted.len())
            });
            assert_eq!(attempted, expected, "backlight opening order");
            assert_eq!(opened, Some(expected.len()), "a backlight window opened");
        }
    }

    #[test]
    fn highest_reported_level_fills_the_bar() {
        assert_eq!(relative_level(7, max_backlight_level(8)), 1.0);
        assert_eq!(relative_level(0, max_backlight_level(8)), 0.0);
    }
}
