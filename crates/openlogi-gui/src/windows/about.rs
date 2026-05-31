//! The About window — a small standalone OS window (menu / footer link)
//! showing the app logo, wordmark, version, a one-line description, outbound
//! links, and a manual "Check for Updates" control backed by [`gpui_updater`].
//!
//! The logo is the embedded `openlogi.png` served by [`crate::app_assets`], so
//! `img()` resolves it the same inside a packaged `.app` as in a dev build.

use gpui::{
    App, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement as _, Render,
    Size, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, img, px,
};
use gpui_component::{IconName, button::Button, h_flex, v_flex};
use gpui_updater::{UpdateStatus, Updater};

use crate::theme;
use crate::windows::{self, AuxWindow};

const REPO_URL: &str = "https://github.com/AprilNEA/OpenLogi";
const RELEASES_URL: &str = "https://github.com/AprilNEA/OpenLogi/releases/latest";
/// Release page for this exact build, opened by clicking the version label.
const RELEASE_TAG_URL: &str = concat!(
    "https://github.com/AprilNEA/OpenLogi/releases/tag/v",
    env!("CARGO_PKG_VERSION")
);

/// Standalone About window root view.
pub struct AboutView {
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
    updater: Entity<Updater>,
    #[allow(dead_code, reason = "held to keep the updater observation alive")]
    updater_obs: Subscription,
}

impl AboutView {
    fn new(cx: &mut Context<Self>) -> Self {
        // Reuse the app-wide shared updater installed at launch, so a launch-time
        // check result is already visible here. Fall back to a fresh one if it
        // somehow wasn't installed.
        let updater = match crate::platform::updater::shared(cx) {
            Some(updater) => updater,
            None => crate::platform::updater::new_entity(cx),
        };
        let updater_obs = cx.observe(&updater, |_, _, cx| cx.notify());
        Self {
            appearance_obs: None,
            updater,
            updater_obs,
        }
    }

    /// The "Check for Updates" control plus a one-line status message and a
    /// contextual action (install when available, restart when staged).
    fn update_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let status = self.updater.read(cx).status().clone();
        let updater = self.updater.clone();

        let action = match &status {
            UpdateStatus::Available(_) => {
                let u = updater.clone();
                Some(
                    Button::new("update-install")
                        .outline()
                        .label("Download & Install")
                        .on_click(move |_, _, cx| {
                            u.update(cx, Updater::download_and_install);
                        }),
                )
            }
            UpdateStatus::Staged(_) => {
                let u = updater.clone();
                Some(
                    Button::new("update-restart")
                        .outline()
                        .label("Restart to Update")
                        .on_click(move |_, _, cx| {
                            u.update(cx, |u, cx| u.restart(cx));
                        }),
                )
            }
            _ => None,
        };

        let message = match &status {
            UpdateStatus::Idle => None,
            UpdateStatus::Checking => Some("Checking for updates…".to_string()),
            UpdateStatus::UpToDate => Some("You're on the latest version.".to_string()),
            UpdateStatus::Available(v) => Some(format!("Version {v} is available.")),
            UpdateStatus::Downloading { downloaded, total } => Some(match total {
                Some(t) if *t > 0 => format!("Downloading… {}%", *downloaded * 100 / *t),
                _ => format!("Downloading… {} MB", *downloaded / 1_048_576),
            }),
            UpdateStatus::Installing => Some("Installing…".to_string()),
            UpdateStatus::Staged(v) => Some(format!("Version {v} is ready.")),
            UpdateStatus::Errored(e) => Some(format!("Update failed: {e}")),
        };

        let check = {
            let u = updater.clone();
            Button::new("update-check")
                .outline()
                .label("Check for Updates")
                .on_click(move |_, _, cx| {
                    u.update(cx, Updater::check);
                })
        };

        v_flex()
            .gap_2()
            .items_center()
            .child(h_flex().gap_3().child(check).children(action))
            .children(message.map(|text| {
                div()
                    .max_w(px(280.))
                    .text_xs()
                    .text_center()
                    .text_color(pal.text_muted)
                    .child(text)
            }))
    }
}

impl AuxWindow for AboutView {
    fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }
}

/// Open the About window, or focus it if it's already open.
pub fn open(cx: &mut App) {
    windows::open_or_focus(
        |reg| &mut reg.about,
        "About OpenLogi",
        Size::new(px(360.), px(460.)),
        AboutView::new,
        cx,
    );
}

impl Render for AboutView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);

        v_flex()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .items_center()
            .justify_center()
            .gap_3()
            .p_8()
            .child(img(crate::app_assets::LOGO).w(px(72.)).h(px(72.)))
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .child("OpenLogi"),
            )
            .child(
                div()
                    .id("about-version")
                    .text_sm()
                    .text_color(pal.text_muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(pal.text_primary))
                    .child(concat!("v", env!("CARGO_PKG_VERSION")))
                    .on_click(|_, _, cx| cx.open_url(RELEASE_TAG_URL)),
            )
            .child(
                div()
                    .max_w(px(280.))
                    .text_sm()
                    .text_center()
                    .text_color(pal.text_muted)
                    .child(tr!(
                        "Open-source Logitech mouse configuration — DPI, SmartShift, button \
                         bindings, and gestures."
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .pt_2()
                    .child(
                        Button::new("about-repo")
                            .outline()
                            .icon(IconName::Github)
                            .label("GitHub")
                            .on_click(|_, _, cx| cx.open_url(REPO_URL)),
                    )
                    .child(
                        Button::new("about-releases")
                            .outline()
                            .icon(IconName::ExternalLink)
                            .label("Releases")
                            .on_click(|_, _, cx| cx.open_url(RELEASES_URL)),
                    ),
            )
            .child(self.update_section(cx))
            .child(
                div()
                    .text_xs()
                    .text_color(pal.text_muted)
                    .child("Licensed under MIT OR Apache-2.0"),
            )
    }
}
