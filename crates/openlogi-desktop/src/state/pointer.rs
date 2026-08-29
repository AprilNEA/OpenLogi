//! Pointer-device query and active-editor state.

use openlogi_core::hid::{Dpi, ReportRateHz};
use swr_core::{Runtime, SwrClient};

use crate::services::device_reads::DeviceReads;

use super::{AppState, DEFAULT_DPI};

pub(super) struct PointerState {
    pub(super) dpi: Dpi,
    pub(super) report_rate: ReportRateHz,
    pub(super) reads: DeviceReads,
    pub(super) next_smartshift_write_id: u64,
}

impl Default for PointerState {
    fn default() -> Self {
        Self {
            dpi: DEFAULT_DPI,
            report_rate: ReportRateHz::new(1000),
            reads: DeviceReads::default(),
            next_smartshift_write_id: 0,
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
