//! Copying one device's settings onto another: a new mouse set up like the
//! one it replaces, or a mouse of another generation set up like the rest.
//!
//! A transfer is computed first and applied second, so the UI can show what
//! it will change and what it leaves out before anything is written. Buttons
//! merge: the source's bindings land on the target, and the target's other
//! buttons keep theirs. Controls the target does not have, or that were
//! never measured on it, are left out and reported, never written. From a
//! file, bindings whose payload decides what they do (a command, a script,
//! typed text, an app, file or URL to open) are left out too: a shared file
//! must not put one on a button without the user seeing it. A file's
//! profiles bring the application aliases they name.

use std::collections::BTreeSet;

use super::{Config, DeviceConfig};
use crate::app::AppAliases;
use crate::binding::{Action, ActionRingConfig, ActionRingEntry, Binding, ButtonId};
use crate::device::{Capabilities, DeviceKind};

/// One setting a transfer writes on the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferItem {
    /// The binding of one button, a gesture map included.
    Button(ButtonId),
    /// The gesture directions remembered for a button whose gesture mode is
    /// off, so turning it back on restores them.
    StoredGestures(ButtonId),
    /// One button's override in one application's profile.
    AppButton {
        /// The application identifier the profile is keyed by.
        app: String,
        /// The overridden button.
        button: ButtonId,
    },
    /// The Actions Ring layouts and settings.
    ActionRing,
    /// The thumb wheel's sensitivity.
    ThumbwheelSensitivity,
    /// The main wheel's direction.
    InvertScroll,
    /// The main wheel's resolution.
    ScrollResolution,
    /// Smooth scrolling of the main wheel.
    SmoothWheel,
    /// Momentum scrolling of the main wheel.
    MomentumScrolling,
    /// SmartShift.
    SmartShift,
    /// The sensor resolution.
    Dpi,
    /// The DPI presets.
    DpiPresets,
    /// The pointer speed.
    PointerSpeed,
}

/// Why a transfer leaves a setting out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The target has no haptic sense panel.
    NoHapticPanel,
    /// The target has no thumb wheel.
    NoThumbwheel,
    /// The target's DPI button has no gestures.
    NoDpiGestures,
    /// The target cannot reverse its wheel.
    NoScrollInversion,
    /// The target's wheel has no high-resolution mode.
    NoHiresWheel,
    /// The target has no adjustable sensor resolution.
    NoDpi,
    /// The target has no adjustable pointer speed.
    NoPointerSpeed,
    /// A key or control a mouse does not have.
    NotOnAMouse,
    /// From a file, a binding that runs a shell command, an AppleScript or a
    /// workflow, types text, or opens an app, file or URL.
    NotFromAFile,
    /// The target's controls were never measured, so nothing shows it has
    /// the one this setting needs.
    NotMeasured,
    /// From a file, a profile for an application alias this config defines
    /// differently, or whose identifiers another alias here already lists.
    AppAliasConflict,
}

/// A setting a transfer leaves out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedItem {
    /// The setting.
    pub item: TransferItem,
    /// Why the target cannot take it.
    pub reason: SkipReason,
}

/// What a transfer needs to know about its target besides its settings:
/// its kind and the capabilities last measured on it. `None` capabilities
/// mean nothing was measured, so every setting that needs a control is left
/// out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferTarget {
    /// The target's kind.
    pub kind: Option<DeviceKind>,
    /// The target's capabilities.
    pub capabilities: Option<Capabilities>,
    /// Whether OpenLogi can reverse the target's wheel, through its firmware
    /// or in software. [`Capabilities::scroll_inversion`] covers only the
    /// firmware, so the caller decides this the way its scroll settings do.
    pub invertible_wheel: bool,
}

/// The outcome of a transfer: the target's settings afterwards, what
/// changed, and what was left out.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsTransfer {
    /// The target's settings with the transfer applied.
    pub settings: DeviceConfig,
    /// The application aliases of the target's config afterwards, when the
    /// profiles a file brings name aliases the config does not define yet.
    pub apps: Option<AppAliases>,
    /// The settings that change, in the order the UI lists them.
    pub changed: Vec<TransferItem>,
    /// The settings left out, in the same order.
    pub skipped: Vec<SkippedItem>,
}

impl SettingsTransfer {
    /// Whether applying the transfer writes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }
}

