//! Interface-language picker, shared by the Appearance page and the view.

use crate::ui::components::control_select;

use super::{
    AppContext as _, AppState, Context, Entity, IndexPath, IntoElement, ParentElement, SelectItem,
    SelectState, SharedString, Styled, Window, div, px,
};

/// Initialize the picker from the saved language preference.
pub(super) fn select_state<V: 'static>(
    window: &mut Window,
    cx: &mut Context<V>,
) -> Entity<SelectState<Vec<LanguageOption>>> {
    let current = AppState::try_read(cx).and_then(|state| state.app_settings().language.clone());
    let options = language_options();
    let selected = selected_language_index(current.as_deref(), &options);
    cx.new(|cx| SelectState::new(options, Some(selected), window, cx))
}

#[derive(Clone)]
pub(super) struct LanguageOption {
    label: &'static str,
    value: &'static str,
    localize_label: bool,
}

impl SelectItem for LanguageOption {
    type Value = &'static str;

    fn title(&self) -> SharedString {
        if self.localize_label {
            SharedString::from(rust_i18n::t!("appearance.follow_system").into_owned())
        } else {
            SharedString::from(self.label)
        }
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

pub(super) fn language_options() -> Vec<LanguageOption> {
    let mut options = vec![LanguageOption {
        label: "Follow system",
        value: "",
        localize_label: true,
    }];
    options.extend(
        openlogi_core::locale::SUPPORTED
            .iter()
            .map(|(code, name)| LanguageOption {
                label: name,
                value: code,
                localize_label: false,
            }),
    );
    options
}

pub(super) fn selected_language_index(
    current: Option<&str>,
    options: &[LanguageOption],
) -> IndexPath {
    let value = current.unwrap_or_default();
    let row = options
        .iter()
        .position(|option| option.value == value)
        .unwrap_or_default();
    IndexPath::default().row(row)
}

/// The language picker field. "Follow system" clears the stored preference
/// (`None`); explicit locale entries come from [`openlogi_core::locale::SUPPORTED`].
#[expect(
    clippy::needless_pass_by_value,
    reason = "built inside an `Fn` render closure, so a `&Entity` parameter would make \
              the returned element borrow a captured variable; `Entity` is a cheap handle"
)]
pub(super) fn language_select_field(
    language_select: Entity<SelectState<Vec<LanguageOption>>>,
) -> impl IntoElement {
    // The Select's root is `size_full`, so pin it to a fixed-size box instead
    // of letting it consume the whole Settings item row.
    div().flex_shrink_0().w(px(220.)).h_6().child(
        control_select(&language_select)
            .w(px(220.))
            .menu_width(px(220.)),
    )
}
