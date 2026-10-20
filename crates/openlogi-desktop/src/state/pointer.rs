//! Pointer-device query and active-editor state.

use std::collections::HashSet;

use openlogi_core::hid::Dpi;
use swr_core::{Runtime, SwrClient};

use crate::services::device_reads::DeviceReads;

use super::device_key::DeviceKey;
use super::{AppState, DEFAULT_DPI};

pub(super) struct PointerState {
    pub(super) dpi: Dpi,
    pub(super) reads: DeviceReads,
    pub(super) next_smartshift_write_id: u64,
    /// The mice whose last pointer-speed write the mouse refused or the agent
    /// could not deliver.
    pub(super) refused_pointer_speeds: HashSet<DeviceKey>,
}

impl Default for PointerState {
    fn default() -> Self {
        Self {
            dpi: DEFAULT_DPI,
            reads: DeviceReads::default(),
            next_smartshift_write_id: 0,
            refused_pointer_speeds: HashSet::new(),
        }
    }
}

impl AppState {
    pub(crate) fn connect_device_reads(
        &mut self,
        client: SwrClient,
        runtime: std::sync::Arc<dyn Runtime>,
    ) {
        self.pointer.reads.connect(client, runtime);
    }
}
