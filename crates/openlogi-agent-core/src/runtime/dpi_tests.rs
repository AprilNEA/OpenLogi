//! DPI lifecycle regressions through real action dispatch and HID++ replay.

use super::*;
use openlogi_fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
};
use openlogi_hid::replay::{ReplayBackend, ReplayResponseBarrier, ReplayTopology};

const CHANNEL: &str = "dpi-mouse";
const DPI_INDEX: u8 = 4;

fn report(feature: u8, function: u8, payload: &[u8]) -> Vec<u8> {
    let mut report = vec![0; 20];
    report[..4].copy_from_slice(&[0x11, 0xff, feature, function << 4]);
    report[4..4 + payload.len()].copy_from_slice(payload);
    report
}

#[derive(Default)]
struct SensorScript(Vec<CassetteExchange>);

impl SensorScript {
    fn exchange(&mut self, feature: u8, function: u8, request: &[u8], response: &[u8]) {
        self.0.push(CassetteExchange {
            request_match: RequestMatch::Hidpp20,
            request: report(feature, function, request),
            response: Some(report(feature, function, response)),
            required: true,
        });
    }

    fn open(&mut self) {
        self.exchange(0, 1, &[], &[4, 5]);
        self.exchange(0, 0, &[0x22, 1], &[DPI_INDEX, 0, 0]);
    }

    fn supported(&mut self) {
        self.exchange(
            DPI_INDEX,
            1,
            &[],
            &[0, 1, 0x90, 3, 0x20, 4, 0xb0, 6, 0x40, 0, 0],
        );
    }

    fn current(&mut self, value: u16) {
        let [hi, lo] = value.to_be_bytes();
        self.exchange(DPI_INDEX, 2, &[], &[0, hi, lo]);
    }

    fn read(&mut self, value: u16) {
        self.open();
        self.exchange(DPI_INDEX, 0, &[], &[1]);
        self.current(value);
        self.supported();
    }

    fn write(&mut self, value: u16) {
        self.open();
        self.supported();
        let [hi, lo] = value.to_be_bytes();
        self.exchange(DPI_INDEX, 3, &[0, hi, lo], &[0, hi, lo]);
        self.current(value);
    }
}

struct Mouse {
    backend: ReplayBackend,
    runtime: ActionRuntime,
}

impl Mouse {
    async fn new(script: SensorScript) -> Self {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb023,
        };
        let cassette = HidCassette {
            schema_version: FIXTURE_SCHEMA_VERSION,
            name: "dpi-lifecycle".into(),
            channel: CHANNEL.into(),
            report_support: ReportSupport::LongOnly,
            exchanges: script.0,
        };
        let backend = ReplayBackend::new(
            ReplayTopology::for_device(&route, 0x046d, 0xb023, &cassette),
            vec![cassette],
        )
        .unwrap();
        let registry = openlogi_hid::ChannelRegistry::default();
        backend.publish_route(&registry, &route).await.unwrap();
        let cycles = DpiCycles {
            selected: Some("mouse".into()),
            by_key: HashMap::from([(
                "mouse".into(),
                DpiCycleState {
                    presets: vec![Dpi::new(400), Dpi::new(1600)],
                    target: Some(route),
                    ..DpiCycleState::default()
                },
            )]),
        };
        let (action_ring, _ring) = tokio::sync::mpsc::unbounded_channel();
        let runtime = ActionRuntime::new(
            Arc::new(RwLock::new(cycles)),
            DeviceAccess {
                channel: Arc::new(RwLock::new(None)),
                registry,
                receiver_access: crate::receiver_access::ReceiverAccess::default(),
                device_io: openlogi_hid::device_io_channel().1,
                transport_turns: crate::hardware::TransportTurns::default(),
            },
            action_ring,
        )
        .unwrap();
        Self { backend, runtime }
    }

    fn hold_response(&self, function: u8, payload: &[u8]) -> ReplayResponseBarrier {
        self.backend
            .hold_next_response(
                CHANNEL,
                RequestMatch::Hidpp20,
                &report(DPI_INDEX, function, payload),
            )
            .unwrap()
    }

    fn press(&self, button: ButtonId) {
        assert!(
            self.runtime
                .dispatcher()
                .try_hook_button_down(
                    button,
                    Some(&Binding::Single(Action::DpiShift)),
                    ActionDispatchTarget::Keyboard
                )
                .is_some()
        );
    }

    fn release(&self, button: ButtonId) {
        assert!(self.runtime.dispatcher().try_hook_button_up(button));
    }
}

async fn written(barrier: &ReplayResponseBarrier) {
    tokio::time::timeout(Duration::from_secs(2), barrier.request_written())
        .await
        .expect("expected HID++ request reached sensor");
}

