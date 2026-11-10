//! Copying one device's settings onto another: a new mouse set up like the
//! one it replaces, or a mouse of another generation set up like the rest.
//!
//! A transfer is computed first and applied second, so the UI can show what
//! it will change and what it leaves out before anything is written. Buttons
//! merge: the source's bindings land on the target, and the target's other
//! buttons keep theirs. Controls the target does not have are left out and
//! reported, never written. From a file, bindings that run a command or a
//! script are left out too: a shared file must not put one on a button
//! without the user seeing it.

use super::{Config, DeviceConfig};
use crate::binding::{Action, ActionRingConfig, ActionRingEntry, Binding, ButtonId, WorkflowStep};
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
    /// From a file, a binding that runs a shell command or an AppleScript,
    /// itself or as a workflow step.
    RunsCode,
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
/// mean nothing is known, and nothing is left out for want of a control.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferTarget {
    /// The target's kind.
    pub kind: Option<DeviceKind>,
    /// The target's capabilities.
    pub capabilities: Option<Capabilities>,
}

/// The outcome of a transfer: the target's settings afterwards, what
/// changed, and what was left out.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsTransfer {
    /// The target's settings with the transfer applied.
    pub settings: DeviceConfig,
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

/// Where a transfer's source comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// A device in this machine's config.
    ThisMachine,
    /// A file, which may come from someone else.
    File,
}

/// The key an export stores its one device under, in place of the key that
/// names the device's serial number or unit id.
const EXPORT_KEY: &str = "exported";

