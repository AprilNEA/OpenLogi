use gpui::{AnyElement, Context, IntoElement, ParentElement};
use gpui_kit::component::h_flex;
use crate::ui::theme::Palette;

pub fn os_pointer_speed_section(cx: &mut Context<crate::features::pointer::dpi::DpiPanel>, _pal: Palette) -> AnyElement {
    let value = crate::state::AppState::try_read(cx).and_then(|s| s.current_os_pointer_speed()).unwrap_or(1.0);
    h_flex().child(format!("OS pointer speed: {:.2}", value)).into_any_element()
}
