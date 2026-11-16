//! Key modules on the orchestrator's side: which keys each module owns,
//! the images it sent for them, and where a press of one goes.

use std::collections::BTreeMap;

use openlogi_core::binding::{Binding, ButtonId, GridKey, GridSlot};
use openlogi_core::config::Config;
use openlogi_core::module::ModuleId;
use tracing::debug;

use super::{Orchestrator, publish_arc_if_changed};
use crate::modules::{ModuleEvent, ModuleKey, ModulePlan, ModulePress};
use crate::watchers::key_display::KeyFace;

impl Orchestrator {
    /// The keys each module owns: on every keypad whose keys show images, the
    /// keys whose binding on the current page, for the app in front, is that
    /// module: exactly the bindings those keys dispatch and show.
    pub(super) fn module_keys(&self) -> BTreeMap<ModuleId, Vec<ModuleKey>> {
        let mut keys: BTreeMap<ModuleId, Vec<ModuleKey>> = BTreeMap::new();
        for keypad in self.display_keypads() {
            for key in GridKey::ALL {
                let Some(id) = bound_module(keypad.bindings.get(&ButtonId::grid(key))) else {
                    continue;
                };
                keys.entry(id).or_default().push(ModuleKey {
                    device: keypad.dev.config_key.clone(),
                    slot: GridSlot {
                        page: keypad.page,
                        key,
                    },
                    size: keypad.display.key_size,
                });
            }
        }
        keys
    }

    /// The plan the module host runs from.
    pub(super) fn module_plan_for(&self) -> ModulePlan {
        ModulePlan {
            config: self.config.modules.clone(),
            keys: self.module_keys(),
            turned_off: self.modules_turned_off.clone(),
        }
    }

    /// Count each module `previous` turned on that the current config does
    /// not.
    pub(super) fn count_modules_turned_off(&mut self, previous: &Config) {
        for id in previous.modules.keys() {
            if previous.module_enabled(id) && !self.config.module_enabled(id) {
                *self.modules_turned_off.entry(id.clone()).or_default() += 1;
            }
        }
    }

    /// What a key bound as `binding` at `slot` of `device` shows when the
    /// binding hands it to a module: the image the module sent for it, or
    /// `None` for the binding's own face (the module glyph, for a module
    /// that has sent none).
    pub(super) fn module_face(
        &self,
        device: &str,
        slot: GridSlot,
        binding: Option<&Binding>,
    ) -> Option<KeyFace> {
        let id = bound_module(binding)?;
        self.module_faces
            .get(&id)?
            .get(&(device.to_owned(), slot))
            .cloned()
            .map(KeyFace::Image)
    }

    /// Apply a face change a module asked for, or drop its images when it
    /// stopped, and republish what the keypads show.
    ///
    /// An image counts only for a key the module owns as it arrives; one
    /// for `image_all` covers the keys it owns then. A key keeps the image
    /// after the module loses it, shown only while the module owns it, as
    /// checked when faces are rendered, so a key it never drew shows the
    /// module glyph until the module sends for it.
    pub fn apply_module_event(&mut self, event: ModuleEvent) {
        let keys = self.module_keys();
        let owned = |module: &ModuleId| keys.get(module).map_or(&[][..], Vec::as_slice);
        match event {
            ModuleEvent::Image {
                module,
                device,
                slot,
                image,
            } => {
                if !owned(&module)
                    .iter()
                    .any(|key| key.device == device && key.slot == slot)
                {
                    debug!(%module, device, slot = %slot.config_name(), "module image for a key it does not own: ignored");
                    return;
                }
                self.module_faces
                    .entry(module)
                    .or_default()
                    .insert((device, slot), image);
            }
            ModuleEvent::ImageAll { module, image } => {
                let owned = owned(&module);
                if !owned.is_empty() {
                    let faces = self.module_faces.entry(module).or_default();
                    for key in owned {
                        faces.insert((key.device.clone(), key.slot), image.clone());
                    }
                }
            }
            ModuleEvent::Clear {
                module,
                device,
                slot,
            } => {
                if let Some(faces) = self.module_faces.get_mut(&module) {
                    faces.remove(&(device, slot));
                    if faces.is_empty() {
                        self.module_faces.remove(&module);
                    }
                }
            }
            ModuleEvent::Stopped { module } => {
                self.module_faces.remove(&module);
            }
        }
        publish_arc_if_changed(&self.key_display_specs_tx, self.key_display_specs_for());
    }

    /// Where a press of grid key `key` on `device_key`, whose binding named
    /// `module` when it went down, goes: to `module`, at the slot of the
    /// keypad's current page, while that key is still bound to it. `None`
    /// when the binding moved on meanwhile, or the press names no keypad.
    #[must_use]
    pub fn module_press(
        &self,
        device_key: Option<&str>,
        module: &ModuleId,
        key: GridKey,
    ) -> Option<ModulePress> {
        let device = device_key?;
        let page = self.keypad_page(device);
        let bindings = self.current_page_of(device, &self.foreground_key_bindings(device));
        if bound_module(bindings.get(&ButtonId::grid(key))).as_ref() != Some(module) {
            debug!(%module, device, ?key, "module key rebound before its press arrived: dropped");
            return None;
        }
        Some(ModulePress {
            module: module.clone(),
            device: device.to_owned(),
            slot: GridSlot { page, key },
        })
    }
}

/// The module a key bound as `binding` hands its presses and face to.
fn bound_module(binding: Option<&Binding>) -> Option<ModuleId> {
    binding
        .and_then(Binding::key_action)
        .and_then(|action| action.module().cloned())
}
