use super::*;

const BACK: i64 = 3;

fn device(vendor_id: u32) -> EventDevice {
    EventDevice {
        vendor_id: Some(vendor_id),
        ..EventDevice::default()
    }
}

fn candidate(vendor_id: u32, state: ButtonState) -> ButtonCandidate {
    ButtonCandidate {
        device: device(vendor_id),
        state,
    }
}

fn logitech() -> EventDevice {
    device(0x046d)
}

fn identified(sender_id: u64, device: EventDevice) -> ButtonSender {
    ButtonSender::Device { sender_id, device }
}

/// One transition with `poll` standing in for the `IOHIDManager` read.
fn step(
    resolver: &mut SenderlessButtonResolver,
    button_number: i64,
    pressed: bool,
    sender: ButtonSender,
    poll: Option<&[ButtonCandidate]>,
) -> Option<EventDevice> {
    resolver
        .tracker
        .transition(button_number, pressed, sender, || poll.map(<[_]>::to_vec))
}

fn press(
    resolver: &mut SenderlessButtonResolver,
    button_number: i64,
    poll: Option<&[ButtonCandidate]>,
) -> Option<EventDevice> {
    step(
        resolver,
        button_number,
        true,
        ButtonSender::Unidentified,
        poll,
    )
}

fn release(
    resolver: &mut SenderlessButtonResolver,
    poll: Option<&[ButtonCandidate]>,
) -> Option<EventDevice> {
    step(resolver, BACK, false, ButtonSender::Unidentified, poll)
}

const HOLDER_RELEASED: [ButtonCandidate; 1] = [ButtonCandidate {
    device: EventDevice {
        vendor_id: Some(0x046d),
        product_id: None,
        product_name: None,
    },
    state: ButtonState::Released,
}];

/// A resolver holding a sender-less Logitech attribution for Back.
fn holding_senderless_back() -> SenderlessButtonResolver {
    let mut resolver = SenderlessButtonResolver::unavailable();
    let only_logitech = [candidate(0x046d, ButtonState::Pressed)];
    assert_eq!(
        press(&mut resolver, BACK, Some(&only_logitech)),
        Some(logitech())
    );
    resolver
}

#[test]
fn selects_only_one_pressed_logitech_device() {
    let values = [
        candidate(0x046d, ButtonState::Pressed),
        candidate(0x4f53, ButtonState::Released),
    ];
    assert_eq!(
        unique_pressed_logitech(&values),
        Some(values[0].device.clone())
    );
}

#[test]
fn rejects_non_logitech_ambiguous_and_unpressed_sources() {
    use ButtonState::{Pressed, Released};
    assert!(unique_pressed_logitech(&[candidate(0x4f53, Pressed)]).is_none());
    assert!(
        unique_pressed_logitech(&[candidate(0x046d, Pressed), candidate(0x4f53, Pressed)])
            .is_none()
    );
    assert!(unique_pressed_logitech(&[candidate(0x046d, Released)]).is_none());
}

#[test]
fn an_unreadable_mouse_blocks_attribution() {
    // A competing mouse whose button state could not be read may be the one
    // pressing; the readable Logitech must not look unique because of it.
    let mut resolver = SenderlessButtonResolver::unavailable();
    let unknown_rival = [
        candidate(0x046d, ButtonState::Pressed),
        candidate(0x4f53, ButtonState::Unknown),
    ];
    assert!(press(&mut resolver, BACK, Some(&unknown_rival)).is_none());
    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Unidentified)
            .is_none(),
        "a refused press must not leave an attribution for its release"
    );
}

#[test]
fn a_missing_hid_manager_never_attributes() {
    let mut resolver = SenderlessButtonResolver::unavailable();
    assert!(
        resolver
            .resolve(BACK, true, ButtonSender::Unidentified)
            .is_none()
    );
}

#[test]
fn sender_ids_classify_posted_senderless_and_attributed_events() {
    let lookup = |_| panic!("only a nonzero sender id names a device");
    assert!(matches!(
        ButtonSender::from_sender_id(None, lookup),
        ButtonSender::Posted
    ));
    assert!(matches!(
        ButtonSender::from_sender_id(Some(0), lookup),
        ButtonSender::Unidentified
    ));
    assert!(matches!(
        ButtonSender::from_sender_id(Some(7), |id| {
            assert_eq!(id, 7);
            logitech()
        }),
        ButtonSender::Device { sender_id: 7, device } if device == logitech()
    ));
}