/// Transfer `source`'s buttons, scrolling and pointer settings onto
/// `target`, both devices of this machine. A setting the source never set
/// leaves the target's alone. A copied value also clears the target's
/// per-link override of it, so the copied value is the one that takes effect.
#[must_use]
pub fn transfer_settings(
    source: &DeviceConfig,
    target: &DeviceConfig,
    facts: TransferTarget,
) -> SettingsTransfer {
    run_transfer(source, target, facts, Origin::ThisMachine)
}

/// One device's settings read from a file, with the application aliases its
/// per-app profiles name.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedDevice {
    settings: DeviceConfig,
    apps: AppAliases,
}

impl ImportedDevice {
    /// The device's settings.
    #[must_use]
    pub fn settings(&self) -> &DeviceConfig {
        &self.settings
    }
}

/// Where a transfer's source comes from.
#[derive(Debug, Clone, Copy)]
enum Origin<'a> {
    /// A device in this machine's config.
    ThisMachine,
    /// A file, which may come from someone else.
    File {
        /// The aliases the file defines for the device's profiles.
        source: &'a AppAliases,
        /// The aliases this machine's config defines.
        here: &'a AppAliases,
    },
}

/// The key an export stores its one device under, in place of the key that
/// names the device's serial number or unit id.
const EXPORT_KEY: &str = "exported";

fn run_transfer(
    source: &DeviceConfig,
    target: &DeviceConfig,
    facts: TransferTarget,
    origin: Origin<'_>,
) -> SettingsTransfer {
    let mut run = Transfer {
        facts,
        origin,
        settings: target.clone(),
        apps: None,
        changed: Vec::new(),
        skipped: Vec::new(),
    };
    run.buttons(source);
    run.scrolling(source);
    run.pointer(source);
    SettingsTransfer {
        settings: run.settings,
        apps: run.apps,
        changed: run.changed,
        skipped: run.skipped,
    }
}

impl Config {
    /// [`transfer_settings`] from the device `source_key` onto the device
    /// `target_key`. A target with no entry yet starts from the defaults.
    /// `None` when the source has no entry.
    #[must_use]
    pub fn settings_transfer(
        &self,
        source_key: &str,
        target_key: &str,
        facts: TransferTarget,
    ) -> Option<SettingsTransfer> {
        let source = self.devices.get(source_key)?;
        let target = self.devices.get(target_key).cloned().unwrap_or_default();
        Some(transfer_settings(source, &target, facts))
    }

    /// The device `key` of this config, read as a file to import from.
    /// `None` when the config has no such device.
    #[must_use]
    pub fn imported_device(&self, key: &str) -> Option<ImportedDevice> {
        let settings = self.devices.get(key)?;
        Some(ImportedDevice {
            apps: self.apps.named_by(settings.profile_keys()),
            settings: settings.clone(),
        })
    }

    /// A transfer from `source`, a device read from a file, onto the device
    /// `target_key`. Bindings that run a command, a script or a workflow,
    /// type text, or open an app, file or URL are left out and reported as
    /// [`SkipReason::NotFromAFile`]. A profile keyed by an application alias
    /// brings the file's definition of it, and is left out as
    /// [`SkipReason::AppAliasConflict`] when that definition cannot join this
    /// config's.
    #[must_use]
    pub fn settings_transfer_from_file(
        &self,
        source: &ImportedDevice,
        target_key: &str,
        facts: TransferTarget,
    ) -> SettingsTransfer {
        let target = self.devices.get(target_key).cloned().unwrap_or_default();
        let origin = Origin::File {
            source: &source.apps,
            here: &self.apps,
        };
        run_transfer(&source.settings, &target, facts, origin)
    }

    /// Store `transfer`'s settings as the device `target_key`'s, with the
    /// application aliases it brings. `transfer` is one this config computed
    /// as it is now, so those aliases still fit its own.
    pub fn apply_settings_transfer(&mut self, target_key: &str, transfer: SettingsTransfer) {
        if let Some(apps) = transfer.apps {
            self.apps = apps;
        }
        self.devices
            .insert(target_key.to_owned(), transfer.settings);
    }

