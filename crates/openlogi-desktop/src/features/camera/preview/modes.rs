//! The resolution and frame-rate dropdowns under the live preview.
//!
//! One dropdown lists every frame size the camera offers, the other every rate
//! it offers at the selected size, so neither presents a mode the camera would
//! refuse. A pick is saved through [`AppState::commit_camera_stream_mode`]; the
//! preview hears the change and reopens its stream at the new mode.
//!
//! [`ModePicker`] owns the two [`SelectState`]s, which need a window to exist
//! and keep their own selection and open/closed state. The preview owns the
//! facts they show — the camera's modes and the saved choice — and hands them
//! over each render through [`ModePicker::show`], which only touches a select
//! whose contents actually differ.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Window, div, px,
};
use gpui_component::{
    h_flex,
    select::{SelectEvent, SelectItem, SelectState},
    v_flex,
};
use openlogi_camera::{StreamMode, StreamModes};

use super::PREVIEW_W;
use crate::state::AppState;
use crate::ui::components::control_select;
use crate::ui::theme::{self, Typography as _};

/// What the dropdowns show for the selected camera.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ModeChoice {
    /// Where the camera's choice is saved.
    pub(super) config_key: String,
    /// Every mode the camera offers.
    pub(super) modes: StreamModes,
    /// The mode the stream resolves to. Always one of `modes`
    /// ([`StreamModes::resolve`] guarantees it), so a stale saved choice shows
    /// the default it fell back to.
    pub(super) selected: StreamMode,
}

/// A frame size in the resolution dropdown.
#[derive(Clone)]
pub(super) struct ResolutionOption((u32, u32));

impl SelectItem for ResolutionOption {
    type Value = (u32, u32);

