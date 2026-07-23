use super::*;
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};

use hidpp::channel::{HidppChannel, RawHidChannel};

struct LivenessRawChannel {
    connected: Arc<AtomicBool>,
}

#[hidpp::async_trait]
impl RawHidChannel for LivenessRawChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xb023
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Sync + Send>> {
        Ok(src.len())
    }

    async fn read_report(&self, _buf: &mut [u8]) -> Result<usize, Box<dyn Error + Sync + Send>> {
        std::future::pending().await
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((false, true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Sync + Send>> {
        unreachable!("mock declares HID++ support")
    }
}

#[tokio::test]
async fn established_capture_exits_when_its_channel_disconnects() {
    let connected = Arc::new(AtomicBool::new(true));
    let channel = HidppChannel::from_raw_channel(LivenessRawChannel {
        connected: Arc::clone(&connected),
    })
    .await
    .unwrap_or_else(|e| panic!("mock HID++ channel should open: {e}"));
    let (_shutdown_tx, shutdown_rx) = oneshot::channel();

    connected.store(false, Ordering::Release);
    let exit = tokio::time::timeout(
        Duration::from_millis(50),
        wait_for_capture_exit(&channel, shutdown_rx, Duration::from_millis(1)),
    )
    .await
    .unwrap_or_else(|_| panic!("capture did not notice the disconnected channel"));

    assert_eq!(exit, CaptureExit::Disconnected);
}

#[tokio::test]
async fn established_capture_honors_shutdown_while_connected() {
    let connected = Arc::new(AtomicBool::new(true));
    let channel = HidppChannel::from_raw_channel(LivenessRawChannel { connected })
        .await
        .unwrap_or_else(|e| panic!("mock HID++ channel should open: {e}"));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    shutdown_tx
        .send(CaptureStop::Restore)
        .unwrap_or_else(|_| panic!("capture shutdown receiver should still be live"));

    let exit = wait_for_capture_exit(&channel, shutdown_rx, Duration::from_millis(1)).await;

    assert_eq!(exit, CaptureExit::Stopped(CaptureStop::Restore));
}

#[tokio::test]
async fn established_capture_can_abandon_stale_firmware_state() {
    let connected = Arc::new(AtomicBool::new(true));
    let channel = HidppChannel::from_raw_channel(LivenessRawChannel { connected })
        .await
        .unwrap_or_else(|e| panic!("mock HID++ channel should open: {e}"));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    shutdown_tx
        .send(CaptureStop::Abandon)
        .unwrap_or_else(|_| panic!("capture shutdown receiver should still be live"));

    let exit = wait_for_capture_exit(&channel, shutdown_rx, Duration::from_millis(1)).await;

    assert_eq!(exit, CaptureExit::Stopped(CaptureStop::Abandon));
}

fn press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::GESTURE_BUTTON_CID, 0, 0, 0])
}

fn release() -> RawControlEvent {
    RawControlEvent::DivertedButtons([0, 0, 0, 0])
}

#[test]
fn quick_tap_is_a_click_even_while_the_cursor_moves() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), &[], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Click))
    );
    assert!(
        rx.try_recv().is_err(),
        "a quick tap emits exactly one click"
    );
}

#[test]
fn a_held_gesture_commits_a_swipe_and_does_not_also_click() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), &[], &tx);
    // Pretend the button has been held well past the swipe gate.
    acc.swipe.backdate_hold_for_test();
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 120, dy: 5 },
        &[],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::Gesture(GestureDirection::Right))
    );

    handle_reprog(&mut acc, release(), &[], &tx);
    assert!(
        rx.try_recv().is_err(),
        "a committed swipe must not also click on release"
    );
}

#[test]
fn a_held_dpi_button_presses_once_on_the_rising_edge() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);

    handle_reprog(&mut acc, down, &[dpi], &tx);
    handle_reprog(&mut acc, down, &[dpi], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle))
    );
    assert!(rx.try_recv().is_err(), "a held DPI button presses once");
}

#[test]
fn a_dpi_button_re_presses_after_a_release() {
    // Rising-edge detection must re-arm: press → release → press is two
    // distinct presses. The release (a frame without the CID) is what resets
    // the edge; without it a re-press would be swallowed as "still held".
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);
    let up = RawControlEvent::DivertedButtons([0, 0, 0, 0]);

    handle_reprog(&mut acc, down, &[dpi], &tx);
    handle_reprog(&mut acc, up, &[dpi], &tx);
    handle_reprog(&mut acc, down, &[dpi], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle)),
        "a release re-arms the rising edge"
    );
    assert!(rx.try_recv().is_err());
}