    /// The device `key`'s settings as a config file of their own, for
    /// another OpenLogi to import from. The routes it was seen on and its
    /// host-switch partners belong to this machine and stay out, and the
    /// entry is keyed by a neutral key instead of one naming the device's
    /// serial number or unit id, so a shared file carries no hardware
    /// identifier. Its identity stays in, so the importing side can name it,
    /// and so do the application aliases its profiles name, so the file
    /// loads. Other app settings are left at their defaults. `None` when the
    /// device has no entry.
    #[must_use]
    pub fn device_export(&self, key: &str) -> Option<Self> {
        let mut device = self.devices.get(key)?.clone();
        device.links.clear();
        device.host_switch_targets.clear();
        let mut export = Self {
            apps: self.apps.named_by(device.profile_keys()),
            ..Self::default()
        };
        export.devices.insert(EXPORT_KEY.to_owned(), device);
        Some(export)
    }
}

struct Transfer<'a> {
    facts: TransferTarget,
    origin: Origin<'a>,
    settings: DeviceConfig,
    /// The target config's aliases once the transfer defines one.
    apps: Option<AppAliases>,
    changed: Vec<TransferItem>,
    skipped: Vec<SkippedItem>,
}

impl Transfer<'_> {
    /// Why a setting cannot come along when the target's controls were never
    /// measured.
    fn measured(&self) -> Option<SkipReason> {
        self.facts
            .capabilities
            .is_none()
            .then_some(SkipReason::NotMeasured)
    }

    /// Why the target cannot take a setting that needs `control`, if it
    /// cannot: it lacks the control, reported as `missing`, or its controls
    /// were never measured.
    fn gap(
        &self,
        control: impl Fn(&Capabilities) -> bool,
        missing: SkipReason,
    ) -> Option<SkipReason> {
        match &self.facts.capabilities {
            None => Some(SkipReason::NotMeasured),
            Some(capabilities) if !control(capabilities) => Some(missing),
            Some(_) => None,
        }
    }

    /// Why the target's wheel cannot be reversed, if it cannot.
    fn inversion_gap(&self) -> Option<SkipReason> {
        self.measured()
            .or_else(|| (!self.facts.invertible_wheel).then_some(SkipReason::NoScrollInversion))
    }

    fn skip(&mut self, item: TransferItem, reason: SkipReason) {
        self.skipped.push(SkippedItem { item, reason });
    }

    /// Whether `actions` must stay out because the source is a file and one
    /// of them does what its payload says.
    fn refuses<'s>(&self, mut actions: impl Iterator<Item = &'s Action>) -> bool {
        matches!(self.origin, Origin::File { .. }) && actions.any(acts_on_its_payload)
    }

    /// Whether `item` may come along: `gap` is `None` and none of `actions`
    /// is one a file must not bring. Otherwise it is reported as skipped.
    fn admit<'s>(
        &mut self,
        item: TransferItem,
        gap: Option<SkipReason>,
        actions: impl Iterator<Item = &'s Action>,
    ) -> bool {
        let reason = gap.or_else(|| self.refuses(actions).then_some(SkipReason::NotFromAFile));
        match reason {
            Some(reason) => {
                self.skip(item, reason);
                false
            }
            None => true,
        }
    }

    /// Whether `item`, which stores the per-app profiles `keys`, may come
    /// along. From a file, each `app:` key brings the file's alias, which
    /// must join this config's aliases the way a loaded table is checked;
    /// otherwise the item is reported as skipped.
    fn bring_profiles<'k>(
        &mut self,
        item: &TransferItem,
        keys: impl IntoIterator<Item = &'k str>,
    ) -> bool {
        let Origin::File { source, here } = self.origin else {
            return true;
        };
        let mut defined: Option<AppAliases> = None;
        for key in keys {
            let current = defined.as_ref().or(self.apps.as_ref()).unwrap_or(here);
            let next = match source.for_profile_key(key) {
                Some((name, alias)) => current.defining(name, alias),
                None if current.is_undefined_reference(key) => None,
                None => continue,
            };
            let Some(next) = next else {
                self.skip(item.clone(), SkipReason::AppAliasConflict);
                return false;
            };
            if next != *current {
                defined = Some(next);
            }
        }
        if defined.is_some() {
            self.apps = defined;
        }
        true
    }

    /// Why the target cannot take `binding` on `button`, if it cannot.
    fn button_gap(&self, button: ButtonId, binding: Option<&Binding>) -> Option<SkipReason> {
        let is_mouse = self.facts.kind == Some(DeviceKind::Mouse);
        if is_mouse && !is_mouse_control(button) {
            return Some(SkipReason::NotOnAMouse);
        }
        match button {
            ButtonId::HapticPanel => self.gap(|c| c.haptic_panel, SkipReason::NoHapticPanel),
            ButtonId::Thumbwheel
            | ButtonId::ThumbwheelScrollUp
            | ButtonId::ThumbwheelScrollDown => self.gap(has_thumbwheel, SkipReason::NoThumbwheel),
            ButtonId::DpiToggle if matches!(binding, None | Some(Binding::Gesture(_))) => {
                self.gap(|c| c.dpi_gestures, SkipReason::NoDpiGestures)
            }
            _ => None,
        }
    }

    fn buttons(&mut self, source: &DeviceConfig) {
        let buttons: BTreeSet<ButtonId> = source
            .bindings
            .keys()
            .chain(source.disabled_gestures.keys())
            .copied()
            .collect();
        for button in buttons {
            self.button(source, button);
        }
        for (app, overrides) in &source.per_app_bindings {
            for (&button, action) in overrides {
                let item = TransferItem::AppButton {
                    app: app.clone(),
                    button,
                };
                let gap = self.button_gap(button, Some(&Binding::Single(action.clone())));
                if !self.admit(item.clone(), gap, std::iter::once(action))
                    || !self.bring_profiles(&item, [app.as_str()])
                {
                    continue;
                }
                let current = self
                    .settings
                    .per_app_bindings
                    .get(app)
                    .and_then(|profile| profile.get(&button));
                if current != Some(action) {
                    self.settings
                        .per_app_bindings
                        .entry(app.clone())
                        .or_default()
                        .insert(button, action.clone());
                    self.changed.push(item);
                }
            }
        }
        let ring = &source.action_ring;
        if ring.is_default()
            || !self.admit(TransferItem::ActionRing, None, ring_actions(ring))
            || !self.bring_profiles(
                &TransferItem::ActionRing,
                ring.per_app.keys().map(String::as_str),
            )
        {
            return;
        }
        if self.settings.action_ring != *ring {
            self.settings.action_ring = ring.clone();
            self.changed.push(TransferItem::ActionRing);
        }
    }

    /// Copy `button` from `source`: its binding and the directions stored
    /// for its gesture mode, each as far as the target can take it.
    fn button(&mut self, source: &DeviceConfig, button: ButtonId) {
        let binding = source.bindings.get(&button).filter(|binding| {
            let gap = self.button_gap(button, Some(binding));
            self.admit(TransferItem::Button(button), gap, binding_actions(binding))
        });
        let stored = source.disabled_gestures.get(&button).filter(|directions| {
            let gap = self.button_gap(button, None);
            self.admit(
                TransferItem::StoredGestures(button),
                gap,
                directions.values(),
            )
        });
        let copy = self.settings.copy_button(button, binding, stored);
        if copy.binding {
            self.changed.push(TransferItem::Button(button));
        }
        if copy.stored_gestures {
            self.changed.push(TransferItem::StoredGestures(button));
        }
    }

    fn scrolling(&mut self, source: &DeviceConfig) {
        if let Some(sensitivity) = source.thumbwheel_sensitivity {
            let item = TransferItem::ThumbwheelSensitivity;
            if let Some(reason) = self.gap(has_thumbwheel, SkipReason::NoThumbwheel) {
                self.skip(item, reason);
            } else if self.settings.thumbwheel_sensitivity != Some(sensitivity) {
                self.settings.thumbwheel_sensitivity = Some(sensitivity);
                self.changed.push(item);
            }
        }
        // `invert_scroll` and `smooth_wheel` default to off and the file
        // omits them then, so a source with one off never set it.
        if source.invert_scroll {
            if let Some(reason) = self.inversion_gap() {
                self.skip(TransferItem::InvertScroll, reason);
            } else {
                let overridden = self.clear_overrides(|o| o.invert_scroll.take().is_some());
                if overridden || !self.settings.invert_scroll {
                    self.settings.invert_scroll = true;
                    self.changed.push(TransferItem::InvertScroll);
                }
            }
        }
        if let Some(resolution) = source.scroll_resolution {
            let item = TransferItem::ScrollResolution;
            if let Some(reason) = self.gap(|c| c.hires_wheel, SkipReason::NoHiresWheel) {
                self.skip(item, reason);
            } else {
                let overridden = self.clear_overrides(|o| o.scroll_resolution.take().is_some());
                if overridden || self.settings.scroll_resolution != Some(resolution) {
                    self.settings.scroll_resolution = Some(resolution);
                    self.changed.push(item);
                }
            }
        }
        if source.smooth_wheel {
            if let Some(reason) = self.gap(|c| c.hires_wheel, SkipReason::NoHiresWheel) {
                self.skip(TransferItem::SmoothWheel, reason);
            } else if !self.settings.smooth_wheel {
                self.settings.smooth_wheel = true;
                self.changed.push(TransferItem::SmoothWheel);
            }
        }
        if let Some(momentum) = source.momentum_scrolling
            && self.settings.momentum_scrolling != Some(momentum)
        {
            self.settings.momentum_scrolling = Some(momentum);
            self.changed.push(TransferItem::MomentumScrolling);
        }
        if let Some(smartshift) = source.smartshift {
            if let Some(reason) = self.measured() {
                self.skip(TransferItem::SmartShift, reason);
            } else {
                let overridden = self.clear_overrides(|o| o.smartshift.take().is_some());
                if overridden || self.settings.smartshift != Some(smartshift) {
                    self.settings.smartshift = Some(smartshift);
                    self.changed.push(TransferItem::SmartShift);
                }
            }
        }
    }

    fn pointer(&mut self, source: &DeviceConfig) {
        if let Some(dpi) = source.dpi {
            if let Some(reason) = self.gap(|c| c.pointer, SkipReason::NoDpi) {
                self.skip(TransferItem::Dpi, reason);
            } else {
                let overridden = self.clear_overrides(|o| o.dpi.take().is_some());
                if overridden || self.settings.dpi != Some(dpi) {
                    self.settings.dpi = Some(dpi);
                    self.changed.push(TransferItem::Dpi);
                }
            }
        }
        if !source.dpi_presets.is_empty() {
            if let Some(reason) = self.gap(|c| c.pointer, SkipReason::NoDpi) {
                self.skip(TransferItem::DpiPresets, reason);
            } else if self.settings.dpi_presets != source.dpi_presets {
                self.settings.dpi_presets.clone_from(&source.dpi_presets);
                self.changed.push(TransferItem::DpiPresets);
            }
        }
        if let Some(speed) = source.pointer_speed {
            if let Some(reason) = self.gap(|c| c.pointer_scaling, SkipReason::NoPointerSpeed) {
                self.skip(TransferItem::PointerSpeed, reason);
            } else if self.settings.pointer_speed != Some(speed) {
                self.settings.pointer_speed = Some(speed);
                self.changed.push(TransferItem::PointerSpeed);
            }
        }
    }

    /// Run `clear` on every link's overrides; whether any link had one.
    fn clear_overrides(&mut self, clear: impl Fn(&mut super::LinkOverrides) -> bool) -> bool {
        let mut any = false;
        for link in self.settings.links.values_mut() {
            any |= clear(&mut link.overrides);
        }
        any
    }
}

