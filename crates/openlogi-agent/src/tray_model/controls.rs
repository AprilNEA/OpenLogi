//! What a device's submenu can change, and what each item says: decided from
//! the device's capabilities, the config, and what the agent last read back
//! from the device.
//!
//! Reading a device is the tray's job, off the main thread; here a reading is
//! data. A submenu shows the last reading it has — kept between openings — so
//! it opens at once, and the tray rebuilds it when a fresh one lands.
//!
//! DPI, the wheel and whether OpenLogi manages a device are saved in
//! `config.toml`, so they change offline too: the agent writes them when the
//! device reconnects. Everything else is only on the device, and greys out
//! while it is offline or until it has been read.

use openlogi_agent_core::orchestrator::SettingError;
use openlogi_core::brand::DeeplinkCommand;
use openlogi_core::config::{
    ConfigError, DeviceEditError, DisableKey, SMARTSHIFT_AUTO_DISENGAGE_DEFAULT, SmartShift,
};
use openlogi_core::device::LightCapabilities;
use openlogi_core::hid::{
    BacklightChange, BacklightState, DisableKeysMask, DisableKeysState, Dpi, LightCommand,
    SidetoneLevel, SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus, WriteError,
};
use rust_i18n::t;

use super::{DeviceRow, DeviceState};

/// The sidetone levels the submenu offers, in percent.
const SIDETONE_LEVELS: [u8; 5] = [0, 25, 50, 75, 100];
/// The brightness levels the submenu offers a light, in percent of its range.
const LIGHT_BRIGHTNESS_LEVELS: [u8; 4] = [25, 50, 75, 100];

/// What a device's submenu offers to change, and what the config holds for
/// it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per device capability, as `Capabilities` has"
)]
pub struct Controls {
    /// The DPI choices, for a device with adjustable DPI.
    pub dpi: Option<DpiControl>,
    /// Whether the device may have a SmartShift wheel, which only reading it
    /// settles.
    pub wheel: bool,
    /// The wheel the config saved for the device's current link.
    pub saved_wheel: Option<SmartShiftStatus>,
    /// Whether OpenLogi manages the device, for a mouse or keyboard.
    pub managed: Option<bool>,
    /// Whether the keyboard has Fn lock.
    pub fn_lock: bool,
    /// The Fn lock the config saved.
    pub saved_fn_lock: Option<bool>,
    /// Whether the keyboard's backlight can be switched.
    pub backlight: bool,
    /// Whether the keyboard can disable some of its keys.
    pub disable_keys: bool,
    /// Whether the headset plays the microphone back.
    pub sidetone: bool,
    /// The controls of a standalone light.
    pub light: Option<LightCapabilities>,
}

impl Controls {
    /// Whether any change is saved to apply at the next reconnect.
    fn saves_offline(&self) -> bool {
        self.dpi.is_some() || self.wheel || self.managed.is_some()
    }
}

/// A device's DPI choices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DpiControl {
    /// The user's presets, in their order; empty when they set none.
    pub presets: Vec<Dpi>,
    /// The DPI the config saved for the device's current link.
    pub saved: Option<Dpi>,
}

/// What the tray remembers of one device between openings of its submenu.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceMemory {
    /// What the device last answered.
    pub reading: Reading,
    /// Why the last change made from the menu did not take, until the
    /// submenu has shown it.
    pub failure: Option<Failure>,
    phase: ReadPhase,
}

/// Whether a read of a device is under way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ReadPhase {
    #[default]
    Idle,
    /// A read is under way. `changed` holds the values changes from the
    /// menu set since it began, which its answer leaves alone.
    Reading { changed: Reads },
}

impl DeviceMemory {
    /// Start a read of the device, unless one is under way.
    pub fn start_reading(&mut self) -> bool {
        if self.reading_now() {
            return false;
        }
        self.phase = ReadPhase::Reading {
            changed: Reads::default(),
        };
        true
    }

    /// Whether a read of the device is under way.
    #[must_use]
    pub fn reading_now(&self) -> bool {
        matches!(self.phase, ReadPhase::Reading { .. })
    }

    /// Take what the read of `reads` answered. A value a change set while
    /// it ran stays: the read may have reached the device before the change.
    /// An answer with no read under way is not taken.
    pub fn finish_reading(&mut self, reads: Reads, fresh: Reading) {
        if let ReadPhase::Reading { changed } = std::mem::take(&mut self.phase) {
            self.reading.merge(reads.without(changed), fresh);
        }
    }

