//! Local package review, exact-content grants, and lifecycle controls.

use gpui_component::switch::Switch;
use openlogi_core::peripheral::{PluginCommand, PluginPackageRecord};

use super::{
    AppState, Disableable, Entity, IconName, ParentElement, SettingField, SettingGroup,
    SettingItem, SettingPage, SettingsView, SharedString, Styled, div, h_flex, v_flex,
};
use crate::{
    features::peripheral::peripheral_error,
    services::ipc::PeripheralOperation,
    ui::{
        components::{control_button, control_input},
        theme::{self, Typography as _},
    },
};

pub(super) fn plugins_page(
    view: Entity<SettingsView>,
    path: Entity<super::InputState>,
) -> SettingPage {
    SettingPage::new(tr!("peripheral.plugins"))
        .icon(IconName::HardDrive)
        .group(SettingGroup::new().item(SettingItem::new(
            tr!("peripheral.local_packages"),
            SettingField::render(move |_, _, cx| {
                let pal = theme::palette(cx);
                let Some(state) = AppState::try_read(cx) else {
                    return div();
                };
                let snapshot = state.peripheral_snapshot().clone();
                let busy = state.peripheral_busy();
                let issue = state.peripheral_issue().map(peripheral_error);
                let path_value = path.clone();
                let mut content = v_flex()
                    .gap_4()
                    .w_full()
                    .child(
                        div()
                            .text_body()
                            .text_color(pal.text_muted)
                            .child(tr!("peripheral.install_help")),
                    )
                    .child(control_input(&path))
                    .child(
                        control_button("plugin-install")
                            .label(tr!("peripheral.install"))
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                let path = path_value.read(cx).value().trim().to_string();
                                AppState::apply(cx, |state| {
                                    state.manage_peripheral(PeripheralOperation::Plugin(
                                        PluginCommand::Install { path },
                                    ))
                                });
                            }),
                    );
                if busy {
                    content = content.child(div().text_body().child(tr!("peripheral.working")));
                }
                if let Some(issue) = issue {
                    content = content.child(div().text_body().child(issue));
                }
                if snapshot.plugins.is_empty() {
                    content = content.child(div().text_body().child(tr!("peripheral.no_plugins")));
                }
                for package in &snapshot.plugins {
                    content = content.child(package_card(&view, package, busy, cx));
                }
                for diagnostic in snapshot.diagnostics {
                    content = content.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_caption().child(diagnostic.source))
                            .child(div().text_body().child(peripheral_error(&diagnostic.error)))
                            .child(div().text_caption().child(if diagnostic.retained {
                                tr!("peripheral.retained_source")
                            } else {
                                tr!("peripheral.rejected_source")
                            })),
                    );
                }
                content
            }),
        )))
}

fn package_card(
    view: &Entity<SettingsView>,
    package: &PluginPackageRecord,
    busy: bool,
    cx: &mut gpui::App,
) -> gpui::Div {
    let pal = theme::palette(cx);
    let mut card = v_flex()
        .gap_2()
        .border_t_1()
        .border_color(pal.border)
        .pt_4()
        .child(
            div()
                .text_subheading()
                .child(format!("{} · {}", package.driver, package.version)),
        )
        .child(
            div()
                .text_caption()
                .child(format!("SHA-256: {}", package.digest)),
        )
        .child(div().text_body().child(if package.active {
            tr!("peripheral.plugin_active")
        } else if package.selection.is_enabled() {
            tr!("peripheral.plugin_enabled")
        } else {
            tr!("peripheral.plugin_disabled")
        }))
        .child(div().text_subheading().child(tr!("peripheral.permissions")));
    for permission in &package.permissions {
        card = card.child(div().text_body().child(permission.clone()));
    }
    card = card.child(
        div()
            .text_body()
            .child(tr!("peripheral.select_descriptors")),
    );
    for (descriptor, details) in &package.descriptors {
        let selected = if package.selection.is_enabled() {
            details.granted
        } else {
            view.read(cx)
                .plugin_descriptors
                .get(&package.digest)
                .is_some_and(|set| set.contains(descriptor))
        };
        card = card.child(div().text_caption().child(details.matching.clone()));
        let edit = view.clone();
        let digest = package.digest.clone();
        let descriptor = descriptor.clone();
        card = card.child(
            Switch::new(SharedString::from(format!("grant-{digest}-{descriptor}")))
                .label(descriptor.to_string())
                .checked(selected)
                .disabled(busy || package.selection.is_enabled())
                .on_click(move |selected, _, cx| {
                    edit.update(cx, |view, cx| {
                        let descriptors =
                            view.plugin_descriptors.entry(digest.clone()).or_default();
                        if *selected {
                            descriptors.insert(descriptor.clone());
                        } else {
                            descriptors.remove(&descriptor);
                        }
                        cx.notify();
                    });
                }),
        );
    }
    let descriptors: std::collections::BTreeMap<_, _> = view
        .read(cx)
        .plugin_descriptors
        .get(&package.digest)
        .into_iter()
        .flatten()
        .filter_map(|id| {
            package
                .descriptors
                .get(id)
                .map(|details| (id.clone(), details.fingerprint.clone()))
        })
        .collect();
    card.child(package_actions(package, descriptors, busy))
}

fn package_actions(
    package: &PluginPackageRecord,
    descriptors: std::collections::BTreeMap<openlogi_core::peripheral::DescriptorId, String>,
    busy: bool,
) -> gpui::Div {
    let actions = [
        (
            "enable",
            tr!("peripheral.enable_package"),
            package.selection.is_enabled() || descriptors.is_empty(),
            PluginCommand::Enable {
                digest: package.digest.clone(),
                descriptors,
            },
        ),
        (
            "disable",
            tr!("peripheral.disable_package"),
            !package.selection.is_enabled(),
            PluginCommand::Disable {
                driver: package.driver.clone(),
            },
        ),
        (
            "rollback",
            tr!("peripheral.rollback"),
            !package.rollback_available,
            PluginCommand::Rollback {
                driver: package.driver.clone(),
            },
        ),
        (
            "remove",
            tr!("peripheral.remove"),
            false,
            PluginCommand::Remove {
                digest: package.digest.clone(),
            },
        ),
    ];
    h_flex()
        .flex_wrap()
        .gap_2()
        .children(actions.into_iter().map(|(id, label, disabled, command)| {
            control_button(SharedString::from(format!("{id}-{}", package.digest)))
                .label(label)
                .disabled(busy || disabled)
                .on_click(move |_, _, cx| {
                    AppState::apply(cx, |state| {
                        state.manage_peripheral(PeripheralOperation::Plugin(command.clone()))
                    });
                })
        }))
}