fn run_transfer(
    source: &DeviceConfig,
    target: &DeviceConfig,
    facts: TransferTarget,
    origin: Origin,
) -> SettingsTransfer {
    let mut run = Transfer {
        facts,
        origin,
        settings: target.clone(),
        changed: Vec::new(),
        skipped: Vec::new(),
    };
    run.buttons(source);
    run.scrolling(source);
    run.pointer(source);
    SettingsTransfer {
        settings: run.settings,
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

    /// A transfer from `source`, a device's settings read from a file, onto
    /// the device `target_key`. Bindings that run a shell command or an
    /// AppleScript are left out and reported as [`SkipReason::RunsCode`].
    #[must_use]
    pub fn settings_transfer_from_file(
        &self,
        source: &DeviceConfig,
        target_key: &str,
        facts: TransferTarget,
    ) -> SettingsTransfer {
        let target = self.devices.get(target_key).cloned().unwrap_or_default();
        run_transfer(source, &target, facts, Origin::File)
    }

    /// Store `transfer`'s settings as the device `target_key`'s.
    pub fn apply_settings_transfer(&mut self, target_key: &str, transfer: SettingsTransfer) {
        self.devices
            .insert(target_key.to_owned(), transfer.settings);
    }

    /// The device `key`'s settings as a config file of their own, for
    /// another OpenLogi to import from. The routes it was seen on and its
    /// host-switch partners belong to this machine and stay out, and the
    /// entry is keyed by a neutral key instead of one naming the device's
    /// serial number or unit id, so a shared file carries no hardware
    /// identifier. Its identity stays in, so the importing side can name it.
    /// App settings are left at their defaults. `None` when the device has
    /// no entry.
    #[must_use]
    pub fn device_export(&self, key: &str) -> Option<Self> {
        let mut device = self.devices.get(key)?.clone();
        device.links.clear();
        device.host_switch_targets.clear();
        let mut export = Self::default();
        export.devices.insert(EXPORT_KEY.to_owned(), device);
        Some(export)
    }
}

struct Transfer {
    facts: TransferTarget,
    origin: Origin,
    settings: DeviceConfig,
    changed: Vec<TransferItem>,
    skipped: Vec<SkippedItem>,
}

impl Transfer {
    /// Whether the target has a control, or nothing is known about it.
    fn has(&self, control: impl Fn(&Capabilities) -> bool) -> bool {
        self.facts.capabilities.as_ref().is_none_or(control)
    }

    fn skip(&mut self, item: TransferItem, reason: SkipReason) {
        self.skipped.push(SkippedItem { item, reason });
    }

    /// Whether `actions` must stay out because the source is a file and one
    /// of them runs a command or a script.
    fn refuses<'a>(&self, mut actions: impl Iterator<Item = &'a Action>) -> bool {
        self.origin == Origin::File && actions.any(runs_code)
    }

    /// Why the target cannot take `binding` on `button`, if it cannot.
    fn button_gap(&self, button: ButtonId, binding: Option<&Binding>) -> Option<SkipReason> {
        let is_mouse = self.facts.kind == Some(DeviceKind::Mouse);
        if is_mouse && !is_mouse_control(button) {
            return Some(SkipReason::NotOnAMouse);
        }
        match button {
            ButtonId::HapticPanel if !self.has(|c| c.haptic_panel) => {
                Some(SkipReason::NoHapticPanel)
            }
            ButtonId::Thumbwheel
            | ButtonId::ThumbwheelScrollUp
            | ButtonId::ThumbwheelScrollDown
                if !self.has(has_thumbwheel) =>
            {
                Some(SkipReason::NoThumbwheel)
            }
            ButtonId::DpiToggle
                if matches!(binding, None | Some(Binding::Gesture(_)))
                    && !self.has(|c| c.dpi_gestures) =>
            {
                Some(SkipReason::NoDpiGestures)
            }
            _ => None,
        }
    }

    fn buttons(&mut self, source: &DeviceConfig) {
        for (&button, binding) in &source.bindings {
            let item = TransferItem::Button(button);
            if let Some(reason) = self.button_gap(button, Some(binding)) {
                self.skip(item, reason);
            } else if self.refuses(binding_actions(binding)) {
                self.skip(item, SkipReason::RunsCode);
            } else if self.settings.bindings.get(&button) != Some(binding) {
                self.settings.bindings.insert(button, binding.clone());
                self.changed.push(item);
            }
        }
        for (&button, directions) in &source.disabled_gestures {
            let item = TransferItem::StoredGestures(button);
            if let Some(reason) = self.button_gap(button, None) {
                self.skip(item, reason);
            } else if self.refuses(directions.values()) {
                self.skip(item, SkipReason::RunsCode);
            } else if self.settings.disabled_gestures.get(&button) != Some(directions) {
                self.settings
                    .disabled_gestures
                    .insert(button, directions.clone());
                self.changed.push(item);
            }
        }
        for (app, overrides) in &source.per_app_bindings {
            for (&button, action) in overrides {
                let item = TransferItem::AppButton {
                    app: app.clone(),
                    button,
                };
                let binding = Binding::Single(action.clone());
                if let Some(reason) = self.button_gap(button, Some(&binding)) {
                    self.skip(item, reason);
                    continue;
                }
                if self.refuses(std::iter::once(action)) {
                    self.skip(item, SkipReason::RunsCode);
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
        if source.action_ring.is_default() {
            return;
        }
        if self.refuses(ring_actions(&source.action_ring)) {
            self.skip(TransferItem::ActionRing, SkipReason::RunsCode);
        } else if self.settings.action_ring != source.action_ring {
            self.settings.action_ring = source.action_ring.clone();
            self.changed.push(TransferItem::ActionRing);
        }
    }

    fn scrolling(&mut self, source: &DeviceConfig) {
        if let Some(sensitivity) = source.thumbwheel_sensitivity {
            let item = TransferItem::ThumbwheelSensitivity;
            if !self.has(has_thumbwheel) {
                self.skip(item, SkipReason::NoThumbwheel);
            } else if self.settings.thumbwheel_sensitivity != Some(sensitivity) {
                self.settings.thumbwheel_sensitivity = Some(sensitivity);
                self.changed.push(item);
            }
        }
        if self.has(|c| c.scroll_inversion) {
            let overridden = self.clear_overrides(|o| o.invert_scroll.take().is_some());
            if overridden || self.settings.invert_scroll != source.invert_scroll {
                self.settings.invert_scroll = source.invert_scroll;
                self.changed.push(TransferItem::InvertScroll);
            }
        } else if source.invert_scroll {
            self.skip(TransferItem::InvertScroll, SkipReason::NoScrollInversion);
        }
        if let Some(resolution) = source.scroll_resolution {
            let item = TransferItem::ScrollResolution;
            if self.has(|c| c.hires_wheel) {
                let overridden = self.clear_overrides(|o| o.scroll_resolution.take().is_some());
                if overridden || self.settings.scroll_resolution != Some(resolution) {
                    self.settings.scroll_resolution = Some(resolution);
                    self.changed.push(item);
                }
            } else {
                self.skip(item, SkipReason::NoHiresWheel);
            }
        }
        if self.has(|c| c.hires_wheel) {
            if self.settings.smooth_wheel != source.smooth_wheel {
                self.settings.smooth_wheel = source.smooth_wheel;
                self.changed.push(TransferItem::SmoothWheel);
            }
        } else if source.smooth_wheel {
            self.skip(TransferItem::SmoothWheel, SkipReason::NoHiresWheel);
        }
        if let Some(momentum) = source.momentum_scrolling
            && self.settings.momentum_scrolling != Some(momentum)
        {
            self.settings.momentum_scrolling = Some(momentum);
            self.changed.push(TransferItem::MomentumScrolling);
        }
        if let Some(smartshift) = source.smartshift {
            let overridden = self.clear_overrides(|o| o.smartshift.take().is_some());
            if overridden || self.settings.smartshift != Some(smartshift) {
                self.settings.smartshift = Some(smartshift);
                self.changed.push(TransferItem::SmartShift);
            }
        }
    }

    fn pointer(&mut self, source: &DeviceConfig) {
        if let Some(dpi) = source.dpi {
            if self.has(|c| c.pointer) {
                let overridden = self.clear_overrides(|o| o.dpi.take().is_some());
                if overridden || self.settings.dpi != Some(dpi) {
                    self.settings.dpi = Some(dpi);
                    self.changed.push(TransferItem::Dpi);
                }
            } else {
                self.skip(TransferItem::Dpi, SkipReason::NoDpi);
            }
        }
        if !source.dpi_presets.is_empty() {
            if !self.has(|c| c.pointer) {
                self.skip(TransferItem::DpiPresets, SkipReason::NoDpi);
            } else if self.settings.dpi_presets != source.dpi_presets {
                self.settings.dpi_presets.clone_from(&source.dpi_presets);
                self.changed.push(TransferItem::DpiPresets);
            }
        }
        if let Some(speed) = source.pointer_speed {
            if !self.has(|c| c.pointer_scaling) {
                self.skip(TransferItem::PointerSpeed, SkipReason::NoPointerSpeed);
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

/// Whether `action` runs a shell command or an AppleScript, itself or as a
/// workflow step.
fn runs_code(action: &Action) -> bool {
    match action {
        Action::RunAppleScript(_) | Action::RunShellCommand(_) => true,
        Action::Workflow(steps) => steps.iter().any(|step| {
            matches!(
                step,
                WorkflowStep::RunAppleScript(_) | WorkflowStep::RunShellCommand(_)
            )
        }),
        _ => false,
    }
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
