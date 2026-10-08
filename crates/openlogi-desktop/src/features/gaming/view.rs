//! Profile-scoped workspace: selection, editor, and a stable transaction footer.
use super::*;
use gpui::{IntoElement, ParentElement, Render, Styled, div, prelude::FluentBuilder as _, rems};
use gpui_component::{
    Disableable as _, Icon, IconName, Selectable as _,
    button::ButtonVariants as _,
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    v_flex,
};

impl GamingPanel {
    fn blocked(&self) -> bool {
        self.busy || self.needs_refresh || self.profile().is_none_or(|p| !p.checksum_valid)
    }

    fn profile_picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let selected = self.profile().map_or_else(
            || tr!("gaming.title"),
            |p| {
                if p.name.is_empty() {
                    tr!("gaming.profile", number = p.sector)
                } else {
                    p.name.clone().into()
                }
            },
        );
        let profiles = self
            .snapshot
            .as_ref()
            .map(|s| s.profiles.clone())
            .unwrap_or_default();
        let owner = cx.entity().downgrade();
        control_button("gaming-profile-picker")
            .label(selected)
            .icon(IconName::ChevronDown)
            .disabled(self.busy || self.dirty(cx))
            .dropdown_menu(move |mut menu, _, _| {
                for p in &profiles {
                    let sector = p.sector;
                    let label = if p.name.is_empty() {
                        tr!("gaming.profile", number = sector).to_string()
                    } else {
                        format!("{} · {}", sector, p.name)
                    };
                    let owner = owner.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |this, cx| this.select(sector, window, cx));
                    }));
                }
                menu
            })
    }

    fn toolbar(&self, loaded: bool, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
        let mode = self.snapshot.as_ref().map(|s| s.mode);
        h_flex()
            .flex_shrink_0()
            .w_full()
            .gap_3()
            .px_5()
            .py_3()
            .flex_wrap()
            .border_b_1()
            .border_color(pal.border)
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("gaming.editing_profile")),
                    )
                    .child(self.profile_picker(cx)),
            )
            .child(div().flex_1())
            .when(loaded, |row| {
                row.child(div().text_caption().text_color(pal.text_muted).child(
                    if mode == Some(1) {
                        tr!("gaming.onboard")
                    } else {
                        tr!("gaming.host")
                    },
                ))
            })
            .child(
                control_button("gaming-read")
                    .icon(IconName::ArrowDown)
                    .label(tr!("gaming.read"))
                    .disabled(
                        self.busy
                            || (loaded && self.dirty(cx) && !self.needs_refresh)
                            || Self::current_route(cx).is_none(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.request(GamingCommand::Read, window, cx);
                    })),
            )
    }

    fn tabs(&self, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
        h_flex()
            .flex_shrink_0()
            .gap_2()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(pal.border)
            .children(
                [
                    (Page::Assignments, tr!("gaming.assignments")),
                    (Page::Sensitivity, tr!("gaming.sensitivity")),
                    (Page::Profiles, tr!("gaming.manage_profiles")),
                ]
                .into_iter()
                .enumerate()
                .map(|(ix, (page, label))| {
                    control_button(("gaming-page", ix))
                        .label(label)
                        .selected(self.page == page)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.page = page;
                            cx.notify();
                        }))
                }),
            )
    }

    fn button_list(&self, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
        let count = self.profile().map_or(0, |p| p.buttons.len());
        v_flex()
            .w(rems(17.))
            .flex_shrink_0()
            .gap_2()
            .child(
                h_flex().gap_2().children(
                    [
                        (Layer::Normal, tr!("gaming.normal")),
                        (Layer::Shifted, "G-Shift".into()),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (layer, label))| {
                        control_button(("gaming-layer", ix))
                            .label(label)
                            .selected(self.layer == layer)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.layer = layer;
                                cx.notify();
                            }))
                    }),
                ),
            )
            .children((0..count).map(|ix| {
                let index = u8::try_from(ix).unwrap_or(0);
                let changed = self
                    .draft
                    .buttons
                    .iter()
                    .any(|b| b.index == index && b.shifted == (self.layer == Layer::Shifted));
                control_button(("gaming-button", ix))
                    .w_full()
                    .justify_start()
                    .label(format!(
                        "{:02}   {}{}",
                        ix + 1,
                        self.binding_label(ix),
                        if changed { " *" } else { "" }
                    ))
                    .selected(self.selected_button == index)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_button = index;
                        cx.notify();
                    }))
            }))
            .child(
                div()
                    .mt_2()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("gaming.button_help")),
            )
    }

    fn action_group(
        &self,
        title: SharedString,
        group: usize,
        actions: Vec<GamingAction>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let pal = theme::palette(cx);
        v_flex()
            .gap_2()
            .child(div().text_caption().text_color(pal.text_muted).child(title))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(actions.into_iter().enumerate().map(|(ix, action)| {
                        control_button(("gaming-action", group * 100 + ix))
                            .label(action_name(&action))
                            .disabled(self.blocked())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.assign(Some(action.clone()), cx);
                            }))
                    })),
            )
    }

    fn shortcut_editor(&self, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_subheading()
                    .child(tr!("gaming.keyboard_shortcut")),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("gaming.shortcut_help")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(control_input(&self.shortcut).disabled(self.blocked())),
                    )
                    .child(
                        control_button("gaming-set-shortcut")
                            .label(tr!("gaming.assign"))
                            .disabled(self.blocked())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(action) =
                                    keyboard::parse(this.shortcut.read(cx).value().as_ref())
                                {
                                    this.assign(Some(action), cx);
                                    this.error = false;
                                    this.message = tr!("gaming.unsaved").into();
                                } else {
                                    this.error = true;
                                    this.message = tr!("gaming.invalid_shortcut").into();
                                }
                                cx.notify();
                            })),
                    ),
            )
    }

    fn assignments(&self, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
        h_flex()
            .items_start()
            .gap_6()
            .flex_wrap()
            .child(self.button_list(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(rems(18.))
                    .gap_5()
                    .p_5()
                    .bg(pal.panel)
                    .rounded(pal.card_radius)
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(tr!("gaming.button", number = self.selected_button + 1)),
                            )
                            .child(
                                div()
                                    .text_heading()
                                    .child(self.binding_label(usize::from(self.selected_button))),
                            )
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(tr!("gaming.assign_help")),
                            ),
                    )
                    .child(self.shortcut_editor(cx))
                    .child(self.action_group(
                        tr!("gaming.mouse_actions"),
                        0,
                        (1..=5).map(GamingAction::Mouse).collect(),
                        cx,
                    ))
                    .child(self.action_group(
                        tr!("gaming.device_actions"),
                        1,
                        (1..=12).map(GamingAction::Special).collect(),
                        cx,
                    ))
                    .child(
                        self.action_group(
                            tr!("gaming.media_actions"),
                            2,
                            [0xe9, 0xea, 0xe2, 0xcd]
                                .into_iter()
                                .map(GamingAction::Consumer)
                                .collect(),
                            cx,
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                control_button("gaming-reset-binding")
                                    .ghost()
                                    .label(tr!("gaming.keep_original"))
                                    .disabled(self.blocked())
                                    .on_click(cx.listener(|this, _, _, cx| this.assign(None, cx))),
                            )
                            .child(
                                control_button("gaming-disable-binding")
                                    .ghost()
                                    .label(tr!("gaming.unassigned"))
                                    .disabled(self.blocked())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.assign(Some(GamingAction::Disabled), cx);
                                    })),
                            ),
                    ),
            )
    }

    fn sensitivity(&self, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
        v_flex()
            .gap_6()
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_heading().child(tr!("gaming.sensitivity")))
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("gaming.dpi_help")),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .children(self.dpi.iter().enumerate().map(|(ix, input)| {
                        v_flex()
                            .flex_1()
                            .min_w(rems(8.))
                            .gap_3()
                            .p_4()
                            .bg(pal.panel)
                            .rounded(pal.card_radius)
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(tr!("gaming.dpi_stage", number = ix + 1)),
                            )
                            .child(control_input(input).disabled(self.blocked()))
                            .child(
                                control_button(("gaming-default", ix))
                                    .label(tr!("gaming.default"))
                                    .selected(usize::from(self.draft.default_dpi_slot) == ix)
                                    .disabled(self.blocked())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.draft.default_dpi_slot = u8::try_from(ix).unwrap_or(0);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                control_button(("gaming-dpi-shift", ix))
                                    .label(tr!("gaming.dpi_shift"))
                                    .selected(usize::from(self.draft.shift_dpi_slot) == ix)
                                    .disabled(self.blocked())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.draft.shift_dpi_slot = u8::try_from(ix).unwrap_or(0);
                                        cx.notify();
                                    })),
                            )
                    })),
            )
            .child(
                v_flex()
                    .gap_3()
                    .p_5()
                    .bg(pal.panel)
                    .rounded(pal.card_radius)
                    .child(div().text_subheading().child(tr!("gaming.report_rate")))
                    .child(h_flex().gap_2().flex_wrap().children(
                        [125u16, 250, 500, 1000].into_iter().map(|hz| {
                            control_button(("gaming-rate", usize::from(hz)))
                                .label(format!("{hz} Hz"))
                                .selected(self.draft.report_rate_hz == hz)
                                .disabled(self.blocked())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.draft.report_rate_hz = hz;
                                    cx.notify();
                                }))
                        }),
                    )),
            )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "three related profile-management sections share their validated snapshot"
    )]
    fn profiles(&self, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
        let mode = self.snapshot.as_ref().map_or(0, |s| s.mode);
        let profile = self.profile();
        let sector = self.draft.sector;
        let active = self
            .snapshot
            .as_ref()
            .is_some_and(|s| s.active_profile == sector);
        let status = if profile.is_some_and(|p| !p.checksum_valid) {
            tr!("gaming.bad_crc")
        } else if profile.is_some_and(|p| !p.enabled) {
            tr!("gaming.disabled_profile")
        } else if active {
            tr!("gaming.active")
        } else {
            tr!("gaming.available")
        };
        v_flex()
            .gap_5()
            .max_w(theme::ContentWidth::Medium.rems())
            .child(div().text_heading().child(tr!("gaming.manage_profiles")))
            .child(
                v_flex()
                    .gap_3()
                    .p_5()
                    .bg(pal.panel)
                    .rounded(pal.card_radius)
                    .child(div().text_subheading().child(tr!("gaming.name")))
                    .child(control_input(&self.name).disabled(self.blocked()))
                    .child(
                        h_flex()
                            .gap_3()
                            .flex_wrap()
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(status),
                            )
                            .child(
                                control_button("gaming-activate")
                                    .label(tr!("gaming.activate"))
                                    .disabled(
                                        self.blocked()
                                            || self.dirty(cx)
                                            || mode != 1
                                            || profile.is_none_or(|p| !p.enabled),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.request(GamingCommand::Select(sector), window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_3()
                    .p_5()
                    .bg(pal.panel)
                    .rounded(pal.card_radius)
                    .child(div().text_subheading().child(if mode == 1 {
                        tr!("gaming.onboard")
                    } else {
                        tr!("gaming.host")
                    }))
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("gaming.mode_help")),
                    )
                    .child(
                        control_button("gaming-mode")
                            .label(if mode == 1 {
                                tr!("gaming.use_host")
                            } else {
                                tr!("gaming.use_onboard")
                            })
                            .disabled(self.busy || self.dirty(cx) || self.needs_refresh)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request(
                                    GamingCommand::SetMode(if mode == 1 { 2 } else { 1 }),
                                    window,
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_3()
                    .p_5()
                    .bg(pal.panel)
                    .rounded(pal.card_radius)
                    .child(div().text_subheading().child(tr!("gaming.backups")))
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("gaming.backup_help")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                control_button("gaming-export")
                                    .label(tr!("gaming.export"))
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.request(GamingCommand::Export, window, cx);
                                    })),
                            )
                            .child(
                                control_button("gaming-prepare")
                                    .label(tr!("gaming.prepare"))
                                    .disabled(self.blocked())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit(false, window, cx);
                                    })),
                            ),
                    ),
            )
    }

    fn footer(&self, loaded: bool, cx: &mut Context<Self>) -> gpui::Div {
        let pal = theme::palette(cx);
        let dirty = loaded && self.dirty(cx);
        let message = if self.message.is_empty() {
            tr!("gaming.start").to_string()
        } else {
            self.message.clone()
        };
        v_flex()
            .flex_shrink_0()
            .gap_2()
            .px_5()
            .py_3()
            .border_t_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        div()
                            .flex_1()
                            .min_w(rems(12.))
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(if dirty {
                                tr!("gaming.unsaved")
                            } else {
                                tr!("gaming.no_changes")
                            }),
                    )
                    .child(
                        control_button("gaming-discard")
                            .ghost()
                            .label(tr!("gaming.discard"))
                            .disabled(self.busy || !dirty)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select(this.draft.sector, window, cx);
                            })),
                    )
                    .child(
                        control_button("gaming-save")
                            .primary()
                            .label(tr!("gaming.apply"))
                            .disabled(
                                !loaded
                                    || self.blocked()
                                    || !dirty
                                    || self.snapshot.as_ref().is_none_or(|s| s.mode != 1),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.submit(true, window, cx)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .when(self.error, |row| {
                        row.child(Icon::new(IconName::Info).size_4())
                    })
                    .child(
                        div()
                            .min_w_0()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(message),
                    ),
            )
    }
}

impl Render for GamingPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let current = Self::current_route(cx);
        let loaded = current.is_some() && self.route == current && self.snapshot.is_some();
        let body = if loaded {
            match self.page {
                Page::Assignments => self.assignments(cx),
                Page::Sensitivity => self.sensitivity(cx),
                Page::Profiles => self.profiles(cx),
            }
        } else {
            v_flex()
                .gap_3()
                .py_10()
                .items_center()
                .child(
                    Icon::new(IconName::Settings)
                        .size_12()
                        .text_color(pal.text_muted),
                )
                .child(div().text_heading().child(tr!("gaming.title")))
                .child(
                    div()
                        .text_body()
                        .text_color(pal.text_muted)
                        .child(tr!("gaming.start")),
                )
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.toolbar(loaded, cx))
            .when(loaded, |page| page.child(self.tabs(cx)))
            .child(
                div().flex_1().min_h_0().overflow_y_scrollbar().p_5().child(
                    div()
                        .w_full()
                        .max_w(theme::ContentWidth::DoubleExtraLarge.rems())
                        .child(body),
                ),
            )
            .child(self.footer(loaded, cx))
    }
}
