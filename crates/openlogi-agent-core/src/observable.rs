//! The agent's observable state: one cell holding everything the GUI can see.
//!
//! Every fact in [`AgentSnapshot`] already has an event source inside the
//! agent — the inventory watcher, the camera watcher, the accessibility
//! watcher, a config reload, the hook being installed or dropped. Those edges
//! used to stop at the process boundary: the IPC server recomposed its answer
//! from five orchestrator accessors plus a fresh `AXIsProcessTrusted()` call on
//! every request, so a reader could only learn *whether* anything had changed
//! by asking again. Holding the composed value here keeps the edges as well:
//! a write that changes nothing notifies nobody, so a reader can be told
//! *when* to look instead of resampling on a timer.
//!
//! The cell has more than one writer — [`Orchestrator`](crate::orchestrator::Orchestrator)
//! for the device and config facts, the agent binary for the hook ones — so it
//! is shared as an `Arc` and every setter takes `&self`.

use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_hook::Hook;
use openlogi_ipc::{AgentSnapshot, AgentStatus, InventoryHealth, PROTOCOL_VERSION};
use tokio::sync::watch;

/// The agent's observable state, and the notification that it changed.
pub struct ObservableState {
    tx: watch::Sender<AgentSnapshot>,
}

impl ObservableState {
    /// Seed the cell for a starting agent: nothing enumerated yet, no hook, and
    /// the Accessibility trust this process currently holds.
    ///
    /// `agent_version` comes from the binary because only the binary knows
    /// which version is serving. `launch_at_login` starts `false` and is
    /// republished by [`Orchestrator::new`](crate::orchestrator::Orchestrator::new)
    /// from the loaded config, which runs before the IPC socket is bound — no
    /// reader can observe the placeholder.
    #[must_use]
    pub fn new(agent_version: String) -> Self {
        let (tx, _) = watch::channel(AgentSnapshot {
            status: AgentStatus {
                accessibility_granted: Hook::has_accessibility(),
                hook_installed: false,
                launch_at_login: false,
                inventory: InventoryHealth::Scanning,
                protocol_version: PROTOCOL_VERSION,
                agent_version,
            },
            inventory: Vec::new(),
            standalone: Vec::new(),
            camera_active: false,
        });
        Self { tx }
    }

    /// Clone the whole current state.
    #[must_use]
    pub fn snapshot(&self) -> AgentSnapshot {
        self.tx.borrow().clone()
    }

    /// Read part of the current state without cloning the rest. The closure
    /// runs under the cell's read lock, so it must not block or await.
    pub fn read<R>(&self, read: impl FnOnce(&AgentSnapshot) -> R) -> R {
        let state = self.tx.borrow();
        read(&state)
    }

    /// Observe changes. The receiver starts out seeing the current value as
    /// already delivered, and is notified only when a write actually changes
    /// something.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<AgentSnapshot> {
        self.tx.subscribe()
    }

    /// Publish where enumeration stands together with the device set it
    /// produced, so the two can never be read from different generations.
    ///
    /// The inventory watcher re-enumerates on a timer, so most calls carry the
    /// same devices as the last one; those notify nobody.
    pub fn set_inventory(
        &self,
        health: InventoryHealth,
        inventories: &[DeviceInventory],
        standalone: &[StandaloneDevice],
    ) {
        self.tx.send_if_modified(|state| {
            if state.status.inventory == health
                && state.inventory == inventories
                && state.standalone == standalone
            {
                return false;
            }
            state.status.inventory = health;
            state.inventory = inventories.to_vec();
            state.standalone = standalone.to_vec();
            true
        });
    }

    /// Publish the latest aggregate camera-use sample.
    pub fn set_camera_active(&self, active: bool) {
        self.tx.send_if_modified(|state| {
            if state.camera_active == active {
                return false;
            }
            state.camera_active = active;
            true
        });
    }

    /// Publish the autostart state the current config asks for.
    pub fn set_launch_at_login(&self, enabled: bool) {
        self.tx.send_if_modified(|state| {
            if state.status.launch_at_login == enabled {
                return false;
            }
            state.status.launch_at_login = enabled;
            true
        });
    }

    /// Publish an Accessibility trust change, as observed by
    /// [`watchers::accessibility`](crate::watchers::accessibility).
    pub fn set_accessibility_granted(&self, granted: bool) {
        self.tx.send_if_modified(|state| {
            if state.status.accessibility_granted == granted {
                return false;
            }
            state.status.accessibility_granted = granted;
            true
        });
    }

    /// Publish whether the OS input hook is currently installed.
    pub fn set_hook_installed(&self, installed: bool) {
        self.tx.send_if_modified(|state| {
            if state.status.hook_installed == installed {
                return false;
            }
            state.status.hook_installed = installed;
            true
        });
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "unwrap is idiomatic in tests; `has_changed` can only fail once the sender is dropped, and these tests hold it"
)]
mod tests {
    use super::ObservableState;
    use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
    use openlogi_hid::DIRECT_DEVICE_INDEX;
    use openlogi_ipc::InventoryHealth;

