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
    drop_listener_after, restore_after_stop, stop_for_current_publication, undivert_change,
    undivert_keyboard_key, wait_for_channel_change,
};
use crate::session::restore::rollback_start;
use crate::{ChannelRegistry, DeviceRoute};

mod accumulator;
mod main_wheel;
mod restore;
mod session;
mod thumb_wheel;
mod wheel_hold;

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
