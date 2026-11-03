//! Pointer speed and precision mode of the mice whose speed OpenLogi sets.
//!
//! A mouse sets its speed in one of two ways, as in Logi Options+. HID++
//! `0x2205` scales its movement ([`Capabilities::pointer_scaling`]): the
//! "scaled" mice, such as the MX Ergo. Otherwise its sensor DPI sets the
//! speed (`0x2201` / `0x2202`, [`Capabilities::pointer`]): the "DPI" mice.
//! A mouse with both is a DPI mouse, as in Logi Options+.
//!
//! Precision mode follows Logi Options+. A press of a button bound to
//! [`Action::TogglePrecisionMode`] turns it on, and the mouse moves at its
//! slowest speed: [`PointerSpeed::MIN`], or the lowest DPI its sensor lists.
//! The next press turns it off, and the mouse returns to its saved speed or
//! DPI. When none is saved, it returns to its default: 1× for a scaled mouse,
//! the sensor's default DPI for a DPI mouse. Two other events end the mode:
//! a speed or DPI the settings app writes, and a config reload that changes
//! the mouse's saved speed or DPI or its button assignments. A DPI preset
//! press ends it on a DPI mouse too.
//!
//! The DPI LED shows the mode on a mouse with a `0x1300` LED of type DPI, as
//! in Logi Options+: every write that sets the precision speed lights it,
//! and every write that ends the precision speed darkens it. The other
//! writes leave the LED alone, so a mouse that never used precision mode
//! keeps the LED behavior of its firmware.
//!
//! The mode lives in the agent process only. It survives a reconnect,
//! because the reconnect reapply writes the precision speed while the mode is
//! on. When the mode ends, by a press or a reload, online or offline, the
//! mouse is owed its saved speed or its default. Every reconnect pass writes
//! it until one write of it lands, so a write lost to a sleeping mouse does
//! not leave it slow. The mode does not survive an agent restart: the new
//! agent writes the saved speed or DPI, and a mouse with none saved stays at
//! the precision speed until its button is pressed twice.
//!
//! Every pointer-speed write is numbered in one `WriteOrder`, and each
//! number is taken under the lock that reads or changes the mode. The newest
//! number therefore always carries the newest state, and an older write that
//! has not reached the mouse yet is skipped. For a DPI mouse, the DPI writes
//! of the settings app and of the DPI preset actions take numbers too.
//!
//! [`Capabilities::pointer_scaling`]: openlogi_core::device::Capabilities::pointer_scaling
//! [`Capabilities::pointer`]: openlogi_core::device::Capabilities::pointer
//! [`Action::TogglePrecisionMode`]: openlogi_core::binding::Action::TogglePrecisionMode

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use openlogi_core::hid::{DeviceRoute, Dpi, PointerSpeed};
use openlogi_hid::DpiTarget;

use crate::hardware::{WriteOrder, WriteTicket};

/// The precision mode of every mouse whose speed OpenLogi sets, and the
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
    /// Every online, enabled mouse whose speed OpenLogi sets, by config key.
    online: HashMap<String, OnlineMouse>,
    /// The config keys whose precision mode is on, online or not.
    precision: HashSet<String>,
    /// The config keys of the mice that may still move at the precision
    /// speed: their precision mode ended, and no write of their own speed
    /// has landed since. A reconnect writes such a mouse even when nothing
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
    saved: SavedSpeed,
}

/// How a mouse sets its speed, and the value saved for it in `config.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SavedSpeed {
    /// HID++ `0x2205` scales its movement. Holds the saved `pointer_speed`.
    Scaled(Option<PointerSpeed>),
    /// Its sensor DPI sets its speed. Holds the saved `dpi`.
    Dpi(Option<Dpi>),
}

impl SavedSpeed {
    /// The slowest speed: what precision mode writes.
    const fn precision(self) -> SpeedTarget {
        match self {
            Self::Scaled(_) => SpeedTarget::Scaled(PointerSpeed::MIN),
            Self::Dpi(_) => SpeedTarget::Dpi(DpiTarget::Lowest),
        }
    }

    /// The saved value, or the mouse's default when none is saved: what the
    /// end of precision mode writes.
    fn restored(self) -> SpeedTarget {
        match self {
            Self::Scaled(saved) => SpeedTarget::Scaled(saved.unwrap_or_default()),
            Self::Dpi(saved) => {
                SpeedTarget::Dpi(saved.map_or(DpiTarget::Default, DpiTarget::Value))
            }
        }
    }

