//! Menu shortcuts pressed as the frontmost app's menu item.
//!
//! Since macOS 12, AppKit remaps a menu shortcut the current keyboard layout
//! cannot reach: Finder's Back, ⌘[, is ⌘{ on Latin American, ⌘Ñ on Spanish,
//! ⌘Ö on German, and ⌘^ on French (#1479). Neither the shortcut's character
//! nor its US key reaches the item there. The remapping is AppKit's own and
//! unpublished, so the agent keeps AppKit's answer at hand: probe items,
//! hidden in its main menu, that AppKit remaps like any app's. When such a
//! shortcut is pressed, the probe says what it looks like under the current
//! layout, and the frontmost app's menu item showing it is pressed through
//! Accessibility, which no layout, dead key, or remapping can misroute. An app
//! that opted out of the remapping shows the shortcut as written, so that is
//! looked for as well.

use std::cell::RefCell;

use objc2::MainThreadMarker;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSWorkspace};
use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_foundation::NSString;
use openlogi_core::binding::KeyCombo;

use super::ax::{attr_bool, attr_i64, attr_string, children, copy_attr};
use super::main_thread::on_main;

/// Menu bar → menu bar item → menu → item → submenu → item.
const MENU_DEPTH: u8 = 6;

/// `AXMenuItemCmdModifiers` bits (`kAXMenuItemModifier*`); Command is implied
/// unless `NO_COMMAND` is set.
const AX_SHIFT: i64 = 1 << 0;
const AX_OPTION: i64 = 1 << 1;
const AX_CONTROL: i64 = 1 << 2;
const AX_NO_COMMAND: i64 = 1 << 3;

thread_local! {
    /// The probe items, by the chord each carries. Only the main thread
    /// touches AppKit's menus.
    static PROBES: RefCell<Vec<(KeyCombo, Retained<NSMenuItem>)>> = const { RefCell::new(Vec::new()) };
}

/// A menu item's shortcut: its key's character, lowercased because a menu
/// shows letters in capitals whether or not Shift is part of the shortcut,
/// and its modifiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MenuShortcut {
    pub(super) key: String,
    pub(super) modifiers: Modifiers,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per modifier key, as each representation spells them"
)]
pub(super) struct Modifiers {
    pub(super) command: bool,
    pub(super) shift: bool,
    pub(super) option: bool,
    pub(super) control: bool,
}

impl MenuShortcut {
    /// `combo` as a menu would show it without remapping; `None` for a key
    /// that types no character.
    pub(super) fn written(combo: &KeyCombo) -> Option<Self> {
        Some(Self {
            key: combo.key().ascii_char()?.to_string(),
            modifiers: Modifiers {
                // Super is Command on macOS.
                command: combo.has_command() || combo.has_super(),
                shift: combo.has_shift(),
                option: combo.has_option(),
                control: combo.has_control(),
            },
        })
    }

    fn of_item(item: &NSMenuItem) -> Self {
        let flags = item.keyEquivalentModifierMask();
        Self {
            key: item.keyEquivalent().to_string().to_lowercase(),
            modifiers: Modifiers {
                command: flags.contains(NSEventModifierFlags::Command),
                shift: flags.contains(NSEventModifierFlags::Shift),
                option: flags.contains(NSEventModifierFlags::Option),
                control: flags.contains(NSEventModifierFlags::Control),
            },
        }
    }

    /// The shortcut an Accessibility menu item reports, from its
    /// `AXMenuItemCmdChar` and `AXMenuItemCmdModifiers`.
    fn of_ax(key: &str, ax_modifiers: i64) -> Self {
        Self {
            key: key.to_lowercase(),
            modifiers: Modifiers {
                command: ax_modifiers & AX_NO_COMMAND == 0,
                shift: ax_modifiers & AX_SHIFT != 0,
                option: ax_modifiers & AX_OPTION != 0,
                control: ax_modifiers & AX_CONTROL != 0,
            },
        }
    }
}

impl Modifiers {
    fn event_flags(self) -> NSEventModifierFlags {
        let mut flags = NSEventModifierFlags::empty();
        for (held, flag) in [
            (self.command, NSEventModifierFlags::Command),
            (self.shift, NSEventModifierFlags::Shift),
            (self.option, NSEventModifierFlags::Option),
            (self.control, NSEventModifierFlags::Control),
        ] {
            if held {
                flags |= flag;
            }
        }
        flags
    }
}

/// Put a hidden probe for each of `combos` in the main menu, where AppKit
/// keeps its remapped form current as the layout changes. AppKit remaps an
/// item only once the run loop has turned with it in the main menu, so this
/// runs before the loop starts rather than on the first press.
pub(super) fn prepare(mtm: MainThreadMarker, combos: impl IntoIterator<Item = KeyCombo>) {
    let app = NSApplication::sharedApplication(mtm);
    let main = app.mainMenu().unwrap_or_else(|| {
        let main = NSMenu::new(mtm);
        app.setMainMenu(Some(&main));
        main
    });
    let probes = NSMenu::new(mtm);
    PROBES.with_borrow_mut(|installed| {
        for combo in combos {
            let Some(shortcut) = MenuShortcut::written(&combo) else {
                continue;
            };
            let item = NSMenuItem::new(mtm);
            item.setKeyEquivalent(&NSString::from_str(&shortcut.key));
            item.setKeyEquivalentModifierMask(shortcut.modifiers.event_flags());
            probes.addItem(&item);
            installed.push((combo, item));
        }
    });
    let holder = NSMenuItem::new(mtm);
    holder.setHidden(true);
    holder.setSubmenu(Some(&probes));
    main.addItem(&holder);
}

