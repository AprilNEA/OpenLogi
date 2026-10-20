//! Notices: one card per notice, stacked at the bottom of the screen, telling
//! the user something the agent saw — today, that a device's battery is low.
//!
//! The agent decides what is showing and for how long
//! ([`openlogi_ipc::NoticeObservation`]); this module only draws it. The window
//! opens when the first card arrives, is replaced whenever the cards change,
//! and closes when none are left. Clicking a card dismisses it here. The agent
//! still lists it until its time is up, so [`NoticeShelf`] remembers the
//! dismissal for exactly as long as the agent does.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    Pixels, Render, SharedString, Size, StatefulInteractiveElement as _, Styled, Window,
    WindowBackgroundAppearance, WindowKind, WindowOptions, div, px, size, svg,
};
use openlogi_ipc::{Notice, NoticeKind};
use openlogi_ui::action_icons::LOW_BATTERY_ICON;
use tracing::warn;

use crate::platform::NoticePlacement;

/// The most cards shown at once. Older ones wait out their time unseen rather
/// than push the stack up the screen.
pub(crate) const MAX_CARDS: usize = 3;
pub(crate) const CARD_WIDTH: f32 = 320.0;
pub(crate) const CARD_HEIGHT: f32 = 64.0;
pub(crate) const CARD_GAP: f32 = 8.0;
/// Clear space around the stack, so the cards' shadow is not cut off at the
/// window edge.
pub(crate) const STACK_INSET: f32 = 12.0;
/// How far the stack sits above the bottom of the display's usable area.
pub(crate) const BOTTOM_MARGIN: f32 = 48.0;
/// Window identity for notices, distinct from the ring's so a compositor or a
/// window rule can tell them apart.
pub(crate) const NOTICE_APP_ID: &str = "openlogi-notice";

/// Card colours. Like the ring, a notice floats over whatever is on screen, so
/// it commits to a dark card instead of following the OS appearance.
const CARD: Hsla = neutral(0.10, 0.94);
const TITLE: Hsla = neutral(0.98, 1.0);
const DETAIL: Hsla = neutral(0.78, 1.0);
/// The battery glyph: amber, the colour of a warning that needs no action yet.
const WARNING: Hsla = Hsla {
    h: 38.0 / 360.0,
    s: 0.92,
    l: 0.56,
    a: 1.0,
};

const fn neutral(lightness: f32, alpha: f32) -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: lightness,
        a: alpha,
    }
}

/// What the overlay remembers between the agent's lists: the latest list,
/// which cards the user dismissed, and which cards are on screen.
#[derive(Debug, Default)]
pub(crate) struct NoticeShelf {
    dismissed: BTreeSet<u64>,
    /// The agent's latest list, including notices beyond [`MAX_CARDS`], so a
    /// dismissal can make room for one of them without a new list.
    current: Vec<Notice>,
    showing: Vec<Notice>,
}

impl NoticeShelf {
    /// Take the agent's latest list. Returns the cards to show when they
    /// differ from the cards on screen, and `None` when nothing visible
    /// changed.
    pub(crate) fn observe(&mut self, notices: Vec<Notice>) -> Option<Vec<Notice>> {
        // A dismissal only has to outlive the agent's own copy of the notice.
        self.dismissed
            .retain(|id| notices.iter().any(|notice| notice.id == *id));
        self.current = notices;
        let cards = visible(&self.current, &self.dismissed);
        (cards != self.showing).then(|| self.show(cards))
    }

    /// The user dismissed card `id`. Returns the cards to show now, including
    /// a waiting notice that moves into the freed place.
    pub(crate) fn dismiss(&mut self, id: u64) -> Vec<Notice> {
        self.dismissed.insert(id);
        let cards = visible(&self.current, &self.dismissed);
        self.show(cards)
    }

    fn show(&mut self, cards: Vec<Notice>) -> Vec<Notice> {
        self.showing.clone_from(&cards);
        cards
    }
}

/// The newest [`MAX_CARDS`] notices the user has not dismissed, oldest first.
fn visible(notices: &[Notice], dismissed: &BTreeSet<u64>) -> Vec<Notice> {
    let mut cards: Vec<Notice> = notices
        .iter()
        .filter(|notice| !dismissed.contains(&notice.id))
        .cloned()
        .collect();
    let excess = cards.len().saturating_sub(MAX_CARDS);
    cards.drain(..excess);
    cards
}

/// The window size that holds `count` cards and the clear space around them.
pub(crate) fn stack_size(count: usize) -> Size<Pixels> {
    let count = count.clamp(1, MAX_CARDS);
    // `count` is at most `MAX_CARDS`, so the conversion is exact.
    let cards = f32::from(u8::try_from(count).unwrap_or(u8::MAX));
    let stack_height = CARD_HEIGHT * cards + CARD_GAP * (cards - 1.0);
    size(
        px(CARD_WIDTH + 2.0 * STACK_INSET),
        px(stack_height + 2.0 * STACK_INSET),
    )
}