#[test]
fn posted_events_neither_borrow_nor_consume_a_held_attribution() {
    let mut resolver = holding_senderless_back();

    assert!(resolver.resolve(BACK, true, ButtonSender::Posted).is_none());
    assert!(resolver.take_invalidated_sources().is_empty());
    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Posted)
            .is_none()
    );

    assert_eq!(
        release(&mut resolver, Some(&HOLDER_RELEASED)),
        Some(logitech()),
        "the physical release must still find its press-time identity"
    );
}

#[test]
fn release_uses_the_source_cached_on_press() {
    let mut resolver = holding_senderless_back();

    assert_eq!(
        release(&mut resolver, Some(&HOLDER_RELEASED)),
        Some(logitech())
    );
    assert!(release(&mut resolver, Some(&HOLDER_RELEASED)).is_none());
}

#[test]
fn an_abandoned_refused_press_does_not_block_later_attribution() {
    // A second mouse's refused press whose release is lost — it was
    // unplugged mid-press — leaves nothing behind.
    let mut resolver = SenderlessButtonResolver::unavailable();
    let second_mouse = [
        candidate(0x046d, ButtonState::Released),
        candidate(0x4f53, ButtonState::Pressed),
    ];
    assert!(press(&mut resolver, BACK, Some(&second_mouse)).is_none());

    let only_logitech = [candidate(0x046d, ButtonState::Pressed)];
    assert_eq!(
        press(&mut resolver, BACK, Some(&only_logitech)),
        Some(logitech())
    );
}

#[test]
fn an_unreadable_suspect_stays_pinned_once_readable() {
    // The Logitech's press is refused while its state cannot be read. Once
    // readable and still held, it must not lend its identity to an unseen
    // device's press.
    let mut resolver = SenderlessButtonResolver::unavailable();
    let unreadable = [candidate(0x046d, ButtonState::Unknown)];
    let held = [candidate(0x046d, ButtonState::Pressed)];

    assert!(press(&mut resolver, BACK, Some(&unreadable)).is_none());
    assert!(press(&mut resolver, BACK, Some(&held)).is_none());
}

#[test]
fn a_lost_senderless_holder_is_cancelled_by_the_next_press() {
    let mut resolver = holding_senderless_back();
    let other_logitech = EventDevice {
        product_id: Some(2),
        ..logitech()
    };
    let holder_unplugged = [ButtonCandidate {
        device: other_logitech.clone(),
        state: ButtonState::Pressed,
    }];

    assert_eq!(
        press(&mut resolver, BACK, Some(&holder_unplugged)),
        Some(other_logitech)
    );
    assert_eq!(resolver.take_invalidated_sources(), [logitech()]);
}

#[test]
fn a_lost_identified_holder_stops_vetoing_once_seen_released() {
    // The identified Logitech's release is lost; once a poll sees it
    // released, its own sender-less presses are attributable again. The
    // release that poll came with may be its own, so its hold is cancelled.
    let mut resolver = SenderlessButtonResolver::unavailable();
    step(&mut resolver, BACK, true, identified(7, logitech()), None);
    let pressed = [candidate(0x046d, ButtonState::Pressed)];

    assert!(press(&mut resolver, BACK, Some(&pressed)).is_none());
    assert!(release(&mut resolver, Some(&HOLDER_RELEASED)).is_none());
    assert_eq!(resolver.take_invalidated_sources(), [logitech()]);
    assert_eq!(press(&mut resolver, BACK, Some(&pressed)), Some(logitech()));
    assert!(resolver.take_invalidated_sources().is_empty());
}

#[test]
fn the_holders_state_decides_whose_senderless_release_it_is() {
    let ambiguous = [
        candidate(0x046d, ButtonState::Pressed),
        candidate(0x4f53, ButtonState::Pressed),
    ];
    let holder_pressed = [candidate(0x046d, ButtonState::Pressed)];
    let holder_released = [
        candidate(0x046d, ButtonState::Released),
        candidate(0x4f53, ButtonState::Pressed),
    ];
    let holder_unknown = [candidate(0x046d, ButtonState::Unknown)];

    let mut resolver = holding_senderless_back();
    assert!(press(&mut resolver, BACK, Some(&ambiguous)).is_none());
    assert!(resolver.take_invalidated_sources().is_empty());
    assert!(
        release(&mut resolver, Some(&holder_pressed)).is_none(),
        "the second mouse's release must not end the held attribution"
    );
    assert_eq!(
        release(&mut resolver, Some(&holder_released)),
        Some(logitech())
    );

    let mut resolver = holding_senderless_back();
    assert!(press(&mut resolver, BACK, Some(&ambiguous)).is_none());
    assert_eq!(
        release(&mut resolver, Some(&holder_released)),
        Some(logitech())
    );
    assert!(release(&mut resolver, Some(&holder_released)).is_none());

    let mut resolver = holding_senderless_back();
    assert!(press(&mut resolver, BACK, Some(&ambiguous)).is_none());
    assert!(release(&mut resolver, Some(&holder_unknown)).is_none());
    assert_eq!(resolver.take_invalidated_sources(), [logitech()]);
    assert!(release(&mut resolver, Some(&holder_unknown)).is_none());
    assert!(resolver.tracker.holds.is_empty());
}

