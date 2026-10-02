//! Manual callback latency probe. The test dispatcher never injects host input.

use super::*;
use std::hint::black_box;

#[test]
#[ignore = "manual latency comparison; run with --ignored --nocapture --test-threads=1"]
fn hook_button_latency() {
    use super::super::super::button::ButtonRuntimeEvent;
    let (dispatcher, mut owner, events) = test_dispatcher();
    let hooks = Arc::new(RwLock::new(HookMaps {
        bindings: BTreeMap::from([(ButtonId::Back, Action::PreviousDesktop.into())]),
        pointer_target: Some(openlogi_hook::PointerTarget::Desktop),
        ..HookMaps::default()
    }));
    let mouse = EventDevice {
        vendor_id: Some(openlogi_hook::LOGITECH_VENDOR_ID),
        ..EventDevice::default()
    };
    let mut down = Vec::new();
    let mut up = Vec::new();
    for sample in 0..12_000 {
        for (pressed, samples) in [(true, &mut down), (false, &mut up)] {
            let start = Instant::now();
            let disposition = black_box(handle_button(
                black_box(ButtonId::Back),
                pressed,
                Some(&mouse),
                &hooks,
                &dispatcher,
                || ActionDispatchTarget::Keyboard,
            ));
            let elapsed = start.elapsed().as_nanos();
            assert_eq!(disposition, EventDisposition::Suppress);
            if sample >= 2_000 {
                samples.push(elapsed);
            }
            let event = events.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(matches!(
                (pressed, event),
                (true, ButtonRuntimeEvent::Started(_)) | (false, ButtonRuntimeEvent::Ended { .. })
            ));
        }
    }
    assert!(owner.shutdown());
    for (edge, mut samples) in [("down", down), ("up", up)] {
        samples.sort_unstable();
        eprintln!(
            "hook_{edge}: samples={} p50_ns={} p95_ns={}",
            samples.len(),
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100]
        );
    }
}