    /// Take what a change made from the menu came to: the change the device
    /// and config took, `None` when a newer write superseded it — the next
    /// read shows where that write left the device — or why it failed.
    pub fn changed(&mut self, outcome: Result<Option<Change>, Failure>) {
        match outcome {
            Ok(change) => {
                if let Some(change) = change {
                    self.reading.record(change);
                    if let ReadPhase::Reading { changed } = &mut self.phase {
                        *changed = changed.or(change.reads());
                    }
                }
                self.failure = None;
            }
            Err(failure) => self.failure = Some(failure),
        }
    }
}

/// What the agent last read back from one device. Each value outlives the
/// submenu it was read for, so the next opening shows it at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reading {
    /// Its DPI.
    pub dpi: Read<Dpi>,
    /// Its SmartShift wheel.
    pub wheel: Read<SmartShiftStatus>,
    /// Its Fn lock.
    pub fn_lock: Read<bool>,
    /// Its keyboard backlight.
    pub backlight: Read<BacklightState>,
    /// The keys it can disable and has disabled.
    pub disabled_keys: Read<DisableKeysState>,
    /// Its sidetone.
    pub sidetone: Read<SidetoneLevel>,
}

impl Reading {
    /// Take the values `fresh` holds for `reads`, keeping the rest.
    pub fn merge(&mut self, reads: Reads, fresh: Self) {
        fn take<T>(wanted: bool, old: &mut Read<T>, new: Read<T>) {
            if wanted {
                *old = new;
            }
        }
        take(reads.dpi, &mut self.dpi, fresh.dpi);
        take(reads.wheel, &mut self.wheel, fresh.wheel);
        take(reads.fn_lock, &mut self.fn_lock, fresh.fn_lock);
        take(reads.backlight, &mut self.backlight, fresh.backlight);
        take(
            reads.disabled_keys,
            &mut self.disabled_keys,
            fresh.disabled_keys,
        );
        take(reads.sidetone, &mut self.sidetone, fresh.sidetone);
    }

    /// Take a change the device and config just took, so the next opening
    /// shows it before the device is read again.
    pub fn record(&mut self, change: Change) {
        match change {
            Change::Dpi(dpi) => self.dpi = Read::Value(dpi),
            Change::Wheel(status) => self.wheel = Read::Value(status),
            Change::FnLock(on) => self.fn_lock = Read::Value(on),
            Change::Backlight(change) => {
                if let Read::Value(state) = self.backlight {
                    self.backlight = Read::Value(state.changed(change));
                }
            }
            Change::DisabledKeys(disabled) => {
                if let Read::Value(state) = &mut self.disabled_keys {
                    state.disabled = disabled & state.supported;
                }
            }
            Change::Sidetone(level) => self.sidetone = Read::Value(level),
            // The snapshot and the reloaded config carry these.
            Change::Managed(_) | Change::Light(_) => {}
        }
    }
}

/// One value read from a device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Read<T> {
    /// Not read yet.
    #[default]
    Unread,
    /// What the device answered.
    Value(T),
    /// The device lacks the feature.
    Unsupported,
    /// The device did not answer.
    Failed,
}

impl<T> Read<T> {
    /// What a read's `result` tells the menu.
    #[must_use]
    pub fn of(result: Result<T, WriteError>) -> Self {
        match result {
            Ok(value) => Self::Value(value),
            Err(WriteError::FeatureUnsupported { .. }) => Self::Unsupported,
            Err(_) => Self::Failed,
        }
    }

    fn value(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Unread | Self::Unsupported | Self::Failed => None,
        }
    }

    /// The same answer without its value, to compare answers alone.
    fn state(self) -> Read<()> {
        match self {
            Self::Unread => Read::Unread,
            Self::Value(_) => Read::Value(()),
            Self::Unsupported => Read::Unsupported,
            Self::Failed => Read::Failed,
        }
    }
}

/// The reads a submenu asks of its device when it opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent flag per HID++ read, mirroring `Reading`"
)]
pub struct Reads {
    /// Its DPI.
    pub dpi: bool,
    /// Its SmartShift wheel.
    pub wheel: bool,
    /// Its Fn lock.
    pub fn_lock: bool,
    /// Its keyboard backlight.
    pub backlight: bool,
    /// Its disabled keys.
    pub disabled_keys: bool,
    /// Its sidetone.
    pub sidetone: bool,
}