#[test]
fn a_press_of_a_held_button_is_never_attributed_to_its_holder() {
    // A device cannot press a button it still holds, so a second sender-less
    // press — even one the HID poll cannot see the source of — must pass
    // through rather than borrow the holder's identity.
    let mut resolver = holding_senderless_back();
    let only_holder = [candidate(0x046d, ButtonState::Pressed)];

    assert!(press(&mut resolver, BACK, Some(&only_holder)).is_none());
    assert!(resolver.take_invalidated_sources().is_empty());
}

#[test]
fn a_second_senderless_logitech_never_replaces_the_held_one() {
    let mut resolver = holding_senderless_back();
    let other_logitech = EventDevice {
        product_id: Some(2),
        ..logitech()
    };
    let second = |state| ButtonCandidate {
        device: other_logitech.clone(),
        state,
    };
    let both = [
        candidate(0x046d, ButtonState::Pressed),
        second(ButtonState::Pressed),
    ];
    let holder_released = [HOLDER_RELEASED[0].clone(), second(ButtonState::Pressed)];

    assert!(press(&mut resolver, BACK, Some(&both)).is_none());
    assert!(resolver.take_invalidated_sources().is_empty());
    assert_eq!(
        release(&mut resolver, Some(&holder_released)),
        Some(logitech())
    );
}

#[test]
fn a_release_that_may_be_another_presses_is_not_credited_to_the_holder() {
    // A second mouse's press passed through while the Logitech held. If both
    // read released at a release, crediting it to the holder would suppress
    // the second mouse's release and leave its raw press stuck down.
    let mut resolver = holding_senderless_back();
    let second_pressed = [
        candidate(0x046d, ButtonState::Pressed),
        candidate(0x4f53, ButtonState::Pressed),
    ];
    let both_released = [
        candidate(0x046d, ButtonState::Released),
        candidate(0x4f53, ButtonState::Released),
    ];

    assert!(press(&mut resolver, BACK, Some(&second_pressed)).is_none());
    assert!(release(&mut resolver, Some(&both_released)).is_none());
    assert_eq!(resolver.take_invalidated_sources(), [logitech()]);
    assert!(release(&mut resolver, Some(&both_released)).is_none());
    assert!(resolver.take_invalidated_sources().is_empty());
    assert!(resolver.tracker.holds.is_empty());
}

#[test]
fn a_mixed_sender_release_of_an_identified_press_is_not_credited_to_the_holder() {
    // A non-Logitech mouse's identified press passed through; its release
    // arrives sender-less as the held Logitech also reads released.
    let mut resolver = holding_senderless_back();
    let other = device(0x4f53);
    let both_released = [
        candidate(0x046d, ButtonState::Released),
        candidate(0x4f53, ButtonState::Released),
    ];

    resolver.resolve(BACK, true, identified(9, other.clone()));
    assert!(release(&mut resolver, Some(&both_released)).is_none());
    assert_eq!(resolver.take_invalidated_sources(), [other, logitech()]);
}

#[test]
fn an_identified_press_released_queued_contests_the_next_holder() {
    // A non-Logitech mouse's identified press passed through and reads
    // released as a sender-less Logitech press arrives: its release, still
    // queued, may arrive sender-less, and must not end the new hold.
    let mut resolver = SenderlessButtonResolver::unavailable();
    let other = device(0x4f53);
    resolver.resolve(BACK, true, identified(9, other.clone()));
    let logitech_down = [
        candidate(0x046d, ButtonState::Pressed),
        candidate(0x4f53, ButtonState::Released),
    ];
    let both_released = [
        candidate(0x046d, ButtonState::Released),
        candidate(0x4f53, ButtonState::Released),
    ];

    assert_eq!(
        press(&mut resolver, BACK, Some(&logitech_down)),
        Some(logitech())
    );
    assert!(release(&mut resolver, Some(&both_released)).is_none());
    assert_eq!(resolver.take_invalidated_sources(), [other, logitech()]);
}

