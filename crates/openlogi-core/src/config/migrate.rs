//! Schema migrations: the passes that rewrite a config parsed from an older
//! schema into the current shape.
//!
//! [`SCHEMA_VERSION`](super::SCHEMA_VERSION) says what each version changed.
//! The loader runs the passes a file's declared version calls for, oldest
//! schema first.

use std::collections::BTreeMap;

#[cfg(feature = "fs")]
use super::settings::GestureOwner;
use super::{Config, identity};
#[cfg(feature = "fs")]
use crate::binding::{
    Action, Binding, ButtonId, GestureDirection, default_binding, default_binding_for,
    default_gesture_binding,
};
use crate::device_order::PhysicalDeviceKey;

impl Config {
    /// The single button the pre-v4 owner-locked runtime would have dispatched
    /// gestures from, inferred from the binding shapes — the owner-lock-era
    /// resolution rule, retained solely for
    /// [`Self::migrate_owner_locked_gestures`]. `None` means gestures were off.
    #[cfg(feature = "fs")]
    fn infer_gesture_owner(bindings: &BTreeMap<ButtonId, Binding>) -> Option<ButtonId> {
        // An OS-hook button left in gesture mode took the role over.
        if let Some((id, _)) = bindings
            .iter()
            .find(|(id, b)| **id != ButtonId::GestureButton && b.is_gesture())
        {
            return Some(*id);
        }
        // A dedicated HID++ gesture button explicitly assigned non-gesture
        // behavior means gestures were off.
        if matches!(
            bindings.get(&ButtonId::GestureButton),
            Some(Binding::Single(_) | Binding::LongPress(_))
        ) {
            return None;
        }
        // Default: the dedicated HID++ gesture button owns the gesture role.
        Some(ButtonId::GestureButton)
    }

    /// One-time load migration for owner-locked files (`schema_version <= 3`).
    ///
    /// Under the owner lock at most one button dispatched gestures; every other
    /// gesture-capable button could keep a dormant direction map awaiting
    /// re-selection, with [`DeviceConfig::gesture_owner`] recording the choice
    /// (absent = infer). The shape-driven model has no dormant state — a stored
    /// [`Binding::Gesture`] IS gesture mode — so this resolves the old owner
    /// and rewrites the shapes to dispatch exactly what the old config did:
    ///
    /// - the owner keeps its gesture map. A HID++ owner whose stored binding
    ///   is absent or `Single`-shaped gets the seeded default direction map
    ///   materialized: the v3 runtime seeded at projection time and dispatched
    ///   that map regardless of the stored shape, so leaving the shape
    ///   non-gesture would silently lose gestures in the rewritten file. (An
    ///   OS-hook owner is different — the v3 hook only dispatched a stored
    ///   gesture map, so a `Single` owner stays single.)
    /// - every other gesture-shaped binding is stashed into
    ///   [`DeviceConfig::disabled_gestures`] — keeping the owner-lock model's
    ///   restore-on-reselection promise — and demotes to a [`Binding::Single`]
    ///   of its `Click`, the only part of a dormant map the old runtime
    ///   dispatched;
    /// - a non-owner dedicated gesture button with no stored binding is pinned
    ///   with an explicit `Single` at its canonical default (absence would
    ///   re-enter gesture mode under the gesture-shaped default), which the
    ///   capture layer leaves native;
    /// - the consumed `gesture_owner` never serializes again — the shape is
    ///   the whole truth from here on.
    ///
    /// [`DeviceConfig::gesture_owner`]: super::DeviceConfig::gesture_owner
    /// [`DeviceConfig::disabled_gestures`]: super::DeviceConfig::disabled_gestures
    #[cfg(feature = "fs")]
    pub(super) fn migrate_owner_locked_gestures(&mut self) {
        for device in self.devices.values_mut() {
            let owner = match device.gesture_owner.take() {
                Some(GestureOwner::Off) => None,
                Some(GestureOwner::Button(id)) => Some(id),
                None => Self::infer_gesture_owner(&device.bindings),
            };
            for (id, binding) in &mut device.bindings {
                if Some(*id) != owner {
                    if let Binding::Gesture(map) = binding {
                        device.disabled_gestures.insert(*id, map.clone());
                    }
                    binding.demote_to_single(default_binding(*id));
                }
            }
            if let Some(owner) = owner
                && owner.is_hidpp_gesture_source()
            {
                let seeded = || {
                    Binding::Gesture(
                        GestureDirection::ALL
                            .iter()
                            .copied()
                            .map(|d| (d, default_gesture_binding(d)))
                            .collect(),
                    )
                };
                match device.bindings.get_mut(&owner) {
                    // A stored non-gesture shape is replaced by the map v3
                    // actually dispatched.
                    Some(binding) if !binding.is_gesture() => *binding = seeded(),
                    Some(_) => {}
                    // An absent owner only needs materializing when its
                    // canonical default is not gesture-shaped (the haptic
                    // panel); an absent dedicated button already means
                    // default gesture mode.
                    None => {
                        if !default_binding_for(owner).is_gesture() {
                            device.bindings.insert(owner, seeded());
                        }
                    }
                }
            }
            if owner != Some(ButtonId::GestureButton) {
                device
                    .bindings
                    .entry(ButtonId::GestureButton)
                    .or_insert_with(|| Binding::Single(default_binding(ButtonId::GestureButton)));
            }
        }
    }

