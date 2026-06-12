//! DPI-cycle state shared with background action dispatch.

use openlogi_hid::{DeviceRoute, DpiCapabilities};

/// Shared state consumed by the OS hook thread and the DPI panel UI to
/// implement DPI preset cycling and direct preset selection actions.
///
/// `index` is the position of the *current* DPI (i.e. the one last set on the
/// device), not the next-to-fire. `cycle` advances and returns the new value.
#[derive(Debug, Clone, Default)]
pub struct DpiCycleState {
    pub presets: Vec<u32>,
    pub index: usize,
    pub target: Option<DeviceRoute>,
    pub capabilities: Option<DpiCapabilities>,
}

impl DpiCycleState {
    /// Advance to the next preset (wrapping last → first) and return the new
    /// DPI + the device target to write to. Returns `None` if `presets` is
    /// empty.
    pub fn cycle(&mut self) -> Option<(u32, Option<DeviceRoute>)> {
        if self.presets.is_empty() {
            return None;
        }
        self.index = (self.index + 1) % self.presets.len();
        Some((
            self.normalize(self.presets[self.index]),
            self.target.clone(),
        ))
    }

    /// Jump to preset `i`, clamping to the list length. Returns the DPI +
    /// target, or `None` if `presets` is empty.
    pub fn set(&mut self, i: usize) -> Option<(u32, Option<DeviceRoute>)> {
        if self.presets.is_empty() {
            return None;
        }
        let clamped = i.min(self.presets.len() - 1);
        self.index = clamped;
        Some((self.normalize(self.presets[clamped]), self.target.clone()))
    }

    fn normalize(&self, dpi: u32) -> u32 {
        self.capabilities
            .as_ref()
            .map_or(dpi, |caps| caps.snap(dpi))
    }
}