#[test]
fn an_identified_press_anew_is_not_cancelled_by_other_releases() {
    // The identified Logitech read released while another mouse pressed,
    // then pressed again: its earlier release has arrived, so the other
    // mouse's release cannot be its and must not cancel the new hold.
    let mut resolver = SenderlessButtonResolver::unavailable();
    let other = || device(0x4f53);
    resolver.resolve(BACK, true, identified(7, logitech()));
    let other_down = [
        candidate(0x046d, ButtonState::Released),
        ButtonCandidate {
            device: other(),
            state: ButtonState::Pressed,
        },
    ];
    let other_up = [
        candidate(0x046d, ButtonState::Pressed),
        ButtonCandidate {
            device: other(),
            state: ButtonState::Released,
        },
    ];

    assert!(press(&mut resolver, BACK, Some(&other_down)).is_none());
    resolver.resolve(BACK, true, identified(7, logitech()));
    assert!(release(&mut resolver, Some(&other_up)).is_none());
    assert!(resolver.take_invalidated_sources().is_empty());
}

#[test]
fn a_sender_less_release_of_an_identified_press_cancels_its_hold() {
    // The identified Logitech's release arrives sender-less: it must reach
    // the runtime as that press's end, or its hold would never end.
    let mut resolver = SenderlessButtonResolver::unavailable();
    resolver.resolve(BACK, true, identified(7, logitech()));

    assert!(release(&mut resolver, Some(&HOLDER_RELEASED)).is_none());
    assert_eq!(resolver.take_invalidated_sources(), [logitech()]);
    assert!(resolver.tracker.holds.is_empty());
}

#[test]
fn every_passed_through_press_during_a_hold_contests_its_release() {
    use ButtonState::{Pressed, Released};
    // Holding the Logitech, a second mouse's refused press reads released
    // by a third mouse's press, its release still queued. Once the third
    // mouse's release arrives, the second's still contests the holder's.
    let third = || device(0x1234);
    let poll = |logitech, second, third_state| {
        vec![
            candidate(0x046d, logitech),
            candidate(0x4f53, second),
            ButtonCandidate {
                device: third(),
                state: third_state,
            },
        ]
    };
    for invisible in [false, true] {
        let mut resolver = holding_senderless_back();
        assert!(press(&mut resolver, BACK, Some(&poll(Pressed, Pressed, Released))).is_none());
        let third_state = if invisible { Released } else { Pressed };
        assert!(
            press(
                &mut resolver,
                BACK,
                Some(&poll(Pressed, Released, third_state))
            )
            .is_none()
        );
        assert!(release(&mut resolver, Some(&poll(Pressed, Released, Released))).is_none());
        assert!(
            release(&mut resolver, Some(&poll(Released, Released, Released))).is_none(),
            "the second mouse's release may be this one"
        );
        assert_eq!(resolver.take_invalidated_sources(), [logitech()]);
    }
}

#[test]
fn a_release_with_nothing_recorded_never_polls() {
    let mut resolver = SenderlessButtonResolver::unavailable();
    assert!(
        resolver
            .tracker
            .transition(BACK, false, ButtonSender::Unidentified, || {
                panic!("nothing is held, so there is nothing to poll for")
            })
            .is_none()
    );
}

#[test]
fn an_identified_hold_is_never_borrowed_by_a_senderless_press() {
    // An identified Logitech holds Back; a second mouse's sender-less click
    // has already released by poll time, so only the holder reads pressed.
    let mut resolver = SenderlessButtonResolver::unavailable();
    let only_holder = [candidate(0x046d, ButtonState::Pressed)];
    assert_eq!(
        step(&mut resolver, BACK, true, identified(7, logitech()), None),
        Some(logitech())
    );

    assert!(press(&mut resolver, BACK, Some(&only_holder)).is_none());
    assert!(
        resolver.take_invalidated_sources().is_empty(),
        "an attributed hold still gets its attributed release"
    );
    assert!(
        step(
            &mut resolver,
            BACK,
            false,
            ButtonSender::Unidentified,
            Some(&only_holder)
        )
        .is_none()
    );
    assert_eq!(
        step(&mut resolver, BACK, false, identified(7, logitech()), None),
        Some(logitech())
    );
    assert!(resolver.tracker.holds.is_empty());
}

#[test]
fn an_identified_press_keeps_a_held_senderless_attribution() {
    // Hold Back on a sender-less Logitech mouse, then press and release Back
    // on an identified mouse: the Logitech release must still find its
    // press-time identity, or the hold begun under it would never end.
    let mut resolver = holding_senderless_back();
    let other = device(0x4f53);

    assert_eq!(
        resolver.resolve(BACK, true, identified(9, other.clone())),
        Some(other.clone())
    );
    assert!(resolver.take_invalidated_sources().is_empty());
    assert_eq!(
        resolver.resolve(BACK, false, identified(9, other.clone())),
        Some(other)
    );
    assert_eq!(
        release(&mut resolver, Some(&HOLDER_RELEASED)),
        Some(logitech())
    );
}

