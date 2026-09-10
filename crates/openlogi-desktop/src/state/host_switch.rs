//! Which pointing devices follow a keyboard's Easy-Switch.

use tracing::debug;

use openlogi_core::device::DeviceKind;

use crate::state::devices::DeviceRecord;

use super::AppState;

/// One pointing device offered as a follower of the active keyboard.
pub struct HostSwitchFollower {
    /// Persistent config key: the value stored in `host_switch_targets`.
    pub key: String,
    /// User-facing name, the user's alias included.
    pub name: String,
    /// Whether it currently follows the active keyboard.
    pub follows: bool,
    /// Whether the device is reachable right now. A follower that is asleep or
    /// sitting on another host still belongs in the list, unchanged: hiding it
    /// would read as having been unlinked.
    pub online: bool,
}

impl AppState {
    /// Whether the active device can lead a host switch.
    ///
    /// The relationship is keyboard-initiated, so only a keyboard leads, and
    /// only one whose settings can be persisted at all.
    #[must_use]
    pub fn current_device_leads_host_switch(&self) -> bool {
        self.current_record().is_some_and(|record| {
            record.kind == DeviceKind::Keyboard && record.persistent_config_key().is_some()
        })
    }

    /// Whether `kind` is something a user points with, and so something that
    /// belongs on the list of devices a keyboard can take along.
    fn follows_a_keyboard(kind: DeviceKind) -> bool {
        matches!(
            kind,
            DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad
        )
    }

    /// Pointing devices that can follow the active keyboard, each flagged with
    /// whether it currently does. Empty when the active device leads nothing.
    #[must_use]
    pub fn host_switch_followers(&self) -> Vec<HostSwitchFollower> {
        if !self.current_device_leads_host_switch() {
            return Vec::new();
        }
        let Some(leader) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
        else {
            return Vec::new();
        };
        let configured = self.config.host_switch_targets(leader);
        let mut followers: Vec<HostSwitchFollower> = self
            .devices
            .records
            .iter()
            .filter(|record| Self::follows_a_keyboard(record.kind))
            .filter_map(|record| {
                let key = record.persistent_config_key()?;
                (key != leader).then(|| HostSwitchFollower {
                    key: key.to_string(),
                    name: record.display_name.clone(),
                    follows: configured.iter().any(|target| target == key),
                    online: record.online,
                })
            })
            .collect();
        // A device is only in `records` while some route reaches it, so pulling
        // its receiver drops it entirely rather than leaving it there offline. A
        // list built from live devices alone would then lose the row while the
        // link stayed in the file: still followed, invisible, and impossible to
        // undo. Configured keys keep their place, named from the identity the
        // config remembers from when the device was last seen.
        for key in configured {
            if followers.iter().any(|follower| &follower.key == key) {
                continue;
            }
            followers.push(HostSwitchFollower {
                key: key.clone(),
                name: self
                    .config
                    .devices
                    .get(key)
                    .and_then(|device| device.identity.as_ref())
                    .map_or_else(|| key.clone(), |identity| identity.display_name.clone()),
                follows: true,
                online: false,
            });
        }
        followers
    }

    /// Add or remove one follower of the active keyboard and persist it.
    ///
    /// No-op when the active device leads nothing, so a stale click on a page
    /// that has since switched devices cannot write a link the user never saw.
    pub fn commit_host_switch_follower(&mut self, follower_key: &str, follows: bool) {
        let Some(leader) = self
            .current_record()
            .filter(|_| self.current_device_leads_host_switch())
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("active device leads no host switch — follower change ignored");
            return;
        };
        let mut targets: Vec<String> = self.config.host_switch_targets(&leader).to_vec();
        if follows {
            if targets.iter().any(|target| target == follower_key) {
                return;
            }
            targets.push(follower_key.to_string());
        } else {
            let before = targets.len();
            targets.retain(|target| target != follower_key);
            if targets.len() == before {
                return;
            }
        }
        self.config
            .edit(|config| config.set_host_switch_targets(&leader, targets));
        self.persist_and_reload("host switch followers");
    }
}