#[tokio::test]
async fn shutdown_waits_for_dpi_restore_confirmation() {
    let mut script = SensorScript::default();
    script.read(1200);
    script.write(400);
    script.write(1200);
    let mut mouse = Mouse::new(script).await;
    let low = mouse.hold_response(3, &[0, 1, 0x90]);
    mouse.press(ButtonId::Back);
    written(&low).await;
    low.release();
    let restored = mouse.hold_response(3, &[0, 4, 0xb0]);
    mouse.release(ButtonId::Back);
    written(&restored).await;
    // The release is already in flight before exit, so joining only the button
    // worker cannot prove that the device received its restore.
    let mut exiting = tokio::task::spawn_blocking(move || {
        mouse.runtime.shutdown();
        mouse
    });
    let premature = tokio::time::timeout(Duration::from_millis(100), &mut exiting).await;
    restored.release();
    let (returned_early, mouse) = match premature {
        Ok(result) => (true, result.unwrap()),
        Err(_) => (false, exiting.await.unwrap()),
    };
    assert!(
        !returned_early,
        "shutdown returned while the DPI restore response was still held"
    );
    mouse.backend.require_complete().unwrap();
}

#[tokio::test]
async fn dpi_shift_reads_sensor_after_a_native_dpi_change() {
    let mut script = SensorScript::default();
    script.write(1600);
    // The native DPI button changes the sensor independently of OpenLogi's
    // cached preset. The next read observes 800, with no config reload/event.
    script.read(800);
    script.write(400);
    script.write(800);
    let mut mouse = Mouse::new(script).await;
    let preset = mouse.hold_response(2, &[]);
    mouse
        .runtime
        .dispatcher()
        .dispatch(&Action::SetDpiPreset(1), Some("mouse"));
    written(&preset).await;
    preset.release();
    let low = mouse.hold_response(3, &[0, 1, 0x90]);
    mouse.press(ButtonId::Back);
    written(&low).await;
    let reports = mouse
        .backend
        .channel_completion(CHANNEL)
        .unwrap()
        .written_reports;
    let read_sensor = reports
        .iter()
        .any(|packet| packet[2] == DPI_INDEX && packet[3] >> 4 == 0);
    low.release();
    mouse.release(ButtonId::Back);
    assert!(
        read_sensor,
        "a fresh DPI shift skipped the sensor read after a native DPI change"
    );
    let mouse = tokio::task::spawn_blocking(move || {
        mouse.runtime.shutdown();
        mouse
    })
    .await
    .unwrap();
    mouse.backend.require_complete().unwrap();
}

#[tokio::test]
async fn shutdown_cancels_an_active_dpi_hold_and_restores_it() {
    let mut script = SensorScript::default();
    script.read(1200);
    script.write(400);
    script.write(1200);
    let mut mouse = Mouse::new(script).await;
    let low = mouse.hold_response(3, &[0, 1, 0x90]);
    mouse.press(ButtonId::Back);
    written(&low).await;
    low.release();
    mouse.runtime.dispatcher.executor.dpi_order.drain().await;
    let restored = mouse.hold_response(3, &[0, 4, 0xb0]);
    let exiting = tokio::task::spawn_blocking(move || {
        // No up edge: shutdown must cancel the physical hold itself.
        mouse.runtime.shutdown();
        mouse
    });
    written(&restored).await;
    assert!(!exiting.is_finished());
    restored.release();
    let mouse = exiting.await.unwrap();
    mouse.backend.require_complete().unwrap();
}

#[tokio::test]
async fn overlapping_holds_keep_a_preset_selected_while_held() {
    let mut script = SensorScript::default();
    script.read(1200);
    script.write(400);
    script.write(1600);
    script.write(1600);
    let mut mouse = Mouse::new(script).await;
    let low = mouse.hold_response(3, &[0, 1, 0x90]);
    mouse.press(ButtonId::Back);
    written(&low).await;
    low.release();
    mouse.runtime.dispatcher.executor.dpi_order.drain().await;
    mouse.press(ButtonId::Forward);
    mouse.release(ButtonId::Back);
    let preset = mouse.hold_response(2, &[]);
    mouse
        .runtime
        .dispatcher()
        .dispatch(&Action::SetDpiPreset(1), Some("mouse"));
    written(&preset).await;
    preset.release();
    mouse.runtime.dispatcher.executor.dpi_order.drain().await;
    mouse.release(ButtonId::Forward);
    let mouse = tokio::task::spawn_blocking(move || {
        mouse.runtime.shutdown();
        mouse
    })
    .await
    .unwrap();
    // Exactly one low write, no premature restore from releasing Back, and
    // the final restore honors the user's 1600 choice rather than the snapshot.
    mouse.backend.require_complete().unwrap();
}