#[test]
fn the_holders_identified_release_clears_its_senderless_attribution() {
    let mut resolver = holding_senderless_back();

    assert_eq!(
        resolver.resolve(BACK, false, identified(7, logitech())),
        Some(logitech())
    );
    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Unidentified)
            .is_none()
    );
}

#[test]
fn cancel_all_drops_every_cached_attribution() {
    let mut resolver = holding_senderless_back();
    let only_logitech = [candidate(0x046d, ButtonState::Pressed)];
    assert_eq!(
        press(&mut resolver, BACK + 1, Some(&only_logitech)),
        Some(logitech())
    );

    resolver.cancel_all();

    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Unidentified)
            .is_none()
    );
    assert!(
        resolver
            .resolve(BACK + 1, false, ButtonSender::Unidentified)
            .is_none()
    );
}

/// The physical sources of one Back press each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Actor {
    IdentifiedLogitech,
    SenderlessLogitech,
    SecondMouse,
    Posted,
}

const ACTORS: [Actor; 4] = [
    Actor::IdentifiedLogitech,
    Actor::SenderlessLogitech,
    Actor::SecondMouse,
    Actor::Posted,
];

/// What the `IOHIDManager` poll reports for one physical mouse.
#[derive(Clone, Copy, Debug)]
enum PollView {
    Truthful,
    /// A click too fast for the poll, or a mouse it cannot see pressed.
    AlwaysReleased,
    Unknown,
    /// Truthful on the first read, unreadable after.
    FailsLater,
}

const VIEWS: [PollView; 4] = [
    PollView::Truthful,
    PollView::AlwaysReleased,
    PollView::Unknown,
    PollView::FailsLater,
];

/// How the second mouse's press and release carry their sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SecondSenders {
    Senderless,
    Identified,
    /// An identified press whose release arrives sender-less.
    IdentifiedPress,
}

const SECOND_SENDERS: [SecondSenders; 3] = [
    SecondSenders::Senderless,
    SecondSenders::Identified,
    SecondSenders::IdentifiedPress,
];

/// The three HID mice, indexed like the first three `ACTORS`.
fn mouse(index: usize) -> EventDevice {
    EventDevice {
        vendor_id: Some(if index == 2 { 0x4f53 } else { 0x046d }),
        product_id: Some(u32::try_from(index).unwrap()),
        product_name: None,
    }
}

fn interleavings(
    prefix: &mut Vec<(usize, bool)>,
    progress: &mut [u8; 4],
    out: &mut Vec<Vec<(usize, bool)>>,
) {
    if progress.iter().all(|&done| done == 2) {
        out.push(prefix.clone());
        return;
    }
    for actor in 0..ACTORS.len() {
        if progress[actor] == 2 {
            continue;
        }
        let pressed = progress[actor] == 0;
        progress[actor] += 1;
        prefix.push((actor, pressed));
        interleavings(prefix, progress, out);
        prefix.pop();
        progress[actor] -= 1;
    }
}

/// Every ordering of one press and release per actor, under every poll view
/// per mouse, every subset of mice unplugged in place of their release, and
/// every sender mix, checked against the invariants a remapping runtime
/// relies on.
#[test]
fn every_interleaving_attributes_only_physical_presses_and_closes_every_hold() {
    let mut orders = Vec::new();
    interleavings(&mut Vec::new(), &mut [0; 4], &mut orders);
    assert_eq!(orders.len(), 2520);

    let views = VIEWS
        .iter()
        .flat_map(|&a| VIEWS.iter().flat_map(move |&b| VIEWS.map(|c| [a, b, c])));
    std::thread::scope(|scope| {
        for views in views {
            let orders = &orders;
            scope.spawn(move || {
                for unplugged in 0..8_u8 {
                    let unplugged = [0, 1, 2].map(|index| unplugged & (1 << index) != 0);
                    for senders in case_senders(unplugged) {
                        for order in orders {
                            let case = Case {
                                views,
                                unplugged,
                                senders,
                                early: [false; 3],
                            };
                            check_interleaving(order, case);
                        }
                    }
                }
            });
        }
    });
}

