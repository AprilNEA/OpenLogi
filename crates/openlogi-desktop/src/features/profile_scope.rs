//! The profile scope bar: which profile the binding panels are editing.
//!
//! One row above the model — a label, a dropdown naming the open profile, and,
//! inside a per-app one, a caption saying what that profile can express. The
//! dropdown lists the device's existing application profiles, then offers to
//! start one for an application the agent recently saw in front.
//!
//! That last part is why this is possible at all without any per-platform code:
//! the identifiers per-app profiles key on come from four incompatible
//! namespaces, and the agent is the only process holding the one its matcher
//! will compare (see [`ForegroundApps`](openlogi_ipc::ForegroundApps)). The GUI
//! picks from what the agent saw rather than enumerating installed
//! applications, which would produce plausible strings that never match.
//!
//! Not under `mouse/`: nothing here is mouse-specific, and the Actions Ring
//! tab — whose layouts are per-application too — can adopt the same bar.

use gpui::{
    AnyElement, App, BorrowAppContext as _, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, Role, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::popover::{Popover, PopoverState};
use gpui_component::{Icon, IconName, Selectable, h_flex, v_flex};

use crate::features::mouse::picker::{POPOVER_W, divider, menu_card, menu_row, scroll_list, title};
use crate::state::AppState;
use crate::ui::theme::{self, Palette, Typography as _};

/// The bar, or nothing at all when the active device has no persistent config
/// key — a transient probe cannot carry profiles, so offering to author one
/// would be a promise the next enumeration breaks.
pub fn profile_scope_bar(pal: Palette, cx: &App) -> Option<AnyElement> {
    let state = cx.try_global::<AppState>()?;
    // A transient probe carries no config key, so it can hold no profiles —
    // offering to author one would be a promise the next enumeration breaks.
    if !state.current_device_is_persistent() {
        return None;
    }
    let editing = state.editing_app().map(|app| display_name(state, app));
    let open_label = editing
        .clone()
        .unwrap_or_else(|| tr!("Default profile").to_string());

    Some(
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("Profile")),
                    )
                    .child(scope_dropdown(open_label, pal)),
            )
            .when_some(editing, |bar, app| {
                bar.child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!(
                            "Applies only in %{app}. One action per button — gestures stay in the default profile.",
                            app => app
                        )),
                )
            })
            .into_any_element(),
    )
}

/// The application's human name as the agent last reported it, falling back to
/// the identifier — which is what a hand-written `config.toml` entry, or a
/// profile carried over from another machine, shows until that app is seen in
/// front again.
fn display_name(state: &AppState, app: &str) -> String {
    state.recent_app_name(app).unwrap_or(app).to_string()
}

fn scope_dropdown(open_label: String, pal: Palette) -> impl IntoElement {
    Popover::new("profile-scope")
        // The menu draws its own `menu_card`, matching every other list in the
        // binding flow.
        .appearance(false)
        .mouse_button(MouseButton::Left)
        .trigger(ScopeTrigger {
            label: open_label,
            open: false,
            pal,
        })
        .content(move |_state, _window, cx| scope_menu(cx))
}

/// The dropdown's trigger: the open profile's name and a chevron.
///
/// Its own type rather than a bare `div` because [`Popover`] hands the trigger
/// its open state through [`Selectable`], which is how the control stays
/// visibly pressed while its menu is up.
#[derive(IntoElement)]
struct ScopeTrigger {
    label: String,
    open: bool,
    pal: Palette,
}

impl Selectable for ScopeTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for ScopeTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let pal = self.pal;
        h_flex()
            .id("profile-scope-trigger")
            .role(Role::Button)
            .aria_label(tr!("Profile"))
            .aria_expanded(self.open)
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1()
            .rounded(pal.control_radius)
            .border_1()
            .border_color(pal.border)
            .bg(if self.open {
                pal.surface_hover
            } else {
                pal.surface
            })
            .cursor_pointer()
            .hover(move |s| s.bg(pal.surface_hover))
            .text_body()
            .text_color(pal.text_primary)
            .child(self.label)
            .child(
                Icon::new(IconName::ChevronDown)
                    .size_3()
                    .text_color(pal.text_muted),
            )
    }
}