impl Reads {
    /// Whether there is anything to read.
    #[must_use]
    pub const fn any(self) -> bool {
        self.dpi
            || self.wheel
            || self.fn_lock
            || self.backlight
            || self.disabled_keys
            || self.sidetone
    }

    /// The reads in either.
    const fn or(self, other: Self) -> Self {
        Self {
            dpi: self.dpi || other.dpi,
            wheel: self.wheel || other.wheel,
            fn_lock: self.fn_lock || other.fn_lock,
            backlight: self.backlight || other.backlight,
            disabled_keys: self.disabled_keys || other.disabled_keys,
            sidetone: self.sidetone || other.sidetone,
        }
    }

    /// These reads, except those in `other`.
    const fn without(self, other: Self) -> Self {
        Self {
            dpi: self.dpi && !other.dpi,
            wheel: self.wheel && !other.wheel,
            fn_lock: self.fn_lock && !other.fn_lock,
            backlight: self.backlight && !other.backlight,
            disabled_keys: self.disabled_keys && !other.disabled_keys,
            sidetone: self.sidetone && !other.sidetone,
        }
    }
}

impl Change {
    /// The read whose value this change sets.
    fn reads(self) -> Reads {
        let mut reads = Reads::default();
        match self {
            Self::Dpi(_) => reads.dpi = true,
            Self::Wheel(_) => reads.wheel = true,
            Self::FnLock(_) => reads.fn_lock = true,
            Self::Backlight(_) => reads.backlight = true,
            Self::DisabledKeys(_) => reads.disabled_keys = true,
            Self::Sidetone(_) => reads.sidetone = true,
            Self::Managed(_) | Self::Light(_) => {}
        }
        reads
    }
}

/// A change a submenu item makes to its device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Set the DPI and save it.
    Dpi(Dpi),
    /// Set the SmartShift wheel and save it.
    Wheel(SmartShiftStatus),
    /// Have OpenLogi manage the device, or leave it native, and save it.
    Managed(bool),
    /// Set Fn lock and save it.
    FnLock(bool),
    /// Change the keyboard backlight.
    Backlight(BacklightChange),
    /// Disable exactly these keys and save them.
    DisabledKeys(DisableKeysMask),
    /// Set the sidetone.
    Sidetone(SidetoneLevel),
    /// Have the light take the command and save it.
    Light(LightCommand),
}

/// One item of a device's submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A section title.
    Header(String),
    /// A line that only informs.
    Note(String),
    /// An item with a checkmark or none. Clicking makes `change`; an item
    /// with none is greyed out.
    Choice {
        /// What it says.
        title: String,
        /// Whether it shows a checkmark.
        checked: bool,
        /// What clicking it changes.
        change: Option<Change>,
    },
    /// An item that opens the settings app.
    Open {
        /// What it says.
        title: String,
        /// What it opens.
        link: DeeplinkCommand,
    },
    /// A line between groups.
    Separator,
}

/// Why a change made from the menu did not take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Another writer saved `config.toml` since it was read, so nothing was
    /// saved. Trying again reads the new file.
    Conflict,
    /// The device's current link has its own value for the setting, which
    /// a device-wide value would not replace.
    LinkOverride,
    /// The device did not take the change, or did not answer.
    Device,
    /// The change was not saved for another reason.
    NotSaved,
    /// The change was saved, but the agent did not reload it.
    NotReloaded,
}

impl Failure {
    /// What a [`SettingError`] means to the user.
    #[must_use]
    pub fn of(error: &SettingError) -> Self {
        match error {
            SettingError::Refused(DeviceEditError::LinkOverride) => Self::LinkOverride,
            SettingError::NotSaved(ConfigError::Conflict { .. }) => Self::Conflict,
            SettingError::UnknownDevice(_) | SettingError::Write(_) => Self::Device,
            SettingError::Refused(_) | SettingError::NotSaved(_) => Self::NotSaved,
            SettingError::NotReloaded(_) => Self::NotReloaded,
        }
    }

    /// The warning line the device's submenu shows.
    #[must_use]
    pub fn text(self) -> String {
        match self {
            Self::Conflict => t!("tray.change_conflict"),
            Self::LinkOverride => t!("tray.change_link_override"),
            Self::Device => t!("tray.change_device_failed"),
            Self::NotSaved => t!("tray.change_not_saved"),
            Self::NotReloaded => t!("tray.change_not_reloaded"),
        }
        .into_owned()
    }
}