    /// Rewrite v4 transport-scoped direct keys to identity keys.
    ///
    /// `direct:046d:c08d:unit:6be9d300` names one mouse *and the cable it was
    /// plugged into*; `unit:6be9d300` names the mouse. The route it came from
    /// is kept as a link so the index survives the rename. Receiver keys are
    /// left alone — nothing on disk says which device is in a pairing slot, so
    /// they are folded at runtime instead (see `adopt_route`).
    ///
    /// A `direct:` key can appear three ways: as a device's own map key, as
    /// `selected_device`, or inside another device's `host_switch_targets` —
    /// and the last of those can name a device with no `[devices.…]` table of
    /// its own (nothing but the reference survives). The rename is computed
    /// once over every occurrence so all three are rewritten consistently,
    /// not just the ones that also own a device entry.
    ///
    /// Two entries can rename onto the same key — one mouse reached over both
    /// USB and Bluetooth-direct has a v4 entry per route — so the second one
    /// is folded in rather than inserted over the first. That is the one case
    /// where this pass would otherwise not be lossless.
    pub fn migrate_transport_scoped_keys(&mut self) {
        // A v4 direct key is `direct:<vid>:<pid>:<identity-kind>:<identity>`.
        // Splitting off the two leading id fields recovers the route to keep
        // and the identity fragment that becomes the new key.
        let parse_rename = |key: &str| -> Option<(String, String)> {
            let rest = key.strip_prefix("direct:")?;
            let mut parts = rest.splitn(3, ':');
            let vendor = parts.next()?;
            let product = parts.next()?;
            let identity = parts.next()?;
            PhysicalDeviceKey::parse(identity)?;
            Some((identity.to_string(), format!("direct:{vendor}:{product}")))
        };

        let renames: BTreeMap<String, (String, String)> = self
            .devices
            .keys()
            .cloned()
            .chain(self.selected_device.iter().cloned())
            .chain(
                self.devices
                    .values()
                    .flat_map(|device| device.host_switch_targets.iter().cloned()),
            )
            .filter_map(|key| {
                let renamed = parse_rename(&key)?;
                Some((key, renamed))
            })
            .collect();

        for (old, (new, route)) in &renames {
            let Some(mut device) = self.devices.remove(old) else {
                continue;
            };
            device.links.entry(route.clone()).or_default();
            // One device reached on two direct routes — an MX Master 3S over
            // USB and over Bluetooth-direct — has two v4 entries that rename
            // to the same identity key. Inserting would drop whichever lost
            // the `BTreeMap` ordering, bindings and all; folding is what
            // makes this phase lossless, and it is the same merge adoption
            // performs at runtime, so the second entry's disagreements land
            // as overrides on the route they were set for.
            match self.devices.get_mut(new) {
                Some(existing) => identity::fold(existing, device, route),
                None => {
                    self.devices.insert(new.clone(), device);
                }
            }
        }
        if let Some(new) = self
            .selected_device
            .as_deref()
            .and_then(|old| renames.get(old))
            .map(|(new, _)| new.clone())
        {
            self.selected_device = Some(new);
        }
        for device in self.devices.values_mut() {
            for target in &mut device.host_switch_targets {
                if let Some((new, _)) = renames.get(target) {
                    *target = new.clone();
                }
            }
        }
    }

    /// Preserve the native behavior of explicit pre-v7 thumb-wheel defaults.
    ///
    /// Before v7 the built-in pair was forward/up → right and backward/down →
    /// left. Because that pair equaled the defaults, an explicit copy in either
    /// a device or per-app profile kept the wheel native. v7 swaps the defaults
    /// after normalising device polarity; leaving an old pair in place would
    /// now request a real reversal. Rewrite only profiles whose effective pair
    /// was exactly the old default. Mixed/custom pairs already had literal
    /// diverted semantics and must remain untouched.
    #[cfg(feature = "fs")]
    pub(super) fn migrate_thumbwheel_native_direction(&mut self) {
        let old_up = Action::HorizontalScrollRight;
        let old_down = Action::HorizontalScrollLeft;
        let new_up = Action::HorizontalScrollLeft;
        let new_down = Action::HorizontalScrollRight;
        let is_old_default = |binding: Option<&Binding>, old: &Action| match binding {
            None => true,
            Some(Binding::Single(action)) => action == old,
            Some(Binding::Gesture(_) | Binding::LongPress(_)) => false,
        };

        for device in self.devices.values_mut() {
            let global_up_is_old =
                is_old_default(device.bindings.get(&ButtonId::ThumbwheelScrollUp), &old_up);
            let global_down_is_old = is_old_default(
                device.bindings.get(&ButtonId::ThumbwheelScrollDown),
                &old_down,
            );

            for overlay in device.per_app_bindings.values_mut() {
                let overrides_direction = overlay.contains_key(&ButtonId::ThumbwheelScrollUp)
                    || overlay.contains_key(&ButtonId::ThumbwheelScrollDown);
                let effective_up_is_old = overlay
                    .get(&ButtonId::ThumbwheelScrollUp)
                    .map_or(global_up_is_old, |action| action == &old_up);
                let effective_down_is_old = overlay
                    .get(&ButtonId::ThumbwheelScrollDown)
                    .map_or(global_down_is_old, |action| action == &old_down);
                if overrides_direction && effective_up_is_old && effective_down_is_old {
                    // Write the complete pair. This also handles a profile that
                    // reached the old defaults by overriding one half of a
                    // custom global pair and inheriting the other half.
                    overlay.insert(ButtonId::ThumbwheelScrollUp, new_up.clone());
                    overlay.insert(ButtonId::ThumbwheelScrollDown, new_down.clone());
                }
            }

            if global_up_is_old && global_down_is_old {
                if let Some(Binding::Single(action)) =
                    device.bindings.get_mut(&ButtonId::ThumbwheelScrollUp)
                {
                    *action = new_up.clone();
                }
                if let Some(Binding::Single(action)) =
                    device.bindings.get_mut(&ButtonId::ThumbwheelScrollDown)
                {
                    *action = new_down.clone();
                }
            }
        }
    }
}
