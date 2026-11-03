//! The small muted label that heads a group inside a panel or popover.
//!
//! The action picker, the Actions Ring editor, the function-row list, and the
//! camera, pointer and lighting panels all want the same thing: one line naming
//! the group below it, a clear step down from the card title above it. This
//! module is the single rendering of that line. It had drifted into four
//! dialects across those screens — two of them uppercasing already *translated*
//! text (not a transform a UI layer can safely apply: casing is per-language,
//! and GPUI has no `text-transform` to defer it to), and one of them set at the
//! same size as the card title it sat under, which left no hierarchy at all.
//!
//! The helper fixes the *type* — one caption step, one muted colour — and
//! returns a [`Div`] so each caller still places it: a popover list insets its
//! heading to match its rows, a panel lets it sit flush.

use gpui::{Div, ParentElement, SharedString, Styled, div};

use crate::ui::theme::{Palette, Typography as _};

/// A group heading inside a panel or popover. The text is pre-localized by the
/// caller and rendered verbatim, in the case the catalog supplies.
pub fn section_label(label: impl Into<SharedString>, pal: Palette) -> Div {
    div()
        .text_caption()
        .text_color(pal.text_muted)
        .child(label.into())
}