/// How AppKit shows `combo` under the current layout, from its probe; `None`
/// without one (outside the agent) or when the main thread does not answer.
pub(super) fn localized(combo: &KeyCombo) -> Option<MenuShortcut> {
    let combo = combo.clone();
    on_main("menu shortcut", move |_| {
        PROBES.with_borrow(|probes| {
            probes
                .iter()
                .find(|(probe, _)| *probe == combo)
                .map(|(_, item)| MenuShortcut::of_item(item))
        })
    })
    .flatten()
}

/// How pressing a shortcut's menu item went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuPress {
    Pressed,
    /// The frontmost app shows no enabled item for the shortcut, or would not
    /// take the press; its keys may still reach that app.
    NotPressed,
    /// Another app came to the front during the search. The shortcut was
    /// meant for the one that left, so nothing else may be sent.
    TargetChanged,
}

/// Press the frontmost app's enabled menu item that shows `combo` as
/// `localized` or, for an app that opted out of remapping, as written.
pub(super) fn press_menu_item(combo: &KeyCombo, localized: Option<&MenuShortcut>) -> MenuPress {
    let written = MenuShortcut::written(combo);
    let mut wanted: Vec<&MenuShortcut> = localized.into_iter().collect();
    if let Some(written) = written
        .as_ref()
        .filter(|written| Some(*written) != localized)
    {
        wanted.push(written);
    }
    autoreleasepool(|_| {
        let Some(pid) = frontmost_pid() else {
            return MenuPress::NotPressed;
        };
        // SAFETY: `pid` names a running process; AX reports a stale one as an
        // error on every call.
        let app = unsafe { AXUIElement::new_application(pid) };
        let Some(menu_bar) = copy_attr(&app, &CFString::from_static_str("AXMenuBar"))
            .and_then(|bar| bar.downcast::<AXUIElement>().ok())
        else {
            return MenuPress::NotPressed;
        };
        let attrs = MenuAttrs::new();
        let item = wanted
            .iter()
            .find_map(|shortcut| find_item(&menu_bar, shortcut, &attrs, MENU_DEPTH));
        // The search can take a while; a switch of apps meanwhile cancels.
        if frontmost_pid() != Some(pid) {
            return MenuPress::TargetChanged;
        }
        let Some(item) = item else {
            return MenuPress::NotPressed;
        };
        // SAFETY: `item` is a retained AXUIElement and the action a valid string.
        if unsafe { item.perform_action(&CFString::from_static_str("AXPress")) } == AXError::Success
        {
            MenuPress::Pressed
        } else {
            MenuPress::NotPressed
        }
    })
}

/// The attribute names [`find_item`] reads, built once per search.
struct MenuAttrs {
    role: CFRetained<CFString>,
    enabled: CFRetained<CFString>,
    key: CFRetained<CFString>,
    modifiers: CFRetained<CFString>,
}

impl MenuAttrs {
    fn new() -> Self {
        Self {
            role: CFString::from_static_str("AXRole"),
            enabled: CFString::from_static_str("AXEnabled"),
            key: CFString::from_static_str("AXMenuItemCmdChar"),
            modifiers: CFString::from_static_str("AXMenuItemCmdModifiers"),
        }
    }
}

fn find_item(
    el: &AXUIElement,
    wanted: &MenuShortcut,
    attrs: &MenuAttrs,
    depth: u8,
) -> Option<CFRetained<AXUIElement>> {
    if depth == 0 {
        return None;
    }
    children(el).find_map(|child| {
        let is_item = attr_string(&child, &attrs.role).as_deref() == Some("AXMenuItem");
        if is_item
            && let Some(key) = attr_string(&child, &attrs.key)
            && MenuShortcut::of_ax(&key, attr_i64(&child, &attrs.modifiers).unwrap_or(0)) == *wanted
            && attr_bool(&child, &attrs.enabled) == Some(true)
        {
            return Some(child);
        }
        find_item(&child, wanted, attrs, depth - 1)
    })
}

fn frontmost_pid() -> Option<i32> {
    Some(
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()?
            .processIdentifier(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(text: &str) -> MenuShortcut {
        MenuShortcut::written(&text.parse().expect("valid chord")).expect("a character key")
    }

    #[test]
    fn a_menu_shows_letters_in_capitals_without_meaning_shift() {
        let remapped = MenuShortcut {
            key: "ñ".into(),
            modifiers: Modifiers {
                command: true,
                ..Modifiers::default()
            },
        };
        assert_eq!(MenuShortcut::of_ax("Ñ", 0), remapped);
        assert_ne!(MenuShortcut::of_ax("Ñ", AX_SHIFT), remapped);
    }

    #[test]
    fn accessibility_modifier_bits_imply_command_unless_told_otherwise() {
        assert_eq!(MenuShortcut::of_ax("[", 0), written("Cmd+["));
        assert_eq!(
            MenuShortcut::of_ax("z", AX_SHIFT | AX_OPTION),
            written("Cmd+Shift+Alt+Z")
        );
        assert_eq!(
            MenuShortcut::of_ax("k", AX_NO_COMMAND | AX_CONTROL),
            written("Ctrl+K")
        );
    }
}
