//! Compact transient keyboard-backlight indicator.

use gpui::{
    Context, IntoElement, ParentElement, Render, Styled, Window, WindowBackgroundAppearance,
    WindowKind, WindowOptions, div, px,
};
use openlogi_ipc::BacklightObservation;

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
        let level = format!(
            "Backlight {} / {}",
            self.observation.current_level, self.observation.levels
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
            .bg(gpui::hsla(0.0, 0.0, 0.08, 0.94))
            .text_color(gpui::hsla(0.0, 0.0, 0.96, 1.0))
            .child(level)
            .child(
                div()
                    .w(px(220.0))
                    .h(px(8.0))
                    .rounded(px(4.0))
                    .bg(gpui::hsla(0.0, 0.0, 0.25, 1.0))
                    .child(
                        div()
                            .h_full()
                            .w(px(220.0
                                * relative_level(
                                    self.observation.current_level,
                                    self.observation.levels,
                                )))
                            .rounded(px(4.0))
                            .bg(gpui::hsla(0.58, 0.75, 0.62, 1.0)),
                    ),
            )
    }
}

fn relative_level(level: u8, levels: u8) -> f32 {
    if levels == 0 {
        0.0
    } else {
        f32::from(level.min(levels)) / f32::from(levels)
    }
}

pub(crate) fn window_options() -> WindowOptions {
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
        ..WindowOptions::default()
    }
}