/// The same orderings with the HID poll running ahead of the event queue:
/// a mouse may already read pressed, or released, one event before its own
/// press or release event. Skipped, as indistinguishable from the Logitech's
/// own transition: an invisible second mouse's press arriving while a
/// Logitech is already down ahead of its own press event, whose poll sees
/// just that Logitech pressed; an invisible press made before the
/// sender-less Logitech's hold began, released just as that Logitech
/// already reads released ahead of its own release event; and an invisible
/// release arriving just as the identified Logitech already reads released
/// ahead of its own sender-less release.
#[test]
fn every_interleaving_holds_with_the_poll_ahead_of_press_events() {
    let mut orders = Vec::new();
    interleavings(&mut Vec::new(), &mut [0; 4], &mut orders);

    std::thread::scope(|scope| {
        for early in 1..8_u8 {
            let early = [0, 1, 2].map(|index| early & (1 << index) != 0);
            let orders = &orders;
            scope.spawn(move || {
                for second_mouse in [PollView::Truthful, PollView::AlwaysReleased] {
                    let position = |order: &[(usize, bool)], event| {
                        order.iter().position(|&step| step == event)
                    };
                    let indistinguishable = |order: &[(usize, bool)], senders: Senders| {
                        matches!(second_mouse, PollView::AlwaysReleased)
                            && (order.windows(2).any(|pair| {
                                pair[0] == (2, true)
                                    && matches!(pair[1], (next, true) if next < 2 && early[next])
                            }) || early[1]
                                && position(order, (2, true)) < position(order, (1, true))
                                && order
                                    .windows(2)
                                    .any(|pair| pair == [(2, false), (1, false)])
                                || early[0]
                                    && senders.senderless_release
                                    && order
                                        .windows(2)
                                        .any(|pair| pair == [(2, false), (0, false)]))
                    };
                    for unplugged in 0..8_u8 {
                        let unplugged = [0, 1, 2].map(|index| unplugged & (1 << index) != 0);
                        for senders in case_senders(unplugged) {
                            for order in orders
                                .iter()
                                .filter(|order| !indistinguishable(order, senders))
                            {
                                let case = Case {
                                    views: [PollView::Truthful, PollView::Truthful, second_mouse],
                                    unplugged,
                                    senders,
                                    early,
                                };
                                check_interleaving(order, case);
                            }
                        }
                    }
                }
            });
        }
    });
}

#[derive(Clone, Copy, Debug)]
struct Case {
    views: [PollView; 3],
    /// Mice unplugged in place of their release.
    unplugged: [bool; 3],
    senders: Senders,
    /// Mice that read pressed one event before their press event.
    early: [bool; 3],
}

#[derive(Clone, Copy, Debug)]
struct Senders {
    /// Whether the identified Logitech's release arrives sender-less.
    senderless_release: bool,
    /// Whether the sender-less Logitech's release arrives identified.
    identified_release: bool,
    second: SecondSenders,
}

/// Every sender mix whose mixed release is not lost to an unplug.
fn case_senders(unplugged: [bool; 3]) -> impl Iterator<Item = Senders> {
    [false, true]
        .into_iter()
        .flat_map(move |senderless_release| {
            [false, true]
                .into_iter()
                .flat_map(move |identified_release| {
                    SECOND_SENDERS
                        .into_iter()
                        .filter(move |&second| {
                            !(senderless_release && unplugged[0]
                                || identified_release && unplugged[1]
                                || second == SecondSenders::IdentifiedPress && unplugged[2])
                        })
                        .map(move |second| Senders {
                            senderless_release,
                            identified_release,
                            second,
                        })
                })
        })
}

/// The resolver plus the runtime-side view of the sender-less hold.
struct Model {
    resolver: SenderlessButtonResolver,
    /// The device a hold begun under a sender-less attribution belongs to.
    hold: Option<EventDevice>,
    /// The runtime's suppressed presses, by attributed device: every
    /// attributed Logitech press is remapped.
    suppressed: Vec<EventDevice>,
    /// Each mouse's press the runtime passed through to the OS, unreleased.
    raw: [bool; 3],
    /// Mice whose identified press no identified release has ended yet.
    recorded: [bool; 3],
    /// Recorded mice a sender-less poll saw released or could not read:
    /// only their suppressed press may be cancelled by another event.
    stale: [bool; 3],
    /// Mice whose suppressed press an invalidation cancelled.
    cancelled: [bool; 3],
}

impl Model {
    fn new() -> Self {
        Self {
            resolver: SenderlessButtonResolver::unavailable(),
            hold: None,
            suppressed: Vec::new(),
            raw: [false; 3],
            recorded: [false; 3],
            stale: [false; 3],
            cancelled: [false; 3],
        }
    }