    fn title(&self) -> SharedString {
        format!("{}×{}", self.0.0, self.0.1).into()
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

/// A frame rate in the rate dropdown.
#[derive(Clone)]
pub(super) struct RateOption(u32);

impl SelectItem for RateOption {
    type Value = u32;

    /// Looked up per call rather than cached, so a language change reaches a
    /// dropdown that stays on screen.
    fn title(&self) -> SharedString {
        tr!("camera.frame_rate_fps", fps => self.0)
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

/// The frame sizes on offer, largest first.
fn resolution_options(modes: &StreamModes) -> Vec<ResolutionOption> {
    modes
        .resolutions()
        .into_iter()
        .map(ResolutionOption)
        .collect()
}

/// The rates on offer at `resolution`, fastest first.
fn rate_options(modes: &StreamModes, resolution: (u32, u32)) -> Vec<RateOption> {
    modes
        .rates_at(resolution)
        .into_iter()
        .map(RateOption)
        .collect()
}

/// The two dropdowns and the line that says a pick applies to the preview only.
pub(super) struct ModePicker {
    pub(super) resolution: Entity<SelectState<Vec<ResolutionOption>>>,
    pub(super) rate: Entity<SelectState<Vec<RateOption>>>,
    /// What was last shown, for the pick handlers: the key to save under and
    /// the modes to resolve a pick against.
    shown: Option<ModeChoice>,
    _subscriptions: [Subscription; 2],
}

impl ModePicker {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let resolution = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let rate = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let subscriptions = [
            cx.subscribe(&resolution, Self::on_resolution),
            cx.subscribe(&rate, Self::on_rate),
        ];
        Self {
            resolution,
            rate,
            shown: None,
            _subscriptions: subscriptions,
        }
    }

    /// Make the dropdowns show `choice`.
    ///
    /// Called from the preview's render, so it only writes to a select whose
    /// list or selection is not already what it should be: a write notifies,
    /// and rewriting an unchanged select every frame would repaint forever. The
    /// selection is compared with the select's own, not with what was last
    /// shown, so a pick the state refused is put back.
    pub(super) fn show(&mut self, choice: ModeChoice, window: &mut Window, cx: &mut Context<Self>) {
        let selected = choice.selected;
        let shown = self.shown.as_ref();
        let sizes_changed = shown.map(|shown| &shown.modes) != Some(&choice.modes);
        // The rates on offer follow the size, so a new size is a new list.
        let rates_changed = sizes_changed
            || shown.map(|shown| shown.selected.resolution()) != Some(selected.resolution());

        // A replaced list keeps the old selection, whose index may mean another
        // row now, so it is selected again even when the value is the same.
        if sizes_changed {
            let options = resolution_options(&choice.modes);
            self.resolution
                .update(cx, |select, cx| select.set_items(options, window, cx));
        }
        if sizes_changed
            || self.resolution.read(cx).selected_value() != Some(&selected.resolution())
        {
            self.resolution.update(cx, |select, cx| {
                select.set_selected_value(&selected.resolution(), window, cx);
            });
        }

        if rates_changed {
            let options = rate_options(&choice.modes, selected.resolution());
            self.rate
                .update(cx, |select, cx| select.set_items(options, window, cx));
        }
        if rates_changed || self.rate.read(cx).selected_value() != Some(&selected.fps) {
            self.rate.update(cx, |select, cx| {
                select.set_selected_value(&selected.fps, window, cx);
            });
        }

        if self.shown.as_ref() != Some(&choice) {
            self.shown = Some(choice);
            cx.notify();
        }
    }

    /// Save `mode` for the camera the dropdowns are showing.
    fn save(&self, mode: StreamMode, cx: &mut Context<Self>) {
        let Some(shown) = &self.shown else {
            return;
        };
        let key = shown.config_key.clone();
        AppState::apply(cx, |state| state.commit_camera_stream_mode(&key, mode));
    }

    /// A new size keeps the frame rate when it offers it, and takes the nearest
    /// one it does offer otherwise ([`StreamModes::with_resolution`]).
    fn on_resolution(
        &mut self,
        _: Entity<SelectState<Vec<ResolutionOption>>>,
        event: &SelectEvent<Vec<ResolutionOption>>,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(resolution)) = event else {
            return;
        };
        let Some(shown) = &self.shown else {
            return;
        };
        if let Some(mode) = shown.modes.with_resolution(shown.selected, *resolution) {
            self.save(mode, cx);
        }
    }

    fn on_rate(
        &mut self,
        _: Entity<SelectState<Vec<RateOption>>>,
        event: &SelectEvent<Vec<RateOption>>,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(fps)) = event else {
            return;
        };
        let Some(shown) = &self.shown else {
            return;
        };
        let mode = StreamMode {
            fps: *fps,
            ..shown.selected
        };
        if shown.modes.contains(mode) {
            self.save(mode, cx);
        }
    }
}

impl Render for ModePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let field = |label: SharedString, select: AnyElement| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(label),
                )
                // A select's root fills its parent, so give it a control's height.
                .child(
                    div()
                        .w_full()
                        .h(px(theme::CONTROL_H))
                        .child(select),
                )
        };

        v_flex()
            .w(PREVIEW_W)
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(field(
                        tr!("camera.resolution"),
                        control_select(&self.resolution)
                            .accessibility_label(tr!("camera.resolution"))
                            .into_any_element(),
                    ))
                    .child(field(
                        tr!("camera.frame_rate"),
                        control_select(&self.rate)
                            .accessibility_label(tr!("camera.frame_rate"))
                            .into_any_element(),
                    )),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("camera.stream_note")),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, fps: u32) -> StreamMode {
        StreamMode { width, height, fps }
    }

    fn modes() -> StreamModes {
        StreamModes::new([
            mode(1920, 1080, 60),
            mode(1920, 1080, 30),
            mode(1280, 720, 30),
            mode(640, 360, 30),
        ])
    }

    #[test]
    fn the_resolution_list_is_every_size_largest_first() {
        let sizes: Vec<_> = resolution_options(&modes())
            .into_iter()
            .map(|option| option.0)
            .collect();
        assert_eq!(sizes, [(1920, 1080), (1280, 720), (640, 360)]);
    }

    #[test]
    fn the_rate_list_is_only_what_the_selected_size_offers_fastest_first() {
        let rates = |size| -> Vec<_> {
            rate_options(&modes(), size)
                .into_iter()
                .map(|option| option.0)
                .collect()
        };
        assert_eq!(rates((1920, 1080)), [60, 30]);
        assert_eq!(rates((1280, 720)), [30]);
        assert!(
            rates((800, 600)).is_empty(),
            "a size the camera does not offer lists no rates"
        );
    }

    #[test]
    fn a_size_is_titled_width_by_height() {
        assert_eq!(ResolutionOption((1920, 1080)).title().as_ref(), "1920×1080");
    }
}
