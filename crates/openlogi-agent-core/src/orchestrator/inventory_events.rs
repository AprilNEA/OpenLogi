//! The production consumer of inventory worker publications.

use openlogi_hid::inventory::events::BacklightUpdate;

use super::Orchestrator;
use crate::watchers::inventory::{InventoryEvent, InventoryRefresh, ReapplyReason};

impl Orchestrator {
    /// Apply one worker publication, retaining recovery before refreshing even
    /// an unchanged inventory. Returns only the transient backlight presentation
    /// for the agent's overlay; all device state and confirmation work stays here.
    pub fn apply_inventory_event(
        &mut self,
        event: InventoryEvent,
        refresh: &InventoryRefresh,
    ) -> Option<BacklightUpdate> {
        match event {
            InventoryEvent::Snapshot {
                inventories,
                standalone,
                hid_open_failures,
                reapply_volatile,
                confirms_volatile_settings,
            } => {
                match reapply_volatile {
                    Some(ReapplyReason::Volatile) => self.reapply_volatile_on_next_refresh(),
                    Some(ReapplyReason::Reconnect) => self.reapply_reconnected_on_next_refresh(),
                    None => {}
                }
                if confirms_volatile_settings {
                    self.refresh_inventory_for_settings_confirmation(
                        &inventories,
                        &standalone,
                        hid_open_failures,
                    );
                } else {
                    self.refresh_inventory(&inventories, &standalone, hid_open_failures);
                }
                if self.needs_reapply_confirmation() {
                    refresh.request_settings_confirmation();
                }
            }
            InventoryEvent::Unavailable => self.mark_inventory_unavailable(),
            InventoryEvent::SystemWake => self.reapply_after_system_resume(),
            InventoryEvent::DeviceWake => self.reapply_reconnected_on_next_refresh(),
            InventoryEvent::LockKeyChanged(change) => self.announce_lock_key(change),
            InventoryEvent::BacklightChanged {
                device_index,
                current_level,
                levels,
                visible,
            } => {
                return Some(BacklightUpdate {
                    device_index,
                    current_level,
                    levels,
                    visible,
                });
            }
        }
        None
    }
}