    /// Apply one transition's invalidation and attribution to `hold`, with
    /// `may_invalidate` saying whether dropping the hold unreleased is
    /// justified. Returns the attribution.
    fn apply(
        &mut self,
        pressed: bool,
        sender: ButtonSender,
        poll: Vec<ButtonCandidate>,
        may_invalidate: bool,
        context: &dyn Fn() -> String,
    ) -> Option<EventDevice> {
        let senderless = matches!(sender, ButtonSender::Unidentified);
        for index in 0..3 {
            if let ButtonSender::Device { device, .. } = &sender
                && *device == mouse(index)
            {
                self.recorded[index] = pressed;
                self.stale[index] = false;
            } else if senderless {
                self.stale[index] |= self.recorded[index]
                    && matches!(
                        polled_state(&poll, &mouse(index)),
                        Some(ButtonState::Released | ButtonState::Unknown)
                    );
            }
        }
        let result = self
            .resolver
            .tracker
            .transition(BACK, pressed, sender, || Some(poll));
        for source in self.resolver.take_invalidated_sources() {
            if self.hold.as_ref() == Some(&source) {
                assert!(
                    may_invalidate,
                    "invalidated without a lost, unreadable or contested holder: {}",
                    context()
                );
                self.hold = None;
            } else {
                let index = (0..3).find(|&index| mouse(index) == source).unwrap();
                assert!(
                    senderless && !pressed && self.stale[index],
                    "invalidated another source's press: {}",
                    context()
                );
            }
            let before = self.suppressed.len();
            self.suppressed.retain(|held| *held != source);
            if let Some(index) = (0..3).find(|&index| mouse(index) == source) {
                self.cancelled[index] |= self.suppressed.len() < before;
            }
        }
        if senderless && let Some(source) = &result {
            if pressed {
                assert!(self.hold.is_none(), "{}", context());
                self.hold = Some(source.clone());
            } else {
                assert_eq!(
                    self.hold.take().as_ref(),
                    Some(source),
                    "a sender-less release ended a hold it does not own: {}",
                    context()
                );
            }
        } else if !pressed && result.is_some() && self.hold == result {
            self.hold = None;
        }
        result
    }

    /// Mirror the runtime's per-source suppression of `actor`'s edge: a
    /// release is suppressed only where that device's press was, and never
    /// for a press passed through to the OS.
    fn check_runtime(
        &mut self,
        actor: usize,
        pressed: bool,
        result: Option<&EventDevice>,
        context: &dyn Fn() -> String,
    ) {
        let source = result.filter(|source| source.is_logitech());
        let held = source.and_then(|source| self.suppressed.iter().position(|held| held == source));
        if pressed {
            if let (Some(source), None) = (source, held) {
                self.suppressed.push(source.clone());
            }
            self.raw[actor] = source.is_none();
            return;
        }
        let suppressed = held
            .map(|index| self.suppressed.swap_remove(index))
            .is_some();
        assert!(
            !(self.raw[actor] && suppressed),
            "a passed-through press's release was suppressed: {}",
            context()
        );
        assert!(
            suppressed || self.cancelled[actor] || ACTORS[actor] != Actor::IdentifiedLogitech,
            "another source's invalidation cancelled this press: {}",
            context()
        );
        self.raw[actor] = false;
    }
}

/// The sender macOS reports for `actor`'s press or release under `senders`.
fn case_sender(actor: usize, pressed: bool, senders: Senders) -> ButtonSender {
    match ACTORS[actor] {
        Actor::IdentifiedLogitech if !pressed && senders.senderless_release => {
            ButtonSender::Unidentified
        }
        Actor::IdentifiedLogitech => identified(7, mouse(0)),
        Actor::SenderlessLogitech if !pressed && senders.identified_release => {
            identified(9, mouse(1))
        }
        Actor::SecondMouse
            if senders.second == SecondSenders::Identified
                || pressed && senders.second == SecondSenders::IdentifiedPress =>
        {
            identified(8, mouse(2))
        }
        Actor::SenderlessLogitech | Actor::SecondMouse => ButtonSender::Unidentified,
        Actor::Posted => ButtonSender::Posted,
    }
}

