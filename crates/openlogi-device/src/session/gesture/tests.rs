use hidpp::device::Device;
use tokio::sync::{mpsc, oneshot};

use super::accum::handle_reprog;
use super::arm::arm_controls_into;
use super::*;
use crate::backend::NodeId;
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};
use crate::reprog_controls::RawControlEvent;
use crate::session::capture_restore::{
    ArmedReporting, CaptureStop, PointerDivert, ReprogRestore, divert_change, divert_keyboard_key,
    drop_listener_after, restore_after_stop, stop_for_current_publication, stop_for_dead_channel,
    undivert_change, undivert_keyboard_key, wait_for_channel_change,
};
use crate::session::restore::rollback_start;
use crate::{ChannelRegistry, DeviceRoute};

mod accumulator;
mod foreign_change;
mod gesture46;
mod main_wheel;
mod release;
mod restore;
mod session;
mod thumb_wheel;
mod wheel_hold;

/// The route every scripted device in these tests sits on: a direct
/// connection the host kernel diverts nothing on.
const TEST_ROUTE: DeviceRoute = DeviceRoute::Direct {
    vendor_id: 0x046d,
    product_id: 0xb35b,
};

fn reporting(
    diverted: bool,
    remap: Option<reprog_controls::ControlId>,
) -> reprog_controls::CidReporting {
    reprog_controls::CidReporting {
        cid: reprog_controls::ControlId(reprog_controls::GESTURE_BUTTON_CID),
        diverted,
        persistently_diverted: true,
        force_raw_xy: true,
        raw_xy: true,
        remap,
        analytics_key_events: true,
        raw_wheel: true,
    }
}
