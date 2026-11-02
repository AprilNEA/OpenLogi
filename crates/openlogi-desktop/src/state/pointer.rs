//! Pointer-device query and active-editor state.

use std::collections::{HashMap, HashSet};

use openlogi_core::hid::Dpi;
use swr_core::{Runtime, SwrClient};

use crate::services::device_reads::DeviceReads;

use super::device_key::DeviceKey;
use super::{AppState, DEFAULT_DPI};

pub(super) struct PointerState {
    pub(super) dpi: Dpi,
    pub(super) reads: DeviceReads,
    pub(super) next_smartshift_write_id: u64,
    /// The newest DPI write sent to each device whose answer has not arrived
    /// yet. Only that write's answer may change the shown DPI.
    pub(super) pending_dpi_writes: HashMap<DeviceKey, u64>,
    pub(super) next_dpi_write: u64,
    /// The mice whose last pointer-speed write the mouse refused or the agent
    /// could not deliver.
    pub(super) refused_pointer_speeds: HashSet<DeviceKey>,
    /// The devices whose last press-force write the device refused or the
    /// agent could not deliver.
    pub(super) refused_force_sensing: HashSet<DeviceKey>,
    /// The newest press-force write sent to each device whose answer has not
    /// arrived yet. Only that write's answer may change the shown force or
    /// save it.
    pub(super) pending_force_sensing_writes: HashMap<DeviceKey, u64>,
    pub(super) next_force_sensing_write: u64,
}

impl Default for PointerState {
    fn default() -> Self {
        Self {
            dpi: DEFAULT_DPI,
            reads: DeviceReads::default(),
            next_smartshift_write_id: 0,
            pending_dpi_writes: HashMap::new(),
            next_dpi_write: 0,
            refused_pointer_speeds: HashSet::new(),
            refused_force_sensing: HashSet::new(),
            pending_force_sensing_writes: HashMap::new(),
            next_force_sensing_write: 0,
        }
    }
}

impl AppState {
    pub(crate) fn connect_device_reads(
        &mut self,
        client: SwrClient,
        runtime: std::sync::Arc<dyn Runtime>,
    ) {
        self.pointer.reads.connect(client.clone(), runtime.clone());
        self.disable_keys_reads.connect(client, runtime);
    }
}
