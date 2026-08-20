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
use openlogi_ipc::{
    AgentSnapshot, AgentStatus, FoundDevice, Generation, InventoryHealth, OBSERVE_HOLD,
    Observation, PROTOCOL_VERSION, PairingPhase,
};
use tokio::sync::watch;

/// The agent's observable state, and the notification that it changed.
pub struct ObservableState {
    tx: watch::Sender<Observation>,
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
        // Generation 1, not 0: 0 is the client sentinel for "I have seen
        // nothing", so a first `observe(0)` must differ from whatever is here.
        let (tx, _) = watch::channel(Observation {
            generation: 1,
            snapshot: AgentSnapshot {
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
                pairing: None,
            },
        });
        Self { tx }
    }

    /// Clone the whole current state.
    #[must_use]
    pub fn snapshot(&self) -> AgentSnapshot {
        self.tx.borrow().snapshot.clone()
    }

    /// Read part of the current state without cloning the rest. The closure
    /// runs under the cell's read lock, so it must not block or await.
    pub fn read<R>(&self, read: impl FnOnce(&AgentSnapshot) -> R) -> R {
        let state = self.tx.borrow();
        read(&state.snapshot)
    }

    /// Observe changes. The receiver starts out seeing the current value as
    /// already delivered, and is notified only when a write actually changes
    /// something.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Observation> {
        self.tx.subscribe()
    }

    /// Serve one [`Agent::observe`](openlogi_ipc::Agent::observe): wait until
    /// the state differs from `since`, then answer with all of it. With nothing
    /// to report the hold elapses and the caller gets the unchanged state, which
    /// is how it learns the agent is still alive.
    pub async fn observe(&self, since: Generation) -> Observation {
        let mut rx = self.tx.subscribe();
        let changed = rx.wait_for(|state| state.generation != since);
        match tokio::time::timeout(OBSERVE_HOLD, changed).await {
            Ok(Ok(state)) => state.clone(),
            // The hold elapsed, or every sender is gone (the agent is shutting
            // down). Answer with what we have; the caller compares generations.
            Ok(Err(_)) | Err(_) => self.tx.borrow().clone(),
        }
    }

    /// Apply `change`, and stamp the next generation if it reported a real
    /// difference. A change that reports `false` notifies nobody — that is what
    /// lets a reader block on this cell instead of resampling it.
    fn update(&self, change: impl FnOnce(&mut AgentSnapshot) -> bool) {
        self.tx.send_if_modified(|state| {
            if !change(&mut state.snapshot) {
                return false;
            }
            state.generation += 1;
            true
        });
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
        self.update(|snapshot| {
            if snapshot.status.inventory == health
                && snapshot.inventory == inventories
                && snapshot.standalone == standalone
            {
                return false;
            }
            snapshot.status.inventory = health;
            snapshot.inventory = inventories.to_vec();
            snapshot.standalone = standalone.to_vec();
            true
        });
    }

    /// Publish the latest aggregate camera-use sample.
    pub fn set_camera_active(&self, active: bool) {
        self.update(|snapshot| {
            if snapshot.camera_active == active {
                return false;
            }
            snapshot.camera_active = active;
            true
        });
    }

    /// Publish the autostart state the current config asks for.
    pub fn set_launch_at_login(&self, enabled: bool) {
        self.update(|snapshot| {
            if snapshot.status.launch_at_login == enabled {
                return false;
            }
            snapshot.status.launch_at_login = enabled;
            true
        });
    }

    /// Publish an Accessibility trust change, as observed by
    /// [`watchers::accessibility`](crate::watchers::accessibility).
    pub fn set_accessibility_granted(&self, granted: bool) {
        self.update(|snapshot| {
            if snapshot.status.accessibility_granted == granted {
                return false;
            }
            snapshot.status.accessibility_granted = granted;
            true
        });
    }

    /// Publish where the pairing session stands, or `None` for no session.
    ///
    /// A terminal phase is left in place on purpose: it is the session's result,
    /// and it has to survive until the GUI cancels or starts another one, or a
    /// result could fall between two observations.
    pub fn set_pairing(&self, phase: Option<PairingPhase>) {
        self.update(|snapshot| {
            if snapshot.pairing == phase {
                return false;
            }
            snapshot.pairing = phase;
            true
        });
    }

    /// Add a discovered device to the pairing session, moving it into
    /// [`PairingPhase::Found`] if it isn't there yet. Re-discovering the same
    /// address changes nothing.
    pub fn found_pairing_device(&self, device: FoundDevice) {
        self.update(|snapshot| {
            let mut found = match snapshot.pairing.take() {
                Some(PairingPhase::Found(found)) => found,
                _ => Vec::new(),
            };
            let known = found.iter().any(|seen| seen.address == device.address);
            if !known {
                found.push(device);
            }
            snapshot.pairing = Some(PairingPhase::Found(found));
            !known
        });
    }

    /// Publish whether the OS input hook is currently installed.
    pub fn set_hook_installed(&self, installed: bool) {
        self.update(|snapshot| {
            if snapshot.status.hook_installed == installed {
                return false;
            }
            snapshot.status.hook_installed = installed;
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::ObservableState;
    use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
    use openlogi_hid::DIRECT_DEVICE_INDEX;
    use openlogi_ipc::InventoryHealth;
    use std::sync::Arc;
    use std::time::Duration;

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

    #[tokio::test]
    async fn a_client_that_has_seen_nothing_gets_the_current_state_at_once() {
        let state = state();
        let observed = state.observe(0).await;
        assert_eq!(observed.generation, 1, "the cell starts past the sentinel");
        assert_eq!(
            observed.snapshot.status.inventory,
            InventoryHealth::Scanning
        );
    }

    #[tokio::test]
    async fn a_stale_generation_gets_the_current_state_at_once() {
        let state = state();
        state.set_inventory(InventoryHealth::Ready, &[inventory(true)], &[]);

        let observed = state.observe(1).await;
        assert_eq!(observed.generation, 2);
        assert_eq!(observed.snapshot.inventory.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_up_to_date_client_is_woken_by_the_next_change() {
        let state = Arc::new(state());
        let writer = Arc::clone(&state);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            writer.set_hook_installed(true);
        });

        let observed = state.observe(1).await;
        assert_eq!(observed.generation, 2);
        assert!(observed.snapshot.status.hook_installed);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_to_report_answers_with_the_unchanged_state() {
        let state = state();
        // Up to date and nobody writes: the hold elapses and the caller learns
        // the agent is alive without learning anything new.
        let observed = state.observe(1).await;
        assert_eq!(observed.generation, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_write_does_not_end_the_hold() {
        let state = Arc::new(state());
        state.set_hook_installed(true);
        let writer = Arc::clone(&state);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            // Same value: this must not be mistaken for news.
            writer.set_hook_installed(true);
        });

        let observed = state.observe(2).await;
        assert_eq!(observed.generation, 2, "the hold ran out, nothing changed");
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