/// Which of the three wheel behaviours `status` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wheel {
    /// Ratchet that releases into free spin past a speed.
    SmartShift,
    /// Ratchet that never releases.
    Ratchet,
    /// Free spin.
    Free,
}

impl Wheel {
    const ALL: [Self; 3] = [Self::SmartShift, Self::Ratchet, Self::Free];

    fn of(status: SmartShiftStatus) -> Self {
        match (status.mode, status.auto_disengage) {
            (SmartShiftMode::Free, _) => Self::Free,
            (SmartShiftMode::Ratchet, SmartShiftAutoDisengage::Permanent) => Self::Ratchet,
            (SmartShiftMode::Ratchet, SmartShiftAutoDisengage::Threshold(_)) => Self::SmartShift,
        }
    }

    /// `status` turned into this behaviour, keeping its torque and the speed
    /// SmartShift releases at — or the default speed, when `status` has none
    /// a config can hold.
    fn from(self, status: SmartShiftStatus) -> SmartShiftStatus {
        let status = SmartShiftStatus {
            auto_disengage: match status.auto_disengage {
                speed @ SmartShiftAutoDisengage::Threshold(_)
                    if SmartShift::accepts_auto_disengage(speed) =>
                {
                    speed
                }
                SmartShiftAutoDisengage::Threshold(_) | SmartShiftAutoDisengage::Permanent => {
                    SmartShiftAutoDisengage::Threshold(SMARTSHIFT_AUTO_DISENGAGE_DEFAULT)
                }
            },
            ..status
        };
        match self {
            Self::SmartShift => SmartShiftStatus {
                mode: SmartShiftMode::Ratchet,
                ..status
            },
            Self::Ratchet => SmartShiftStatus {
                mode: SmartShiftMode::Ratchet,
                auto_disengage: SmartShiftAutoDisengage::Permanent,
                ..status
            },
            Self::Free => SmartShiftStatus {
                mode: SmartShiftMode::Free,
                ..status
            },
        }
    }

    fn title(self) -> String {
        match self {
            Self::SmartShift => t!("pointer.smartshift"),
            Self::Ratchet => t!("pointer.ratchet"),
            Self::Free => t!("pointer.free_spin"),
        }
        .into_owned()
    }
}

/// What a key is called in the menu.
fn key_title(key: DisableKey) -> String {
    match key {
        DisableKey::CapsLock => t!("keyboard.caps_lock"),
        DisableKey::NumLock => t!("keyboard.num_lock"),
        DisableKey::ScrollLock => t!("keyboard.scroll_lock"),
        DisableKey::Insert => t!("keyboard.insert"),
        DisableKey::WindowsCommand if cfg!(target_os = "macos") => t!("keyboard.command"),
        DisableKey::WindowsCommand => t!("keyboard.windows"),
    }
    .into_owned()
}

impl DeviceRow {
    fn online(&self) -> bool {
        !matches!(self.state, DeviceState::Offline(_))
    }

    /// What its submenu reads from the device when it opens: nothing while
    /// it is offline.
    #[must_use]
    pub fn reads(&self) -> Reads {
        if !self.online() || self.route.is_none() {
            return Reads::default();
        }
        let controls = &self.controls;
        Reads {
            dpi: controls.dpi.is_some(),
            wheel: controls.wheel,
            fn_lock: controls.fn_lock,
            backlight: controls.backlight,
            disabled_keys: controls.disable_keys,
            sidetone: controls.sidetone,
        }
    }

    /// The items its submenu offers, as what the tray remembers of the
    /// device leaves them.
    #[must_use]
    pub fn entries(&self, memory: &DeviceMemory) -> Vec<Entry> {
        let reading = &memory.reading;
        let mut groups = vec![
            self.status(memory),
            self.dpi_group(reading),
            self.wheel_group(reading),
        ];
        if let Some(managed) = self.controls.managed {
            groups.push(vec![Entry::Choice {
                title: t!("tray.managed_by_openlogi").into_owned(),
                checked: managed,
                change: self.saved(Change::Managed(!managed)),
            }]);
        }
        groups.push(self.keyboard_group(reading));
        groups.push(self.disabled_keys_group(reading));
        groups.push(self.sidetone_group(reading));
        if let Some(capabilities) = self.controls.light {
            groups.extend(self.light_groups(capabilities));
        }

        let mut entries = Vec::new();
        for group in groups.into_iter().filter(|group| !group.is_empty()) {
            if !entries.is_empty() {
                entries.push(Entry::Separator);
            }
            entries.extend(group);
        }
        entries
    }

