use gpui::{TestAppContext, size};
use openlogi_core::config::{Config, UiScale};

use super::*;
use crate::services::assets::AssetResolver;
use crate::state::Sources;

/// The Actions Ring tab body at `width` px, in a window at `scale`.
struct NarrowTab {
    panel: Entity<ActionRingPanel>,
    width: f32,
    scale: UiScale,
}

impl Render for NarrowTab {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        theme::apply_scale(window, self.scale);
        div()
            .debug_selector(|| "ring-tab-body".into())
            .w(px(self.width))
            .child(self.panel.clone())
    }
}

/// At 125% in a 900 px window the tab body is 640 px wide: too narrow for the
/// fixed-size preview beside the scaled library, so the library must wrap
/// below the preview instead of running past the body's edge.
#[gpui::test]
fn a_narrow_tab_keeps_the_library_inside_its_body(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state =
            cx.new(|_| AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands)));
        AppState::set_global(state, cx);
    });
    let (_, cx) = cx.add_window_view(|_, cx| NarrowTab {
        panel: cx.new(ActionRingPanel::new),
        width: 640.,
        scale: UiScale::ExtraLarge,
    });
    cx.simulate_resize(size(px(900.), px(800.)));
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let body = cx.debug_bounds("ring-tab-body").expect("tab body");
    let library = cx
        .debug_bounds("ring-action-library-card")
        .expect("action library");
    assert!(
        library.right() <= body.right(),
        "library {library:?} overflows the tab body {body:?}"
    );
    assert!(
        library.top() >= body.top() + px(PREVIEW_SIZE),
        "library {library:?} should wrap below the preview"
    );
}