fn scope_menu(cx: &mut gpui::Context<PopoverState>) -> AnyElement {
    let pal = theme::palette(cx);
    let popover = cx.entity().downgrade();
    // Everything the menu shows, read out of the global in one borrow.
    let Some((editing, profiles, candidates)) = cx.try_global::<AppState>().map(|state| {
        let profiles: Vec<(String, String, usize)> = state
            .app_profiles()
            .map(|(app, count)| (app.to_string(), display_name(state, app), count))
            .collect();
        // Only applications without a profile yet: the ones that have one are
        // already rows above, and offering them twice would mean two things.
        let candidates: Vec<(String, String)> = state
            .recent_apps()
            .filter(|(app, _)| !profiles.iter().any(|(existing, _, _)| existing == app))
            .map(|(app, name)| (app.to_string(), name.to_string()))
            .collect();
        (
            state.editing_app().map(str::to_string),
            profiles,
            candidates,
        )
    }) else {
        return div().into_any_element();
    };

    let mut rows: Vec<AnyElement> = Vec::with_capacity(profiles.len() + 1);
    rows.push(scope_row(
        "profile-default",
        None,
        tr!("Default profile").to_string(),
        None,
        editing.is_none(),
        &popover,
        pal,
    ));
    for (index, (app, name, count)) in profiles.into_iter().enumerate() {
        let selected = editing.as_deref() == Some(app.as_str());
        rows.push(scope_row(
            ("profile-app", index),
            Some(app),
            name,
            Some(count),
            selected,
            &popover,
            pal,
        ));
    }

    menu_card(pal)
        .min_w(px(POPOVER_W))
        .child(title(tr!("Profile"), pal))
        .child(divider(pal))
        .child(scroll_list("profile-scope-scroll", rows))
        .child(divider(pal))
        .child(add_app_section(candidates, &popover, pal))
        .when(editing.is_some(), |card| {
            card.child(divider(pal)).child(remove_row(&popover, pal))
        })
        .into_any_element()
}

/// One profile row: `app` is its identifier, or `None` for the device's global
/// profile. Selecting it only switches what the panels edit — nothing is
/// written, so a profile the user opens and leaves alone stays absent from
/// `config.toml`.
fn scope_row(
    id: impl Into<gpui::ElementId>,
    app: Option<String>,
    name: String,
    overrides: Option<usize>,
    selected: bool,
    popover: &gpui::WeakEntity<PopoverState>,
    pal: Palette,
) -> AnyElement {
    let popover = popover.clone();
    menu_row(id, pal, selected)
        .child(div().child(name))
        .when_some(overrides, |row, count| {
            row.child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(count.to_string()),
            )
        })
        .when(selected, |row| {
            row.child(Icon::new(IconName::Check).size_3())
        })
        .on_click(move |_event, window, cx| {
            let app = app.clone();
            cx.update_global::<AppState, _>(|state, _| state.set_editing_app(app));
            cx.refresh_windows();
            if let Some(p) = popover.upgrade() {
                p.update(cx, |s, cx| s.dismiss(window, cx));
            }
        })
        .into_any_element()
}

fn add_app_section(
    candidates: Vec<(String, String)>,
    popover: &gpui::WeakEntity<PopoverState>,
    pal: Palette,
) -> AnyElement {
    if candidates.is_empty() {
        return div()
            .px_2()
            .py_1p5()
            .text_caption()
            .text_color(pal.text_muted)
            .child(tr!("No recent applications"))
            .into_any_element();
    }
    let rows: Vec<AnyElement> = candidates
        .into_iter()
        .enumerate()
        .map(|(index, (app, name))| {
            let popover = popover.clone();
            menu_row(("profile-add", index), pal, false)
                .child(div().child(name))
                .on_click(move |_event, window, cx| {
                    let app = app.clone();
                    cx.update_global::<AppState, _>(|state, _| state.set_editing_app(Some(app)));
                    cx.refresh_windows();
                    if let Some(p) = popover.upgrade() {
                        p.update(cx, |s, cx| s.dismiss(window, cx));
                    }
                })
                .into_any_element()
        })
        .collect();
    v_flex()
        .child(
            div()
                .px_2()
                .pt_1()
                .pb_0p5()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("Add app…")),
        )
        .children(rows)
        .into_any_element()
}

/// Delete the open profile outright, falling back to the global one.
fn remove_row(popover: &gpui::WeakEntity<PopoverState>, pal: Palette) -> AnyElement {
    let popover = popover.clone();
    menu_row("profile-remove", pal, false)
        .child(div().child(tr!("Remove this app profile")))
        .on_click(move |_event, window, cx| {
            cx.update_global::<AppState, _>(|state, _| state.remove_editing_app_profile());
            cx.refresh_windows();
            if let Some(p) = popover.upgrade() {
                p.update(cx, |s, cx| s.dismiss(window, cx));
            }
        })
        .into_any_element()
}
