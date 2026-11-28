//! Compact transient keyboard-backlight indicator.

use gpui::{
    App, AppContext as _, Bounds, Context, IntoElement, ParentElement, Render, Size, Styled,
    Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, px,
};
use openlogi_core::hid::backlight::max_backlight_level;
use openlogi_ipc::BacklightObservation;
use openlogi_ui::color::{self, neutral};
use tracing::debug;

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

/// Localized OS window title for the keyboard-backlight indicator window.
pub(crate) fn window_title() -> String {
    rust_i18n::t!("overlay.backlight_window_title").into_owned()
}

/// Show `observation`, update the open indicator in place, or close it when
/// `observation.visible` is `false`.
pub(crate) fn present(cx: &mut App, observation: &BacklightObservation) {
    let title = window_title();
    for handle in cx.windows() {
        if let Some(view) = handle.downcast::<BacklightView>() {
            let _ = view.update(cx, |view, window, cx| {
                window.set_window_title(&title);
                view.observation = observation.clone();
                cx.notify();
            });
        }
    }
    if !observation.visible {
        for handle in cx.windows() {
            if handle.downcast::<BacklightView>().is_some() {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
        }
        return;
    }
    if cx
        .windows()
        .iter()
        .all(|handle| handle.downcast::<BacklightView>().is_none())
    {
        let options = window_options(cx);
        let opened = cx.open_window(options, |window, cx| {
            window.set_window_title(&title);
            cx.new(|_| BacklightView::new(observation.clone()))
        });
        debug!(success = opened.is_ok(), "backlight window opened");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highest_reported_level_fills_the_bar() {
        assert_eq!(relative_level(7, max_backlight_level(8)), 1.0);
        assert_eq!(relative_level(0, max_backlight_level(8)), 0.0);
    }
}