    /// `change` to a setting the config saves, which changes while the
    /// device is offline too.
    fn saved(&self, change: Change) -> Option<Change> {
        self.route.is_some().then_some(change)
    }

    /// `change` to a setting only the device holds, which needs it online.
    fn live(&self, change: Change) -> Option<Change> {
        (self.route.is_some() && self.online()).then_some(change)
    }

    /// The lines that say why the last change did not take, that changes
    /// wait for a reconnect, or how reading the device went.
    fn status(&self, memory: &DeviceMemory) -> Vec<Entry> {
        let reading = &memory.reading;
        let mut status = Vec::new();
        if let Some(failure) = memory.failure {
            status.push(Entry::Note(failure.text()));
        }
        if !self.online() && self.controls.saves_offline() {
            status.push(Entry::Note(t!("tray.offline_changes_saved").into_owned()));
        }
        let reads = self.reads();
        // What each read the submenu asks for last answered.
        let answers = [
            (reads.dpi, reading.dpi.state()),
            (reads.wheel, reading.wheel.state()),
            (reads.fn_lock, reading.fn_lock.state()),
            (reads.backlight, reading.backlight.state()),
            (reads.disabled_keys, reading.disabled_keys.state()),
            (reads.sidetone, reading.sidetone.state()),
        ];
        if memory.reading_now() && answers.contains(&(true, Read::Unread)) {
            status.push(Entry::Note(t!("tray.reading").into_owned()));
        } else if answers[2..].contains(&(true, Read::Failed)) {
            // DPI and the wheel fall back on the config; the rest grey out.
            status.push(Entry::Note(t!("tray.read_failed").into_owned()));
        }
        status
    }

    fn dpi_group(&self, reading: &Reading) -> Vec<Entry> {
        let Some(dpi) = &self.controls.dpi else {
            return Vec::new();
        };
        // The config holds what the agent writes at the next reconnect;
        // online, the mouse itself may have moved on with its DPI button.
        let read = if self.online() {
            reading.dpi.value()
        } else {
            None
        };
        let current = read.or(dpi.saved);
        let mut group = vec![Entry::Header(t!("pointer.dpi_presets").into_owned())];
        for &preset in &dpi.presets {
            group.push(Entry::Choice {
                title: t!("tray.dpi_value", dpi => preset).into_owned(),
                checked: current == Some(preset),
                change: self.saved(Change::Dpi(preset)),
            });
        }
        if let Some(current) = current.filter(|current| !dpi.presets.contains(current)) {
            group.push(Entry::Note(
                t!("tray.current_dpi", dpi => current).into_owned(),
            ));
        }
        if dpi.presets.is_empty() {
            group.push(Entry::Open {
                title: t!("tray.set_dpi_presets").into_owned(),
                link: self.settings.clone(),
            });
        }
        group
    }

    fn wheel_group(&self, reading: &Reading) -> Vec<Entry> {
        if !self.controls.wheel {
            return Vec::new();
        }
        let wheel = match reading.wheel {
            Read::Unsupported => None,
            Read::Value(status) if self.online() => Some(status),
            // Offline, the config's wheel is the one the agent writes back;
            // a wheel read earlier at least says it has one.
            read => self.controls.saved_wheel.or_else(|| read.value()),
        };
        let Some(status) = wheel else {
            return Vec::new();
        };
        let current = Wheel::of(status);
        let mut group = vec![Entry::Header(t!("pointer.wheel_mode").into_owned())];
        group.extend(Wheel::ALL.map(|choice| Entry::Choice {
            title: choice.title(),
            checked: choice == current,
            change: self.saved(Change::Wheel(choice.from(status))),
        }));
        group
    }

    fn keyboard_group(&self, reading: &Reading) -> Vec<Entry> {
        let controls = &self.controls;
        let mut group = Vec::new();
        if controls.fn_lock && reading.fn_lock != Read::Unsupported {
            let read = reading.fn_lock.value();
            group.push(Entry::Choice {
                title: t!("device.fn_lock").into_owned(),
                checked: read.or(controls.saved_fn_lock).unwrap_or(false),
                change: read.and_then(|on| self.live(Change::FnLock(!on))),
            });
        }
        if controls.backlight && reading.backlight.state() != Read::Unsupported {
            let read = reading.backlight.value();
            let offer = |change| {
                read.filter(|state| state.accepts(change))
                    .and_then(|_| self.live(Change::Backlight(change)))
            };
            let enabled = read.is_some_and(|state| state.enabled);
            let automatic = enabled && read.is_some_and(BacklightState::is_automatic);
            group.push(Entry::Choice {
                title: t!("device.backlight").into_owned(),
                checked: enabled,
                change: offer(BacklightChange::Enabled(!enabled)),
            });
            group.push(Entry::Choice {
                title: t!("device.automatic_brightness").into_owned(),
                checked: automatic,
                change: if enabled {
                    offer(BacklightChange::Automatic(!automatic))
                } else {
                    None
                },
            });
        }
        group
    }

