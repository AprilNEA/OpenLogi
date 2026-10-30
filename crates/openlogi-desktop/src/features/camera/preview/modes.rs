//! The resolution and frame-rate rows under the live preview.
//!
//! One pill per frame size the camera offers, and one per rate it offers at the
//! selected size, so the rows never present a mode the camera would refuse. A
//! pick is saved through [`AppState::commit_camera_stream_mode`]; the preview
//! hears the change and reopens its stream at the new mode.

use gpui::{AnyElement, IntoElement, ParentElement, SharedString, Styled, div};
use gpui_component::{Selectable as _, h_flex, v_flex};
use openlogi_camera::{StreamMode, StreamModes};

use super::PREVIEW_W;
use crate::state::AppState;
use crate::ui::components::Pill;
use crate::ui::theme::{Palette, Typography as _};

/// The rows for the camera saved under `config_key`, with `selected` marked.
///
/// `selected` must be one of `modes` ([`StreamModes::resolve`] guarantees it),
/// or no pill is marked. Changing the size keeps the frame rate when the new
/// size offers it, and takes the nearest one it does offer otherwise
/// ([`StreamModes::with_resolution`]).
pub(super) fn mode_section(
    config_key: &str,
    modes: &StreamModes,
    selected: StreamMode,
    pal: Palette,
) -> AnyElement {
    let resolutions =
        modes
            .resolutions()
            .into_iter()
            .enumerate()
            .filter_map(|(index, resolution)| {
                let target = modes.with_resolution(selected, resolution)?;
                Some(pill(
                    ("camera-resolution", index),
                    format!("{}×{}", resolution.0, resolution.1),
                    resolution == selected.resolution(),
                    config_key,
                    target,
                ))
            });
    let rates = modes
        .rates_at(selected.resolution())
        .into_iter()
        .enumerate()
        .map(|(index, fps)| {
            pill(
                ("camera-frame-rate", index),
                tr!("camera.frame_rate_fps", fps => fps),
                fps == selected.fps,
                config_key,
                StreamMode { fps, ..selected },
            )
        });

    v_flex()
        .w(PREVIEW_W)
        .gap_3()
        .child(row(tr!("camera.resolution"), resolutions, pal))
        .child(row(tr!("camera.frame_rate"), rates, pal))
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("camera.stream_note")),
        )
        .into_any_element()
}

/// A captioned, wrapping row of pills.
fn row(
    label: impl Into<SharedString>,
    pills: impl IntoIterator<Item = Pill>,
    pal: Palette,
) -> impl IntoElement {
    v_flex()
        .gap_2()
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(label.into()),
        )
        .child(h_flex().gap_2().flex_wrap().children(pills))
}

/// One pill. Clicking it saves `mode` for the camera under `config_key`.
fn pill(
    id: (&'static str, usize),
    label: impl Into<SharedString>,
    selected: bool,
    config_key: &str,
    mode: StreamMode,
) -> Pill {
    let key = config_key.to_owned();
    Pill::new(id, label)
        .selected(selected)
        .on_click(move |_event, _window, cx| {
            AppState::apply(cx, |state| state.commit_camera_stream_mode(&key, mode));
        })
}
