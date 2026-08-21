//! `openlogi-device` over `async-hid`: this host's HID stack, and the wiring
//! that points the device layer at it.
//!
//! Everything that knows HID++ lives in `openlogi-device` and is handed a
//! backend. This crate is the backend — enumeration and opening through
//! `async-hid`, the Windows composite channel, macOS Input Monitoring, the
//! on-disk probe cache — plus [`host`], which supplies it to the entry points
//! so a caller who simply means "this machine" need not say so.
//!
//! The device layer's own types are re-exported unchanged, so a consumer
//! reaches for one path whether it is protocol or platform.

#![deny(missing_docs)]
#![deny(rustdoc::bare_urls)]
#![deny(rustdoc::broken_intra_doc_links)]

mod transport;

pub mod host;
pub mod permissions;
pub mod probe_cache;

// The device layer, verbatim. `host` shadows the entry points that need a
// backend with versions that supply this host's; everything else is the same
// item under a shorter path.
pub use openlogi_device::*;
pub use openlogi_device::{backend, inventory, pairing, session, write};

pub use hidpp::feature::FeatureType;
pub use hidpp::feature::device_information::DeviceEntityType;
pub use host::{
    apply_litra, channel_pool, dump_features, dump_firmware_entities, dump_reprog_controls,
    enumerate, enumerate_standalone, get_backlight, get_dpi, get_dpi_info, get_scroll_wheel_mode,
    get_smartshift_status, list_pairing_receivers, play_haptic, read_battery_raw,
    set_backlight_enabled, set_dpi, set_fn_lock, set_keyboard_color, set_keyboard_color_with,
    set_scroll_inversion, set_scroll_resolution, set_scroll_wheel_mode, set_smartshift,
    set_smartshift_sensitivity, toggle_smartshift, watch_hotplug,
};
pub use probe_cache::FileProbeCacheStore;
