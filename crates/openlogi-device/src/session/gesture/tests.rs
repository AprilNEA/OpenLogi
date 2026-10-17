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

#[tokio::test]
async fn a_middle_button_gesture_source_needs_the_devices_gesture_task() {
    // CID 0x0052 is the MX Anywhere 2S gesture button and an ordinary middle
    // click everywhere else: the device's own task decides.
    for (task_id, expected) in [
        (reprog_controls::GESTURE_BUTTON_TASK_IDS[0], true),
        (reprog_controls::GESTURE_BUTTON_TASK_IDS[1], true),
        (0x003a, false), // Mouse Middle Button
    ] {
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => response[4] = 2,
                (2, 0) => response[4] = 1,
                (2, 1) => {
                    response[4..6].copy_from_slice(&0x0052u16.to_be_bytes());
                    response[6..8].copy_from_slice(&task_id.to_be_bytes());
                    response[8] = 0x20;
                    response[12] = 0x01;
                }
                (2, 2) => response[4..6].copy_from_slice(&0x0052u16.to_be_bytes()),
                (2, 3) => return Some(request.to_vec()),
                _ => panic!("unexpected capture request: {request:02x?}"),
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        let device = Device::new(channel.clone(), 0xff).await.unwrap();
        let spec = CaptureSpec {
            divert_gesture_navigation: vec![(0x0052, ButtonId::MiddleClick)],
            ..CaptureSpec::default()
        };
        let mut armed = ArmedControls::default();
        arm_controls_into(&device, &channel, 0xff, &spec, &mut armed)
            .await
            .unwrap();
        assert_eq!(
            armed.gesture_button_cids,
            if expected {
                vec![(0x0052, ButtonId::MiddleClick)]
            } else {
                vec![]
            },
            "task {task_id:#06x} must {} arm the control",
            if expected { "" } else { "not" }
        );
        let diversion_written = handle
            .written_reports()
            .into_iter()
            .any(|report| report[2] == 2 && report[3] >> 4 == 3);
        assert_eq!(
            diversion_written,
            expected,
            "task {task_id:#06x} must {} write a raw-XY diversion",
            if expected { "" } else { "not" }
        );
    }
}

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