fn check_interleaving(order: &[(usize, bool)], case: Case) {
    let Case {
        views,
        unplugged,
        senders,
        early,
    } = case;
    let context = || format!("{order:?} {case:?}");
    let mut model = Model::new();
    let mut down = [false; 3];
    let mut present = [true; 3];
    let mut polled_before = false;
    // Presses delivered without their release.
    let mut pending = [false; 3];

    for (step, &(actor, pressed)) in order.iter().enumerate() {
        if actor < 3 {
            down[actor] = pressed;
        }
        if let Some(&(next, next_pressed)) = order.get(step + 1)
            && next < 3
            && early[next]
        {
            down[next] = next_pressed;
        }
        if actor < 3 && !pressed && unplugged[actor] {
            present[actor] = false;
            continue;
        }
        let sender = case_sender(actor, pressed, senders);
        let senderless = matches!(sender, ButtonSender::Unidentified);
        let poll = (0..3)
            .filter(|&index| present[index])
            .map(|index| {
                let truth = if down[index] {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                let state = match views[index] {
                    PollView::Truthful => truth,
                    PollView::FailsLater if !polled_before => truth,
                    PollView::AlwaysReleased => ButtonState::Released,
                    PollView::Unknown | PollView::FailsLater => ButtonState::Unknown,
                };
                ButtonCandidate {
                    device: mouse(index),
                    state,
                }
            })
            .collect::<Vec<_>>();
        polled_before |= senderless;
        // The holder's release is contested while another recorded press may
        // have just ended.
        let may_invalidate = matches!(views[1], PollView::FailsLater)
            || !present[1]
            || early[1]
            || (0..3).any(|index| {
                index != 1
                    && (pending[index]
                        || matches!(views[index], PollView::Unknown | PollView::FailsLater))
            });
        let result = model.apply(pressed, sender, poll, may_invalidate, &context);
        if actor < 3 {
            pending[actor] = pressed;
            model.check_runtime(actor, pressed, result.as_ref(), &context);
        }

        match ACTORS[actor] {
            Actor::Posted => assert!(result.is_none(), "{}", context()),
            Actor::IdentifiedLogitech if !senderless => {
                assert_eq!(result, Some(mouse(0)), "{}", context());
            }
            _ if !senderless => assert_eq!(result, Some(mouse(actor)), "{}", context()),
            _ => {
                if let Some(source) = &result {
                    assert!(
                        ACTORS[actor] == Actor::SenderlessLogitech && *source == mouse(1),
                        "attributed to a mouse that is not this event's source: {}",
                        context()
                    );
                }
            }
        }
        // An identified press's sender-less release leaves no hold.
        if actor == 0 && !pressed {
            assert!(
                !model.suppressed.contains(&mouse(0)),
                "an identified press outlived its release: {}",
                context()
            );
        }
        if ACTORS[actor] == Actor::SenderlessLogitech && !pressed {
            assert!(
                model.hold.is_none(),
                "hold outlived its owner's release: {}",
                context()
            );
        }
    }

    probe_after(&mut model, views, present, &context);
}

/// After every release, delivered or lost, each Logitech still plugged in
/// gets a clean sender-less click attributed: no permanent pass-through.
/// With none left, the second mouse's click must still expire a lost hold.
fn probe_after(
    model: &mut Model,
    views: [PollView; 3],
    present: [bool; 3],
    context: &dyn Fn() -> String,
) {
    if !present[0] && !present[1] {
        if present[2] {
            let poll = |state| {
                vec![ButtonCandidate {
                    device: mouse(2),
                    state,
                }]
            };
            for (pressed, state) in [(true, ButtonState::Pressed), (false, ButtonState::Released)] {
                let senderless = ButtonSender::Unidentified;
                assert!(
                    model
                        .apply(pressed, senderless, poll(state), true, context)
                        .is_none(),
                    "{}",
                    context()
                );
            }
            assert!(model.hold.is_none(), "hold leaked: {}", context());
            assert!(
                model.resolver.tracker.holds.is_empty(),
                "state leaked: {}",
                context()
            );
        }
        return;
    }
    for probe in [0, 1].into_iter().filter(|&index| present[index]) {
        let poll = |pressed: bool| {
            (0..3)
                .filter(|&index| present[index])
                .map(|index| ButtonCandidate {
                    device: mouse(index),
                    state: if pressed && index == probe {
                        ButtonState::Pressed
                    } else {
                        ButtonState::Released
                    },
                })
                .collect()
        };
        // While a mouse is unreadable, a sender-less release cannot say which
        // refused press it ended, so a stale record may pin the probed mouse,
        // which reads pressed exactly as if it still held that press, or
        // contest its release; only its first clean release proves otherwise.
        let unreadable =
            |index: usize| matches!(views[index], PollView::Unknown | PollView::FailsLater);
        let may_invalidate = !present[1] || (0..3).any(unreadable);
        let mut click = |pressed: bool| {
            model.apply(
                pressed,
                ButtonSender::Unidentified,
                poll(pressed),
                may_invalidate,
                context,
            )
        };
        let first = [click(true), click(false)];
        if first != [Some(mouse(probe)), Some(mouse(probe))] {
            assert!(
                (0..3).any(unreadable),
                "a clean click after the sequence passed through: {}",
                context()
            );
            assert_eq!(click(true), Some(mouse(probe)), "{}", context());
            assert_eq!(click(false), Some(mouse(probe)), "{}", context());
        }
    }
    assert!(model.hold.is_none(), "hold leaked: {}", context());
    assert!(
        model.resolver.tracker.holds.is_empty(),
        "state leaked: {}",
        context()
    );
}
