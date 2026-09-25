use super::*;

fn candidate(vendor_id: u32, pressed: bool) -> ButtonCandidate {
    ButtonCandidate {
        device: EventDevice {
            vendor_id: Some(vendor_id),
            ..EventDevice::default()
        },
        pressed,
    }
}

#[test]
fn selects_only_one_pressed_logitech_device() {
    let values = [candidate(0x046d, true), candidate(0x4f53, false)];
    assert_eq!(
        unique_pressed_logitech(&values),
        Some(values[0].device.clone())
    );
}

#[test]
fn rejects_non_logitech_ambiguous_and_unpressed_sources() {
    assert!(unique_pressed_logitech(&[candidate(0x4f53, true)]).is_none());
    assert!(unique_pressed_logitech(&[candidate(0x046d, true), candidate(0x4f53, true)]).is_none());
    assert!(unique_pressed_logitech(&[candidate(0x046d, false)]).is_none());
}

#[test]
fn release_uses_the_source_cached_on_press() {
    let mut resolver = SenderlessButtonResolver::unavailable();
    resolver
        .held_sources
        .insert(4, candidate(0x046d, true).device);

    let released = resolver.resolve(4, false, None);

    assert!(released.as_ref().is_some_and(EventDevice::is_logitech));
    assert!(resolver.resolve(4, false, None).is_none());
}

#[test]
fn an_ambiguous_press_invalidates_a_stale_cached_attribution() {
    // Hold Back on a Logitech mouse (cached), then a second mouse presses
    // the same button number while the first is still held: the new press
    // is ambiguous and must pass through unattributed, but it must also
    // clear the stale cache entry — otherwise releasing the *second* mouse
    // would be misattributed to the first, cached, still-held Logitech
    // device.
    let mut resolver = SenderlessButtonResolver::unavailable();
    resolver
        .held_sources
        .insert(4, candidate(0x046d, true).device);

    let ambiguous = [candidate(0x046d, true), candidate(0x4f53, true)];
    let second_down = resolver.resolve_press(4, &ambiguous);

    assert!(
        second_down.is_none(),
        "an ambiguous press must not be attributed"
    );
    assert!(
        resolver.resolve(4, false, None).is_none(),
        "the stale cache entry must not survive the ambiguous press, or the \
         second mouse's release would be misattributed to the first"
    );
}

#[test]
fn cancel_all_drops_every_cached_attribution() {
    let mut resolver = SenderlessButtonResolver::unavailable();
    resolver
        .held_sources
        .insert(3, candidate(0x046d, true).device);
    resolver
        .held_sources
        .insert(4, candidate(0x046d, true).device);

    resolver.cancel_all();

    assert!(resolver.resolve(3, false, None).is_none());
    assert!(resolver.resolve(4, false, None).is_none());
}