/// Every action `binding` can fire.
fn binding_actions(binding: &Binding) -> impl Iterator<Item = &Action> {
    let actions: Vec<&Action> = match binding {
        Binding::Single(action) => vec![action],
        Binding::Gesture(directions) => directions.values().collect(),
        Binding::LongPress(binding) => vec![binding.short(), binding.long()],
        Binding::WheelHold(binding) => vec![binding.click()],
    };
    actions.into_iter()
}

/// Every action in the Actions Ring's layouts.
fn ring_actions(ring: &ActionRingConfig) -> impl Iterator<Item = &Action> {
    std::iter::once(&ring.default)
        .chain(ring.per_app.values())
        .flat_map(|layout| layout.slots.values())
        .map(ActionRingEntry::action)
}

/// Whether `action` does what its own payload says: runs a shell command, an
/// AppleScript or a workflow, types text, or opens an app, file or URL. The
/// label of such an action need not match what it does, so a file must not
/// bring one.
fn acts_on_its_payload(action: &Action) -> bool {
    matches!(
        action,
        Action::RunAppleScript(_)
            | Action::RunShellCommand(_)
            | Action::Workflow(_)
            | Action::TypeText(_)
            | Action::OpenApplication(_)
    )
}

fn has_thumbwheel(capabilities: &Capabilities) -> bool {
    capabilities.thumbwheel || capabilities.virtual_thumbwheel
}

/// Whether `button` is a control a mouse can have. Keypad grid slots,
/// raw keyboard controls, G-keys and mode keys belong to keyboards.
fn is_mouse_control(button: ButtonId) -> bool {
    !matches!(
        button,
        ButtonId::Grid(_) | ButtonId::Control(_) | ButtonId::GKey(_) | ButtonId::ModeKey(_)
    )
}

#[cfg(test)]
mod tests;
