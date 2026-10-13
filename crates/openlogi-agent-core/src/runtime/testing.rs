//! Real button dispatch with a recording sink instead of native action execution.

use std::sync::mpsc;

pub(crate) use super::button::ButtonRuntimeEvent;
use super::*;

/// Keep normal press admission and worker ordering while recording their output.
pub(crate) fn recording_button_runtime() -> (ActionRuntime, mpsc::Receiver<ButtonRuntimeEvent>) {
    let (sent, received) = mpsc::channel();
    let buttons = ButtonRuntimeOwner::spawn(move |event| {
        // A failed assertion may drop the receiver before runtime shutdown.
        let _ = sent.send(event);
    })
    .expect("button worker should start");
    let (_signal, device_io) = openlogi_hid::device_io_channel();
    let (action_ring, _ring_rx) = tokio::sync::mpsc::unbounded_channel();
    let dispatcher = ActionDispatcher {
        executor: ActionExecutor {
            dpi_cycle: Arc::default(),
            access: DeviceAccess {
                channel: openlogi_hid::CaptureChannelSlot::default(),
                registry: openlogi_hid::ChannelRegistry::default(),
                receiver_access: crate::receiver_access::ReceiverAccess::default(),
                device_io,
            },
            action_ring,
        },
        buttons: buttons.input(),
    };
    (
        ActionRuntime {
            dispatcher,
            buttons,
        },
        received,
    )
}