    fn state() -> ObservableState {
        ObservableState::new("test".to_string())
    }

    /// One directly attached mouse, `online` being the only thing a caller varies.
    fn inventory(online: bool) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "MX Master 3S".to_string(),
                vendor_id: 0x046d,
                product_id: 0xb023,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: DIRECT_DEVICE_INDEX,
                codename: Some("MX Master 3S".to_string()),
                wpid: None,
                kind: DeviceKind::Mouse,
                online,
                battery: None,
                model_info: None,
                capabilities: None,
            }],
        }
    }

    #[test]
    fn a_repeated_enumeration_notifies_nobody() {
        let state = state();
        let mut rx = state.subscribe();

        state.set_inventory(InventoryHealth::Ready, &[inventory(true)], &[]);
        assert!(rx.has_changed().unwrap(), "the first enumeration is news");
        rx.mark_unchanged();

        // What the inventory watcher does every couple of seconds on a steady desk.
        state.set_inventory(InventoryHealth::Ready, &[inventory(true)], &[]);
        assert!(
            !rx.has_changed().unwrap(),
            "an identical enumeration must not wake a reader"
        );
    }

    #[test]
    fn a_device_waking_inside_an_otherwise_identical_set_is_news() {
        let state = state();
        let mut rx = state.subscribe();
        state.set_inventory(InventoryHealth::Ready, &[inventory(false)], &[]);
        rx.mark_unchanged();

        state.set_inventory(InventoryHealth::Ready, &[inventory(true)], &[]);
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn a_completed_scan_that_found_nothing_moves_health_alone() {
        let state = state();
        let rx = state.subscribe();

        // "Checked, no devices" differs from "not checked yet" only in health —
        // the distinction the GUI's empty state reads.
        state.set_inventory(InventoryHealth::Ready, &[], &[]);
        assert!(rx.has_changed().unwrap());
        let snapshot = state.snapshot();
        assert_eq!(snapshot.status.inventory, InventoryHealth::Ready);
        assert!(snapshot.inventory.is_empty());
    }

    #[test]
    fn a_hook_write_leaves_the_device_facts_alone() {
        let state = state();
        state.set_inventory(InventoryHealth::Ready, &[inventory(true)], &[]);

        state.set_hook_installed(true);
        state.set_accessibility_granted(true);

        let snapshot = state.snapshot();
        assert!(snapshot.status.hook_installed);
        assert!(snapshot.status.accessibility_granted);
        assert_eq!(snapshot.inventory.len(), 1);
        assert_eq!(snapshot.status.inventory, InventoryHealth::Ready);
    }

    #[test]
    fn an_unchanged_flag_notifies_nobody() {
        let state = state();
        state.set_hook_installed(true);
        let rx = state.subscribe();

        state.set_hook_installed(true);
        assert!(!rx.has_changed().unwrap());

        state.set_hook_installed(false);
        assert!(rx.has_changed().unwrap());
    }
}
