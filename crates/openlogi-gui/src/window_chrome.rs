use gpui::{
    AnyElement, App, Decorations, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    WindowButton, WindowButtonLayout, WindowControlArea, div, px,
};
use gpui_component::{Icon, IconName, h_flex, tooltip::Tooltip, v_flex};

/// Wrap a window body with app-rendered controls when Linux client-side
/// decorations are active. Other platforms keep the native titlebar.
pub fn frame(
    title: impl Into<SharedString>,
    body: impl IntoElement,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    #[cfg(target_os = "linux")]
    {
        if matches!(window.window_decorations(), Decorations::Client { .. }) {
            return linux_frame(title.into(), body, window, cx);
        }
    }

    body.into_any_element()
}

#[cfg(target_os = "linux")]
fn linux_frame(
    title: SharedString,
    body: impl IntoElement,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let pal = crate::theme::palette(cx);

    v_flex()
        .size_full()
        .bg(pal.bg)
        .text_color(pal.text_primary)
        .child(linux_titlebar(title, window, cx))
        .child(div().flex_1().min_h_0().child(body))
        .into_any_element()
}

#[cfg(target_os = "linux")]
fn linux_titlebar(title: SharedString, window: &mut Window, cx: &mut App) -> impl IntoElement {
    let pal = crate::theme::palette(cx);
    let layout = cx
        .button_layout()
        .unwrap_or_else(WindowButtonLayout::linux_default);

    h_flex()
        .id("linux-titlebar")
        .window_control_area(WindowControlArea::Drag)
        .h(px(38.))
        .w_full()
        .flex_shrink_0()
        .items_center()
        .border_b_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            if event.click_count == 1 {
                window.start_window_move();
            }
        })
        .on_click(|event, window, _| {
            if event.click_count() == 2 {
                window.zoom_window();
            }
        })
        .on_mouse_down(MouseButton::Right, |event, window, _| {
            window.show_window_menu(event.position);
        })
        .child(linux_window_controls(
            "linux-titlebar-left-controls",
            layout.left,
            window,
            pal.surface_hover,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(pal.text_muted)
                .child(title),
        )
        .child(linux_window_controls(
            "linux-titlebar-right-controls",
            layout.right,
            window,
            pal.surface_hover,
        ))
}

#[cfg(target_os = "linux")]
fn linux_window_controls(
    id: &'static str,
    buttons: [Option<WindowButton>; gpui::MAX_BUTTONS_PER_SIDE],
    window: &mut Window,
    hover_bg: gpui::Hsla,
) -> impl IntoElement {
    let controls = window.window_controls();
    let is_maximized = window.is_maximized();
    let rendered = buttons.into_iter().flatten().filter_map(move |button| {
        match button {
            WindowButton::Minimize if !controls.minimize => return None,
            WindowButton::Maximize if !controls.maximize => return None,
            _ => {}
        }

        Some(linux_window_button(button, is_maximized, hover_bg))
    });

    h_flex()
        .id(id)
        .h_full()
        .min_w(px(8.))
        .items_center()
        .gap_2()
        .px_3()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .children(rendered)
}

#[cfg(target_os = "linux")]
fn linux_window_button(
    button: WindowButton,
    is_maximized: bool,
    hover_bg: gpui::Hsla,
) -> impl IntoElement {
    let (icon, label) = match button {
        WindowButton::Minimize => (IconName::Minimize, tr!("Minimize")),
        WindowButton::Maximize if is_maximized => (IconName::Maximize, tr!("Restore")),
        WindowButton::Maximize => (IconName::Maximize, tr!("Maximize")),
        WindowButton::Close => (IconName::Close, tr!("Close")),
    };

    h_flex()
        .id(format!("linux-window-control-{}", button.id()))
        .size(px(24.))
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .child(Icon::new(icon).size_4())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            match button {
                WindowButton::Minimize => window.minimize_window(),
                WindowButton::Maximize => window.zoom_window(),
                WindowButton::Close => window.remove_window(),
            }
        })
}
