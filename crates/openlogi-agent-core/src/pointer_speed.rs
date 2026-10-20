//! Pointer speed and precision mode of the mice whose speed HID++ `0x2205`
//! scales ([`Capabilities::pointer_scaling`]).
//!
//! Precision mode follows Logi Options+. A press of a button bound to
//! [`Action::TogglePrecisionMode`] turns it on, and the mouse moves at
//! [`PointerSpeed::MIN`]. The next press turns it off, and the mouse returns
//! to its saved speed, or to 1× when no speed is saved. Two other events end
//! it: a speed the settings app writes, and a config reload that changes the
//! mouse's saved speed or its button assignments.
//!
//! The mode lives in the agent process only. It survives a reconnect,
//! because the reconnect reapply writes the precision speed while the mode is
//! on. When the mode ends, by a press or a reload, online or offline, the
//! mouse is owed its saved speed, or 1× when no speed is saved. Every
//! reconnect pass writes it until one write of it lands, so a write lost to
//! a sleeping mouse does not leave it slow. The mode does not survive an
//! agent restart: the new agent writes the saved speed, and a mouse with no
//! saved speed stays at the precision speed until its button is pressed
//! twice.
//!
//! Every pointer-speed write is numbered in one `WriteOrder`, and each
//! number is taken under the lock that reads or changes the mode. The newest
//! number therefore always carries the newest state, and an older write that
//! has not reached the mouse yet is skipped.
//!
//! [`Capabilities::pointer_scaling`]: openlogi_core::device::Capabilities::pointer_scaling
//! [`Action::TogglePrecisionMode`]: openlogi_core::binding::Action::TogglePrecisionMode

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use openlogi_core::hid::{DeviceRoute, PointerSpeed};

use crate::hardware::{WriteOrder, WriteTicket};

/// The precision mode of every mouse whose speed `0x2205` scales, and the
/// order of its pointer-speed writes.
#[derive(Default)]
pub struct PointerSpeeds {
    order: WriteOrder,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Config key of the GUI-selected device: the target of a press that
    /// names no device, as for the DPI actions.
    selected: Option<String>,
    /// Every online, enabled mouse whose speed `0x2205` scales, by config key.
    online: HashMap<String, OnlineMouse>,
    /// The config keys whose precision mode is on, online or not.
    precision: HashSet<String>,
    /// The config keys of the mice that may still move at the precision
    /// speed: their precision mode ended, and no write of their own speed
    /// has landed since. A reconnect writes such a mouse even when no speed
    /// is saved. Each entry has a generation, so a write that lands late
    /// cannot clear a newer entry.
    restore: HashMap<String, u64>,
    /// The generation of the newest `restore` entry.
    restore_generation: u64,
}

impl State {
    /// Record that `key`'s mouse may still move at the precision speed, and
    /// return the entry the write that ends it settles.
    fn owe_restore(&mut self, key: &str) -> Restore {
        self.restore_generation += 1;
        self.restore.insert(key.to_owned(), self.restore_generation);
        Restore {
            key: key.to_owned(),
            generation: self.restore_generation,
        }
    }

    /// `key`'s pending restore entry, if any.
    fn pending_restore(&self, key: &str) -> Option<Restore> {
        self.restore.get(key).map(|&generation| Restore {
            key: key.to_owned(),
            generation,
        })
    }
}

struct OnlineMouse {
    route: DeviceRoute,
    saved: Option<PointerSpeed>,
}

/// One online, enabled mouse whose speed `0x2205` scales, as the
/// orchestrator publishes it.
pub(crate) struct ScaledMouse {
    /// The mouse's config key.
    pub(crate) key: String,
    /// Where the mouse is reached now.
    pub(crate) route: DeviceRoute,
    /// The speed saved for it in `config.toml`.
    pub(crate) saved: Option<PointerSpeed>,
}

/// One pointer-speed write, numbered when it was requested.
pub(crate) struct SpeedWrite {
    /// The mouse to write.
    pub(crate) route: DeviceRoute,
    /// The speed to write.
    pub(crate) speed: PointerSpeed,
    /// Its place among the mouse's pointer-speed writes.
    pub(crate) ticket: WriteTicket,
    /// The pending restore this write settles once it lands, if any. Pass
    /// it to [`PointerSpeeds::restored`] then.
    pub(crate) restore: Option<Restore>,
}

/// A pending restore a reconnect write carries: the mouse's config key and
/// the generation of its entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Restore {
    key: String,
    generation: u64,
}

/// What a config reload changed for one mouse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReloadChange {
    /// The saved speed differs.
    pub(crate) speed: bool,
    /// The button assignments differ.
    pub(crate) assignments: bool,
}

impl ReloadChange {
    /// Whether the change ends precision mode, as in Logi Options+.
    fn ends_precision(self) -> bool {
        self.speed || self.assignments
    }
}

impl PointerSpeeds {
    /// Replace the online mice and the selected device. The precision mode
    /// of every mouse is kept, online or not, so a mouse that reconnects
    /// comes back in the mode it left in.
    pub(crate) fn publish(
        &self,
        selected: Option<&str>,
        mice: impl IntoIterator<Item = ScaledMouse>,
    ) {
        let mut state = self.lock();
        state.selected = selected.map(str::to_owned);
        state.online = mice
            .into_iter()
            .map(|mouse| {
                let ScaledMouse { key, route, saved } = mouse;
                (key, OnlineMouse { route, saved })
            })
            .collect();
    }