    /// The saved value, `None` when nothing is saved: what a reconnect
    /// outside precision mode writes.
    const fn value(self) -> Option<SpeedTarget> {
        match self {
            Self::Scaled(Some(speed)) => Some(SpeedTarget::Scaled(speed)),
            Self::Dpi(Some(dpi)) => Some(SpeedTarget::Dpi(DpiTarget::Value(dpi))),
            Self::Scaled(None) | Self::Dpi(None) => None,
        }
    }

    /// Whether `other` is saved for a mouse of the same kind.
    const fn same_kind(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Scaled(_), Self::Scaled(_)) | (Self::Dpi(_), Self::Dpi(_))
        )
    }
}

/// What one pointer-speed write sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpeedTarget {
    /// A `0x2205` speed.
    Scaled(PointerSpeed),
    /// A sensor DPI.
    Dpi(DpiTarget),
}

/// Which mice a precision press acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrecisionPress {
    /// [`Action::TogglePrecisionMode`](openlogi_core::binding::Action::TogglePrecisionMode):
    /// every mouse whose speed OpenLogi sets.
    Assigned,
    /// [`Action::CycleDpiPresets`](openlogi_core::binding::Action::CycleDpiPresets):
    /// only a scaled mouse, which has no DPI to cycle. Its DPI button is its
    /// precision button.
    DpiButton,
}

/// One online, enabled mouse whose speed OpenLogi sets, as the orchestrator
/// publishes it.
pub(crate) struct SpeedMouse {
    /// The mouse's config key.
    pub(crate) key: String,
    /// Where the mouse is reached now.
    pub(crate) route: DeviceRoute,
    /// How it sets its speed, and the value saved for it.
    pub(crate) saved: SavedSpeed,
}

/// One pointer-speed write, numbered when it was requested.
pub(crate) struct SpeedWrite {
    /// The mouse to write.
    pub(crate) route: DeviceRoute,
    /// What to write.
    pub(crate) target: SpeedTarget,
    /// Its place among the mouse's pointer-speed writes.
    pub(crate) ticket: WriteTicket,
    /// The pending restore this write settles once it lands, if any. Pass
    /// it to [`PointerSpeeds::restored`] then.
    pub(crate) restore: Option<Restore>,
    /// What the mouse's DPI LED shows after this write: lit when the write
    /// sets the precision speed, dark when it ends it. `None` leaves the LED
    /// to the firmware, so a mouse that never used precision mode keeps its
    /// own LED behavior.
    pub(crate) led: Option<bool>,
}

/// A [`SpeedWrite`] before its target is known.
pub(crate) struct SpeedPlace {
    /// The mouse to write.
    pub(crate) route: DeviceRoute,
    /// Its place among the mouse's pointer-speed writes.
    pub(crate) ticket: WriteTicket,
    /// The pending restore the write settles once it lands, if any.
    pub(crate) restore: Option<Restore>,
    /// What the mouse's DPI LED shows after the write; see
    /// [`SpeedWrite::led`].
    pub(crate) led: Option<bool>,
}

impl SpeedPlace {
    /// The write of `target` at this place.
    pub(crate) fn aimed(self, target: SpeedTarget) -> SpeedWrite {
        let Self {
            route,
            ticket,
            restore,
            led,
        } = self;
        SpeedWrite {
            route,
            target,
            ticket,
            restore,
            led,
        }
    }
}

impl SpeedWrite {
    /// The DPI this write sets, when it names one.
    pub(crate) fn dpi(&self) -> Option<Dpi> {
        match self.target {
            SpeedTarget::Dpi(DpiTarget::Value(dpi)) => Some(dpi),
            _ => None,
        }
    }
}

/// A write of the settings app, numbered when it was requested.
pub(crate) struct AppWrite {
    /// Its place among the mouse's pointer-speed writes.
    pub(crate) ticket: WriteTicket,
    /// The restore the write settles once it lands, when it ends the
    /// precision speed of the mouse: its mode was on, or a restore was owed.
    /// The DPI LED goes dark with the write then. Until it lands, a newer
    /// write that supersedes it carries the restore, and with it the LED.
    pub(crate) restore: Option<Restore>,
}

/// A pending restore a reconnect write carries: the mouse's config key and
/// the generation of its entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Restore {
    key: String,
    generation: u64,
}

/// A DPI shift's restore write: its place, and the pending restore it
/// settles once it lands.
pub(crate) struct ShiftRestore {
    pub(crate) ticket: WriteTicket,
    pub(crate) restore: Option<Restore>,
}