#[tokio::test]
async fn re_press_reads_again_if_the_previous_restore_finishes_first() {
    let mut script = SensorScript::default();
    for _ in 0..2 {
        script.read(1200);
        script.write(400);
        script.write(1200);
    }
    let mut mouse = Mouse::new(script).await;
    let low = mouse.hold_response(3, &[0, 1, 0x90]);
    mouse.press(ButtonId::Back);
    written(&low).await;
    low.release();
    let restored = mouse.hold_response(3, &[0, 4, 0xb0]);
    mouse.release(ButtonId::Back);
    written(&restored).await;
    let low_again = mouse.hold_response(3, &[0, 1, 0x90]);
    mouse.press(ButtonId::Back);
    restored.release();
    written(&low_again).await;
    // The prior restore clears the shared snapshot inside the write turn.
    // This second hold must re-read instead of losing its restore value.
    low_again.release();
    mouse.release(ButtonId::Back);
    let mouse = tokio::task::spawn_blocking(move || {
        mouse.runtime.shutdown();
        mouse
    })
    .await
    .unwrap();
    mouse.backend.require_complete().unwrap();
}

#[tokio::test]
async fn failed_sensor_read_never_lowers_dpi_or_blocks_shutdown() {
    let mut script = SensorScript::default();
    script.open();
    script.exchange(DPI_INDEX, 0, &[], &[0]); // Firmware reports no usable sensor.
    let mut mouse = Mouse::new(script).await;
    let read = mouse.hold_response(0, &[]);
    mouse.press(ButtonId::Back);
    written(&read).await;
    read.release();
    mouse.runtime.dispatcher.executor.dpi_order.drain().await;
    let mouse = tokio::task::spawn_blocking(move || {
        mouse.runtime.shutdown();
        mouse
    })
    .await
    .unwrap();
    mouse.backend.require_complete().unwrap();
}

#[tokio::test]
async fn disconnected_mouse_does_not_leave_shutdown_waiting_for_a_restore() {
    let mut script = SensorScript::default();
    script.read(1200);
    script.write(400);
    let mut mouse = Mouse::new(script).await;
    let low = mouse.hold_response(3, &[0, 1, 0x90]);
    mouse.press(ButtonId::Back);
    written(&low).await;
    low.release();
    mouse.runtime.dispatcher.executor.dpi_order.drain().await;
    mouse
        .backend
        .set_channel_connection(
            CHANNEL,
            openlogi_hid::replay::ChannelConnection::Disconnected,
        )
        .unwrap();
    let mouse = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            mouse.runtime.shutdown();
            mouse
        }),
    )
    .await
    .expect("disconnected restore must finish")
    .unwrap();
    mouse.backend.require_complete().unwrap();
}

#[tokio::test]
async fn pending_preset_is_applied_before_a_fresh_hold_snapshot() {
    let mut script = SensorScript::default();
    script.write(1600);
    script.read(1600);
    script.write(400);
    script.write(1600);
    let mut mouse = Mouse::new(script).await;
    let access = mouse
        .runtime
        .dispatcher
        .executor
        .access
        .receiver_access
        .clone();
    let pairing = access
        .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
        .await;
    mouse
        .runtime
        .dispatcher()
        .dispatch(&Action::SetDpiPreset(1), Some("mouse"));
    // Run the same hold handler synchronously so both requests are definitely
    // queued behind pairing before the receiver becomes available.
    let mut held = HeldDpiShifts::default();
    let press = PressToken::hook_for_test(1, ButtonId::Back);
    held.start(&mouse.runtime.dispatcher.executor, &press, Some("mouse"));
    let low = mouse.hold_response(3, &[0, 1, 0x90]);
    drop(pairing);
    written(&low).await;
    low.release();
    held.end(&mouse.runtime.dispatcher.executor, &press);
    let mouse = tokio::task::spawn_blocking(move || {
        mouse.runtime.shutdown();
        mouse
    })
    .await
    .unwrap();
    mouse.backend.require_complete().unwrap();
}

#[tokio::test]
async fn shutdown_has_a_deadline_while_pairing_retains_the_receiver() {
    let mut script = SensorScript::default();
    script.write(1600);
    let mouse = Mouse::new(script).await;
    let access = mouse
        .runtime
        .dispatcher
        .executor
        .access
        .receiver_access
        .clone();
    let pairing = access
        .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
        .await;
    mouse
        .runtime
        .dispatcher()
        .dispatch(&Action::SetDpiPreset(1), Some("mouse"));
    let mut exiting = tokio::task::spawn_blocking(move || {
        let mut mouse = mouse;
        mouse.runtime.shutdown();
        mouse
    });
    let result = tokio::time::timeout(Duration::from_secs(13), &mut exiting).await;
    // Always unblock the old implementation before asserting, including Drop.
    drop(pairing);
    let returned = result.is_ok();
    if result.is_err() {
        let _ = exiting.await.unwrap();
    }
    assert!(
        returned,
        "shutdown waited indefinitely for pairing receiver access"
    );
}