    /// Turn precision mode on or off for the mouse `key` names, or for the
    /// selected device when `key` is `None`. Returns whether the mode is now
    /// on and the write that applies it, or `None` when that device is not
    /// an online mouse whose speed `0x2205` scales.
    pub(crate) fn toggle_precision(&self, key: Option<&str>) -> Option<(bool, SpeedWrite)> {
        let mut state = self.lock();
        let key = key.or(state.selected.as_deref())?.to_owned();
        let mouse = state.online.get(&key)?;
        let (route, saved) = (mouse.route.clone(), mouse.saved);
        if state.precision.remove(&key) {
            // The mouse stays owed its speed until this write lands.
            let restore = state.owe_restore(&key);
            let write = self.write(route, saved.unwrap_or_default(), Some(restore));
            Some((false, write))
        } else {
            state.restore.remove(&key);
            state.precision.insert(key);
            Some((true, self.write(route, PointerSpeed::MIN, None)))
        }
    }

    /// The write a reconnect owes the mouse `key` names, now at `route`: the
    /// precision speed while the mode is on, else the `saved` speed. A mouse
    /// whose mode ended gets 1× when no speed is saved, on every reconnect
    /// pass until [`Self::restored`] reports that a write of its speed
    /// landed. `None` when there is nothing to write.
    pub(crate) fn reapply(
        &self,
        key: &str,
        route: &DeviceRoute,
        saved: Option<PointerSpeed>,
    ) -> Option<SpeedWrite> {
        let state = self.lock();
        if state.precision.contains(key) {
            return Some(self.write(route.clone(), PointerSpeed::MIN, None));
        }
        let Some(restore) = state.pending_restore(key) else {
            return Some(self.write(route.clone(), saved?, None));
        };
        Some(self.write(route.clone(), saved.unwrap_or_default(), Some(restore)))
    }

    /// A write that carried `restore` landed: the mouse no longer moves at
    /// the precision speed. Clears the pending restore unless a newer reload
    /// replaced it.
    pub(crate) fn restored(&self, restore: &Restore) {
        let mut state = self.lock();
        if state.restore.get(&restore.key) == Some(&restore.generation) {
            state.restore.remove(&restore.key);
        }
    }

    /// The settings app writes `speed` to the mouse at `route`. Ends that
    /// mouse's precision mode, makes `speed` the one precision mode returns
    /// to until the next reload, and numbers the write.
    pub(crate) fn app_write(&self, route: &DeviceRoute, speed: PointerSpeed) -> WriteTicket {
        let mut state = self.lock();
        let State {
            online,
            precision,
            restore,
            ..
        } = &mut *state;
        for (key, mouse) in online.iter_mut().filter(|(_, mouse)| mouse.route == *route) {
            precision.remove(key);
            restore.remove(key);
            mouse.saved = Some(speed);
        }
        self.order.request(route)
    }

    /// The writes a config reload owes. `change` tells what the reload
    /// changed for a config key, and `mice` are the online mice.
    ///
    /// A changed speed or changed assignments end precision mode on every
    /// mouse, online or not, as in Logi Options+. A mouse whose mode ended
    /// returns to its saved speed, or to 1× when no speed is saved: an online
    /// mouse now, and every mouse at its reconnects until one such write
    /// lands. An online mouse whose speed alone changed gets the new speed.
    /// The other mice get no write.
    pub(crate) fn reload(
        &self,
        change: impl Fn(&str) -> ReloadChange,
        mice: impl IntoIterator<Item = ScaledMouse>,
    ) -> Vec<SpeedWrite> {
        let mut state = self.lock();
        let mut ended: HashSet<String> = state
            .precision
            .extract_if(|key| change(key).ends_precision())
            .collect();
        // The writes are numbered while the lock is held, as every write is.
        let writes = mice
            .into_iter()
            .filter_map(|ScaledMouse { key, route, saved }| {
                let (speed, restore) = if ended.remove(&key) {
                    (saved.unwrap_or_default(), Some(state.owe_restore(&key)))
                } else if change(&key).speed {
                    // A pending restore rides on this write: 1× when the
                    // speed was removed, since the mouse may still be slow.
                    let restore = state.pending_restore(&key);
                    let fallback = restore.as_ref().map(|_| PointerSpeed::default());
                    (saved.or(fallback)?, restore)
                } else {
                    return None;
                };
                Some(self.write(route, speed, restore))
            })
            .collect();
        // What is left of `ended` is offline: its reconnect owes the write.
        for key in ended {
            state.owe_restore(&key);
        }
        writes
    }

    /// Whether a reconnect still owes the mouse `key` names a restore.
    #[cfg(test)]
    pub(crate) fn restore_pending(&self, key: &str) -> bool {
        self.lock().restore.contains_key(key)
    }

    /// Whether precision mode is on for the mouse `key` names.
    #[cfg(test)]
    pub(crate) fn precision(&self, key: &str) -> bool {
        self.lock().precision.contains(key)
    }

    fn write(
        &self,
        route: DeviceRoute,
        speed: PointerSpeed,
        restore: Option<Restore>,
    ) -> SpeedWrite {
        let ticket = self.order.request(&route);
        SpeedWrite {
            route,
            speed,
            ticket,
            restore,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A few maps that every method rewrites whole entries of: a panic
        // mid-update leaves at worst one mouse in the wrong mode, which the
        // next press fixes.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