/// What a config reload changed for one mouse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReloadChange {
    /// The saved speed or DPI differs.
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
        mice: impl IntoIterator<Item = SpeedMouse>,
    ) {
        let mut state = self.lock();
        state.selected = selected.map(str::to_owned);
        state.online = mice
            .into_iter()
            .map(|mouse| {
                let SpeedMouse { key, route, saved } = mouse;
                (key, OnlineMouse { route, saved })
            })
            .collect();
    }

    /// Turn precision mode on or off for the mouse `key` names, or for the
    /// selected device when `key` is `None`. Returns whether the mode is now
    /// on and the write that applies it, or `None` when that device is not
    /// an online mouse that `press` acts on.
    pub(crate) fn toggle_precision(
        &self,
        key: Option<&str>,
        press: PrecisionPress,
    ) -> Option<(bool, SpeedWrite)> {
        let mut state = self.lock();
        let key = key.or(state.selected.as_deref())?.to_owned();
        let mouse = state.online.get(&key)?;
        let (route, saved) = (mouse.route.clone(), mouse.saved);
        if press == PrecisionPress::DpiButton && !matches!(saved, SavedSpeed::Scaled(_)) {
            return None;
        }
        if state.precision.remove(&key) {
            // The mouse stays owed its speed until this write lands.
            let restore = state.owe_restore(&key);
            let write = self.write(route, saved.restored(), Some(restore));
            Some((false, write))
        } else {
            state.restore.remove(&key);
            state.precision.insert(key);
            Some((true, self.precision_write(route, saved)))
        }
    }

    /// The write a reconnect owes the mouse `key` names, now at `route`: the
    /// precision speed while the mode is on, else the `saved` speed or DPI.
    /// A mouse whose mode ended gets its default when nothing is saved, on
    /// every reconnect pass until [`Self::restored`] reports that a write of
    /// its speed landed. `None` when there is nothing to write.
    ///
    /// The write is numbered like every other pointer-speed write, so a
    /// precision press or a settings-app write made while it waits for the
    /// mouse supersedes it. The volatile reapply therefore leaves out the DPI
    /// of a mouse this returns a write for.
    pub(crate) fn reapply(
        &self,
        key: &str,
        route: &DeviceRoute,
        saved: SavedSpeed,
    ) -> Option<SpeedWrite> {
        let state = self.lock();
        if state.precision.contains(key) {
            return Some(self.precision_write(route.clone(), saved));
        }
        let Some(restore) = state.pending_restore(key) else {
            return saved
                .value()
                .map(|target| self.write(route.clone(), target, None));
        };
        Some(self.write(route.clone(), saved.restored(), Some(restore)))
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

    /// The settings app writes `saved`, a speed or a DPI, to the mouse at
    /// `route`. Ends the precision mode of that mouse when `saved` is of its
    /// kind, makes `saved` the value precision mode returns to until the next
    /// reload, and numbers the write.
    ///
    /// A mouse whose precision speed the write ends stays owed a restore
    /// until a write lands, as after a press: a newer write that supersedes
    /// this one then settles it and darkens the LED.
    pub(crate) fn app_write(&self, route: &DeviceRoute, saved: SavedSpeed) -> AppWrite {
        let mut state = self.lock();
        let keys: Vec<String> = state
            .online
            .iter()
            .filter(|(_, mouse)| mouse.route == *route && mouse.saved.same_kind(saved))
            .map(|(key, _)| key.clone())
            .collect();
        let mut restore = None;
        for key in keys {
            let was_on = state.precision.remove(&key);
            if was_on || state.restore.contains_key(&key) {
                restore = Some(state.owe_restore(&key));
            }
            if let Some(mouse) = state.online.get_mut(&key) {
                mouse.saved = saved;
            }
        }
        AppWrite {
            ticket: self.order.request(route),
            restore,
        }
    }

    /// A place for a DPI shift's restore among the pointer-speed writes of
    /// the mouse `key` names, at `route`. `own` is the restore the shift's
    /// own write carries, from ending precision mode as it began; the
    /// restore takes it over, since its place skips that write if it still
    /// waits. `None` while precision mode is on or a newer end of it is
    /// owed: that write sets the DPI, and a later place would skip it.
    pub(crate) fn shift_restore(
        &self,
        key: &str,
        route: &DeviceRoute,
        own: Option<&Restore>,
    ) -> Option<ShiftRestore> {
        let state = self.lock();
        let restore = state.pending_restore(key);
        if state.precision.contains(key) || restore.is_some() && restore.as_ref() != own {
            return None;
        }
        Some(ShiftRestore {
            ticket: self.order.request(route),
            restore,
        })
    }

    /// The write the mouse `key` names owes after its session entered host
    /// mode again, which loads the stored profile's DPI: the one a reconnect
    /// owes.
    pub(crate) fn host_mode_write(&self, key: &str) -> Option<SpeedWrite> {
        let (route, saved) = {
            let state = self.lock();
            let mouse = state.online.get(key)?;
            (mouse.route.clone(), mouse.saved)
        };
        self.reapply(key, &route, saved)
    }

    /// A DPI preset action writes `dpi` to the mouse at `route`. Ends the
    /// precision mode of a DPI mouse there and numbers the write. The write
    /// settles the restore that mouse is owed, as a write of its own speed
    /// does, but leaves its saved DPI as it is.
    pub(crate) fn preset_write(&self, route: &DeviceRoute, dpi: Dpi) -> SpeedWrite {
        self.preset_place(route)
            .aimed(SpeedTarget::Dpi(DpiTarget::Value(dpi)))
    }

    /// The place, restore and LED of a [`Self::preset_write`] whose DPI the
    /// write picks once it has read the mouse.
    pub(crate) fn preset_place(&self, route: &DeviceRoute) -> SpeedPlace {
        let mut state = self.lock();
        let key = state
            .online
            .iter()
            .find(|(_, mouse)| mouse.route == *route && matches!(mouse.saved, SavedSpeed::Dpi(_)))
            .map(|(key, _)| key.clone());
        let restore = key.and_then(|key| {
            if state.precision.remove(&key) {
                Some(state.owe_restore(&key))
            } else {
                state.pending_restore(&key)
            }
        });
        self.place(route.clone(), restore)
    }

    /// The writes a config reload owes. `change` tells what the reload
    /// changed for a config key, and `mice` are the online mice.
    ///
    /// A changed speed or DPI, or changed assignments, end precision mode on
    /// every mouse, online or not, as in Logi Options+. A mouse whose mode
    /// ended returns to its saved speed, or to its default when none is
    /// saved: an online mouse now, and every mouse at its reconnects until
    /// one such write lands. An online scaled mouse whose speed alone changed
    /// gets the new speed. An online DPI mouse whose DPI alone changed gets a
    /// write only when it is owed a restore: a reload never wrote a changed
    /// DPI before precision mode, and still does not. The other mice get no
    /// write.
    pub(crate) fn reload(
        &self,
        change: impl Fn(&str) -> ReloadChange,
        mice: impl IntoIterator<Item = SpeedMouse>,
    ) -> Vec<SpeedWrite> {
        let mut state = self.lock();
        let mut ended: HashSet<String> = state
            .precision
            .extract_if(|key| change(key).ends_precision())
            .collect();
        // The writes are numbered while the lock is held, as every write is.
        let writes = mice
            .into_iter()
            .filter_map(|SpeedMouse { key, route, saved }| {
                let (target, restore) = if ended.remove(&key) {
                    (saved.restored(), Some(state.owe_restore(&key)))
                } else if change(&key).speed {
                    // A pending restore rides on this write: the default
                    // when the value was removed, since the mouse may still
                    // be slow.
                    match (state.pending_restore(&key), saved) {
                        (Some(restore), saved) => (saved.restored(), Some(restore)),
                        (None, SavedSpeed::Scaled(speed)) => (SpeedTarget::Scaled(speed?), None),
                        (None, SavedSpeed::Dpi(_)) => return None,
                    }
                } else {
                    return None;
                };
                Some(self.write(route, target, restore))
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

    /// A write of `target` that settles `restore`, if any. A write that
    /// settles a restore ends the precision speed, so it darkens the LED.
    fn write(
        &self,
        route: DeviceRoute,
        target: SpeedTarget,
        restore: Option<Restore>,
    ) -> SpeedWrite {
        self.place(route, restore).aimed(target)
    }

    /// The next place among `route`'s pointer-speed writes, for a write that
    /// settles `restore`. Settling one ends precision mode, so the LED goes
    /// dark.
    fn place(&self, route: DeviceRoute, restore: Option<Restore>) -> SpeedPlace {
        let ticket = self.order.request(&route);
        let led = restore.as_ref().map(|_| false);
        SpeedPlace {
            route,
            ticket,
            restore,
            led,
        }
    }

    /// A write of the precision speed of a mouse with `saved`. It lights the
    /// LED.
    fn precision_write(&self, route: DeviceRoute, saved: SavedSpeed) -> SpeedWrite {
        let ticket = self.order.request(&route);
        SpeedWrite {
            route,
            target: saved.precision(),
            ticket,
            restore: None,
            led: Some(true),
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