    /// The keys the keyboard can disable, each toggled alone. Which keys it
    /// can is the keyboard's to say, so the group waits for a reading.
    fn disabled_keys_group(&self, reading: &Reading) -> Vec<Entry> {
        let Read::Value(keys) = reading.disabled_keys else {
            return Vec::new();
        };
        if !self.controls.disable_keys {
            return Vec::new();
        }
        let mut group = vec![Entry::Header(t!("keyboard.disabled_keys").into_owned())];
        for key in DisableKey::ALL {
            let mask = key.mask();
            if keys.supported.contains(mask) {
                let disabled = keys.disabled.contains(mask);
                group.push(Entry::Choice {
                    title: key_title(key),
                    checked: disabled,
                    change: self.live(Change::DisabledKeys(if disabled {
                        keys.disabled & !mask
                    } else {
                        keys.disabled | mask
                    })),
                });
            }
        }
        group
    }

    fn sidetone_group(&self, reading: &Reading) -> Vec<Entry> {
        if !self.controls.sidetone || reading.sidetone.state() == Read::Unsupported {
            return Vec::new();
        }
        let read = reading.sidetone.value();
        let mut group = vec![Entry::Header(t!("audio.sidetone").into_owned())];
        group.extend(SIDETONE_LEVELS.map(|percent| Entry::Choice {
            title: if percent == 0 {
                t!("common.off").into_owned()
            } else {
                t!("tray.percent", percent => percent).into_owned()
            },
            checked: read.is_some_and(|level| level.percent() == percent),
            change: read.and_then(|_| self.live(Change::Sidetone(SidetoneLevel::clamped(percent)))),
        }));
        group
    }

    /// A light's power, brightness and colour temperature, which change only
    /// while it is online and read back.
    fn light_groups(&self, capabilities: LightCapabilities) -> Vec<Vec<Entry>> {
        let light = match &self.state {
            DeviceState::Light(light) if self.route.is_some() => Some(*light),
            _ => None,
        };
        let on = light.is_some_and(|light| light.on);
        // Brightness and colour change a lit light only.
        let lit = |command| on.then_some(Change::Light(command));
        let mut groups = Vec::new();
        if capabilities.power {
            groups.push(vec![Entry::Choice {
                title: t!("tray.light_power").into_owned(),
                checked: on,
                change: light.map(|light| Change::Light(LightCommand::Power(!light.on))),
            }]);
        }
        if capabilities.brightness.is_some() {
            let mut group = vec![Entry::Header(t!("camera.brightness").into_owned())];
            group.extend(LIGHT_BRIGHTNESS_LEVELS.map(|percent| Entry::Choice {
                title: t!("tray.percent", percent => percent).into_owned(),
                checked: light.is_some_and(|light| light.brightness_percent == Some(percent)),
                change: lit(LightCommand::BrightnessPercent(percent)),
            }));
            groups.push(group);
        }
        if let Some(range) = capabilities.temperature {
            let mut group = vec![Entry::Header(
                t!("lighting.colour_temperature").into_owned(),
            )];
            let mut offered = Vec::new();
            for (kelvin, name) in [
                (6500, t!("tray.cool_light")),
                (4800, t!("tray.neutral_light")),
                (2700, t!("tray.warm_light")),
            ] {
                let kelvin = range.quantize(kelvin);
                if offered.contains(&kelvin) {
                    continue;
                }
                offered.push(kelvin);
                group.push(Entry::Choice {
                    title: t!("tray.light_temperature", name => name, kelvin => kelvin)
                        .into_owned(),
                    checked: light.is_some_and(|light| light.temperature_kelvin == Some(kelvin)),
                    change: lit(LightCommand::TemperatureKelvin(kelvin)),
                });
            }
            groups.push(group);
        }
        groups
    }
}