/// Top-left of a window of `window` size placed at the bottom centre of
/// `area`, [`BOTTOM_MARGIN`] above its bottom edge, and kept inside it.
///
/// Windows places the notice in physical pixels instead (see
/// `platform/windows.rs`), so only the other platforms and the tests use this.
#[cfg(any(not(windows), test))]
pub(crate) fn bottom_centre(
    area: gpui::Bounds<Pixels>,
    window: Size<Pixels>,
) -> gpui::Point<Pixels> {
    let desired = gpui::point(
        area.origin.x + (area.size.width - window.width) / 2.0,
        area.bottom() - window.height - px(BOTTOM_MARGIN),
    );
    let max = gpui::point(
        (area.right() - window.width).max(area.origin.x),
        (area.bottom() - window.height).max(area.origin.y),
    );
    desired.clamp(&area.origin, &max)
}

/// Appearance shared by the platform-specific notice placements.
pub(crate) fn notice_window_options() -> WindowOptions {
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some(NOTICE_APP_ID.to_string()),
        ..WindowOptions::default()
    }
}

/// The two lines of a card: what happened, and to which device.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NoticeText {
    pub(crate) title: SharedString,
    pub(crate) detail: SharedString,
}

/// The text of one notice, in the active locale.
pub(crate) fn notice_text(kind: &NoticeKind) -> NoticeText {
    match kind {
        NoticeKind::LowBattery {
            name,
            kind,
            battery,
        } => {
            let device = name
                .clone()
                .unwrap_or_else(|| rust_i18n::t!(kind.translation_key()).into_owned());
            let detail = match battery.percentage {
                Some(percentage) => format!("{device} · {percentage}%"),
                None => device,
            };
            NoticeText {
                title: rust_i18n::t!("device.low_battery").into_owned().into(),
                detail: detail.into(),
            }
        }
    }
}

/// Show `cards`, replacing any notice window that is open.
pub(crate) fn present(cx: &mut gpui::App, shelf: &Rc<RefCell<NoticeShelf>>, cards: &[Notice]) {
    close_notice_windows(cx);
    if cards.is_empty() {
        return;
    }
    #[cfg_attr(
        windows,
        expect(
            unused_mut,
            reason = "only the macOS and Linux placements drain their openings"
        )
    )]
    let mut placement = match NoticePlacement::capture(cx, stack_size(cards.len())) {
        Ok(placement) => placement,
        Err(error) => {
            warn!(%error, "could not locate the notice display");
            return;
        }
    };
    // Wayland offers a fallback for compositors without layer-shell: the
    // first window that opens is the notice.
    let alternatives = placement
        .openings()
        .into_iter()
        .map(|options| vec![options]);
    let opened = crate::platform::open_first(alternatives.collect(), |options| {
        let view = NoticeView {
            cards: cards.to_vec(),
            shelf: Rc::clone(shelf),
        };
        cx.open_window(options, |_, cx| cx.new(|_| view))
    });
    let Some(handle) = opened else {
        return;
    };
    let placed = handle
        .update(cx, |_, window, cx| {
            // As for the ring: native placement resizes the window inside
            // this update, so resync GPUI's viewport and scale.
            placement.show(window).map(|()| window.bounds_changed(cx))
        })
        .and_then(std::convert::identity);
    if let Err(error) = placed {
        warn!(%error, "could not position the notice window");
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// Close every notice window.
fn close_notice_windows(cx: &mut gpui::App) {
    for handle in cx.windows() {
        if let Some(notice) = handle.downcast::<NoticeView>() {
            let _ = notice.update(cx, |_, window, _| window.remove_window());
        }
    }
}

/// The notice window's view: the stacked cards, newest at the bottom.
pub(crate) struct NoticeView {
    cards: Vec<Notice>,
    shelf: Rc<RefCell<NoticeShelf>>,
}

impl NoticeView {
    fn card(&self, notice: &Notice) -> impl IntoElement + use<> {
        let NoticeText { title, detail } = notice_text(&notice.kind);
        let id = notice.id;
        let notice_shelf = Rc::clone(&self.shelf);
        div()
            .id(("notice", id))
            .w(px(CARD_WIDTH))
            .h(px(CARD_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(16.0))
            .rounded(px(14.0))
            .bg(CARD)
            .shadow_lg()
            .cursor_pointer()
            .child(
                svg()
                    .path(LOW_BATTERY_ICON)
                    .size(px(24.0))
                    .flex_none()
                    .text_color(WARNING),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(TITLE)
                            .child(title),
                    )
                    .child(div().text_xs().text_color(DETAIL).truncate().child(detail)),
            )
            .on_click(move |_, window, cx| {
                // This window is checked out of the app while its handler
                // runs, so close it here and redraw what is left afterwards.
                window.remove_window();
                let notice_shelf = Rc::clone(&notice_shelf);
                cx.defer(move |cx| {
                    let cards = notice_shelf.borrow_mut().dismiss(id);
                    present(cx, &notice_shelf, &cards);
                });
            })
    }
}

impl Render for NoticeView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .gap(px(CARD_GAP))
            .p(px(STACK_INSET))
            .children(self.cards.iter().map(|notice| self.card(notice)))
    }
}

#[cfg(test)]
mod tests;
