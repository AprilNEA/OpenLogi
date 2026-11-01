//! libinput's button debouncing, re-applied in front of the pass-through
//! device.
//!
//! A worn switch bounces: one press reaches the kernel as a press, a release
//! and a press again within a few milliseconds, and the desktop sees a double
//! click. libinput filters bounces out of every physical pointer, but since
//! 1.29.2 not out of a virtual one, i.e. any node under
//! `/sys/devices/virtual/input/`, and the pass-through mouse is a uinput
//! device there. libinput leaves the job to whoever creates the virtual
//! device (commit `6524edd9`, "plugin: don't initialize the debounce plugin
//! on a virtual device"), so the hook debounces before the callback sees a
//! button (#2244).
//!
//! The state machine is libinput's (`libinput-plugin-button-debounce.c`),
//! state for state. Its rules:
//! - The first change of a button at rest is delivered at once.
//! - A change within [`BOUNCE_TIMEOUT`] of the one before is a bounce. It is
//!   held until the button has been still for that long, so a bounce back to
//!   where the button was is dropped.
//! - A release that a press follows within [`SPURIOUS_TIMEOUT`], with the
//!   button then held, is a spurious release: the switch lost contact while
//!   held down. From the first one on, every release is held for that long
//!   and dropped together with a press that follows it.
//! - One button is tracked at a time. A change of another button, or of
//!   several in one report, delivers what is held and starts over.
//!
//! libinput before 1.29.2 also debounces the pass-through device, so there a
//! click passes both filters in series.

use std::time::{Duration, Instant};

use evdev::{BusType, EventSummary, EventType, InputEvent, InputId, KeyCode};

use crate::LOGITECH_VENDOR_ID;

/// libinput's `DEBOUNCE_TIMEOUT_BOUNCE`.
const BOUNCE_TIMEOUT: Duration = Duration::from_millis(25);
/// libinput's `DEBOUNCE_TIMEOUT_SPURIOUS`.
const SPURIOUS_TIMEOUT: Duration = Duration::from_millis(12);

/// The Logitech USB products libinput never debounces: the K400, K400r, K830
/// and K400 Plus touchpad keyboards, marked `ModelBouncingKeys` in its
/// `quirks/30-vendor-logitech.quirks`. The hook grabs only Logitech mice, so
/// no other entry of that quirk can apply.
const NEVER_DEBOUNCED: [u16; 4] = [0x4024, 0x404B, 0x404C, 0x404D];

/// A button going down or up, as one evdev key event reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ButtonChange {
    pub(super) button: KeyCode,
    pub(super) pressed: bool,
}

impl TryFrom<InputEvent> for ButtonChange {
    type Error = ();

    /// The change `event` reports, if it is a key event of a button libinput
    /// debounces. Like libinput, any non-zero value is a press.
    fn try_from(event: InputEvent) -> Result<Self, Self::Error> {
        match event.destructure() {
            EventSummary::Key(_, button, value) if is_button(button) => Ok(Self {
                button,
                pressed: value != 0,
            }),
            _ => Err(()),
        }
    }
}

impl From<ButtonChange> for InputEvent {
    fn from(change: ButtonChange) -> Self {
        Self::new(EventType::KEY.0, change.button.0, i32::from(change.pressed))
    }
}

/// Whether libinput debounces `code`: its `evdev_usage_is_button` from
/// `BTN_MISC` (`BTN_0`) up to the digitizer tools. Above those it also counts
/// stylus and gear buttons, which a grabbed relative mouse does not have.
fn is_button(code: KeyCode) -> bool {
    (KeyCode::BTN_0.0..KeyCode::BTN_TOOL_PEN.0).contains(&code.0)
}

/// libinput's `DEBOUNCE_STATE_*`, minus the prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    IsUp,
    IsDown,
    IsDownWaiting,
    IsUpDelaying,
    IsUpDelayingSpurious,
    IsUpDetectingSpurious,
    IsDownDetectingSpurious,
    IsUpWaiting,
    IsDownDelaying,
    /// Debouncing is off for this device: every change passes at once.
    Disabled,
}

/// libinput's `DEBOUNCE_EVENT_*`, minus the prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Press,
    Release,
    /// The [`BOUNCE_TIMEOUT`] timer ran out.
    Timeout,
    /// The [`SPURIOUS_TIMEOUT`] timer ran out.
    TimeoutShort,
    /// Another button changed, or several did in one report.
    OtherButton,
}

/// The button-debounce state of one grabbed mouse.
#[derive(Debug)]
pub(super) struct ButtonDebounce {
    state: State,
    /// The button `state` is about. `None` until the first button change.
    button: Option<KeyCode>,
    /// Set for good by the first spurious release.
    spurious: bool,
    /// When libinput's `timer` runs out.
    bounce_at: Option<Instant>,
    /// When libinput's `timer_short` runs out.
    spurious_at: Option<Instant>,
    /// The button changes of the report being read.
    report: Vec<ButtonChange>,
}

impl ButtonDebounce {
    /// The state for a newly grabbed device `id`. Debouncing is off where
    /// libinput turns it off.
    pub(super) fn for_device(id: &InputId) -> Self {
        let never = id.bus_type() == BusType::BUS_USB
            && u32::from(id.vendor()) == LOGITECH_VENDOR_ID
            && NEVER_DEBOUNCED.contains(&id.product());
        Self {
            state: if never { State::Disabled } else { State::IsUp },
            button: None,
            spurious: false,
            bounce_at: None,
            spurious_at: None,
            report: Vec::new(),
        }
    }

    /// Take a button change of the report being read. It is held until the
    /// report ends.
    pub(super) fn changed(&mut self, change: ButtonChange) {
        self.report.push(change);
    }

    /// End the report being read, at `now`. Like libinput, run the timers due
    /// by then first. Returns the button changes to deliver, in order.
    pub(super) fn end_report(&mut self, now: Instant) -> Vec<ButtonChange> {
        let mut deliver = self.expire(now);
        let mut report = std::mem::take(&mut self.report);
        // libinput's `debounce_plugin_handle_frame`.
        let several = report.len() > 1;
        let mut flushed = false;
        if let Some(first) = report.first()
            && (several || self.button != Some(first.button))
        {
            deliver.extend(self.handle(Event::OtherButton, now));
            flushed = true;
        }
        for change in &report {
            if flushed && self.state != State::Disabled {
                // The flush left the state at rest. Which way is up to the
                // change: a button is released only from down.
                self.state = if change.pressed {
                    State::IsUp
                } else {
                    State::IsDown
                };
                flushed = false;
            }
            self.button = Some(change.button);
            let event = if change.pressed {
                Event::Press
            } else {
                Event::Release
            };
            deliver.extend(self.handle(event, now));
            if several {
                deliver.extend(self.handle(Event::OtherButton, now));
                flushed = true;
            }
        }
        report.clear();
        self.report = report;
        deliver
    }

    /// When the next timer runs out, if one runs. The device thread must
    /// wake by then even without a report, to deliver a held change.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.bounce_at.into_iter().chain(self.spurious_at).min()
    }

    /// Run the timers due at `now`. Returns the button changes to deliver.
    pub(super) fn expire(&mut self, now: Instant) -> Vec<ButtonChange> {
        let mut deliver = Vec::new();
        while let Some(event) = self.take_due(now) {
            deliver.extend(self.handle(event, now));
        }
        deliver
    }

    /// Deliver a release that is still held, for a device thread that
    /// stops: the callback must not keep a button down that the user let go
    /// of. A held press stays undelivered: the device goes away with the
    /// button down.
    pub(super) fn release_held(&mut self, now: Instant) -> Option<ButtonChange> {
        match self.state {
            State::IsUpDelaying | State::IsUpDelayingSpurious => {
                self.handle(Event::OtherButton, now)
            }
            _ => None,
        }
    }

    /// Stop the timer due at `now`, if any, and name its event. The short
    /// timer is only ever set together with the bounce timer, so when both
    /// are due it is the earlier one.
    fn take_due(&mut self, now: Instant) -> Option<Event> {
        if self.spurious_at.is_some_and(|at| at <= now) {
            self.spurious_at = None;
            return Some(Event::TimeoutShort);
        }
        if self.bounce_at.is_some_and(|at| at <= now) {
            self.bounce_at = None;
            return Some(Event::Timeout);
        }
        None
    }

    /// libinput's `debounce_handle_event` and the per-state handlers it
    /// dispatches to. Returns the change to deliver, if any.
    fn handle(&mut self, event: Event, now: Instant) -> Option<ButtonChange> {
        use Event::{OtherButton, Press, Release, Timeout, TimeoutShort};
        use State::{
            Disabled, IsDown, IsDownDelaying, IsDownDetectingSpurious, IsDownWaiting, IsUp,
            IsUpDelaying, IsUpDelayingSpurious, IsUpDetectingSpurious, IsUpWaiting,
        };

        if event == OtherButton {
            self.cancel_timers();
        }
        match (self.state, event) {
            (IsUp, Press) => {
                self.start_bounce_timer(now);
                self.state = IsDownWaiting;
                self.press()
            }
            (IsDown, Release) => {
                self.start_bounce_timer(now);
                self.start_spurious_timer(now);
                if self.spurious {
                    self.state = IsUpDelayingSpurious;
                    None
                } else {
                    self.state = IsUpDetectingSpurious;
                    self.release()
                }
            }
            (IsDownWaiting, Release) => {
                self.start_bounce_timer(now);
                self.state = IsUpDelaying;
                None
            }
            (IsDownWaiting, Timeout | OtherButton) => {
                self.state = IsDown;
                None
            }
            (IsUpDelaying, Press) => {
                self.start_bounce_timer(now);
                self.state = IsDownWaiting;
                None
            }
            (IsUpDelaying, Timeout | OtherButton) | (IsUpDelayingSpurious, OtherButton) => {
                self.state = IsUp;
                self.release()
            }
            (IsUpDelayingSpurious, Press) => {
                self.cancel_timers();
                self.state = IsDown;
                None
            }
            (IsUpDelayingSpurious, TimeoutShort) => {
                self.state = IsUpWaiting;
                self.release()
            }
            (IsUpDetectingSpurious, Press) => {
                self.start_bounce_timer(now);
                self.start_spurious_timer(now);
                self.state = IsDownDetectingSpurious;
                None
            }
            (IsUpDetectingSpurious | IsUpWaiting, Timeout | OtherButton) => {
                self.state = IsUp;
                None
            }
            (IsUpDetectingSpurious, TimeoutShort) => {
                self.state = IsUpWaiting;
                None
            }
            (IsDownDetectingSpurious, Release) => {
                self.start_bounce_timer(now);
                self.start_spurious_timer(now);
                self.state = IsUpDetectingSpurious;
                None
            }
            (IsDownDetectingSpurious, TimeoutShort) => {
                self.bounce_at = None;
                self.state = IsDown;
                self.spurious = true;
                self.press()
            }
            (IsDownDetectingSpurious | IsDownDelaying, Timeout | OtherButton) => {
                self.state = IsDown;
                self.press()
            }
            (IsUpWaiting, Press) => {
                self.start_bounce_timer(now);
                self.state = IsDownDelaying;
                None
            }
            (IsDownDelaying, Release) => {
                self.start_bounce_timer(now);
                self.state = IsUpWaiting;
                None
            }
            (Disabled, Press) => self.press(),
            (Disabled, Release) => self.release(),
            // `IsDown` ignores a press: libinput takes it for a release lost
            // while something else held the grab. The other pairs are ones
            // libinput logs as its own bug, and it changes nothing for them.
            _ => None,
        }
    }

    fn press(&self) -> Option<ButtonChange> {
        self.button.map(|button| ButtonChange {
            button,
            pressed: true,
        })
    }

    fn release(&self) -> Option<ButtonChange> {
        self.button.map(|button| ButtonChange {
            button,
            pressed: false,
        })
    }

    fn start_bounce_timer(&mut self, now: Instant) {
        self.bounce_at = Some(now + BOUNCE_TIMEOUT);
    }

    fn start_spurious_timer(&mut self, now: Instant) {
        self.spurious_at = Some(now + SPURIOUS_TIMEOUT);
    }

    fn cancel_timers(&mut self) {
        self.bounce_at = None;
        self.spurious_at = None;
    }
}

#[cfg(test)]
mod tests {
    //! The scenarios of libinput's own debounce tests (`test/test-pointer.c`,
    //! `debounce_*`), at the times they run at: events with nothing between
    //! them 1 ms apart, and a `litest_timeout_debounce` as 30 ms of rest.

    use super::*;

    const LEFT: KeyCode = KeyCode::BTN_LEFT;
    const RIGHT: KeyCode = KeyCode::BTN_RIGHT;

    fn press(button: KeyCode) -> ButtonChange {
        ButtonChange {
            button,
            pressed: true,
        }
    }

    fn release(button: KeyCode) -> ButtonChange {
        ButtonChange {
            button,
            pressed: false,
        }
    }

    fn mouse() -> InputId {
        InputId::new(BusType::BUS_USB, 0x046D, 0xB034, 0)
    }

    /// Drives a [`ButtonDebounce`] as the device thread does, waking at each
    /// deadline, and records every delivery with its time in milliseconds.
    struct Run {
        debounce: ButtonDebounce,
        start: Instant,
        delivered: Vec<(u64, ButtonChange)>,
    }

    impl Run {
        fn new() -> Self {
            Self::of(&mouse())
        }

        fn of(id: &InputId) -> Self {
            Self {
                debounce: ButtonDebounce::for_device(id),
                start: Instant::now(),
                delivered: Vec::new(),
            }
        }

        fn at(&self, ms: u64) -> Instant {
            self.start + Duration::from_millis(ms)
        }

        fn record(&mut self, at: Instant, changes: Vec<ButtonChange>) {
            let ms = u64::try_from(at.duration_since(self.start).as_millis()).unwrap();
            self.delivered
                .extend(changes.into_iter().map(|change| (ms, change)));
        }

        /// Stay idle until `ms`, waking at each deadline before then.
        fn idle(&mut self, ms: u64) -> &mut Self {
            let until = self.at(ms);
            while let Some(deadline) = self.debounce.deadline().filter(|&at| at <= until) {
                let delivered = self.debounce.expire(deadline);
                self.record(deadline, delivered);
            }
            self
        }

        /// A report of `changes` that ends at `ms`.
        fn report(&mut self, ms: u64, changes: &[ButtonChange]) -> &mut Self {
            self.idle(ms);
            for &change in changes {
                self.debounce.changed(change);
            }
            let now = self.at(ms);
            let delivered = self.debounce.end_report(now);
            self.record(now, delivered);
            self
        }

        /// libinput's `debounce_trigger_spurious`, from 0 ms to 110 ms: it
        /// turns spurious-release debouncing on.
        fn trigger_spurious(&mut self) -> &mut Self {
            self.report(0, &[press(LEFT)])
                .report(40, &[release(LEFT)])
                .report(41, &[press(LEFT)])
                .idle(71)
                .report(80, &[release(LEFT)])
                .idle(110);
            assert_eq!(
                self.delivered,
                [
                    (0, press(LEFT)),
                    (40, release(LEFT)),
                    (53, press(LEFT)),
                    (92, release(LEFT)),
                ],
                "the spurious release at 40 ms is delivered, the press after it 12 ms late, \
                 and from then on a release waits 12 ms"
            );
            self.delivered.clear();
            self
        }
    }

    /// `debounce_bounce`: a press that bounces and a release that bounces
    /// each reach the desktop once.
    #[test]
    fn a_bouncing_press_and_release_are_delivered_once_each() {
        let mut run = Run::new();
        run.report(0, &[press(LEFT)])
            .report(1, &[release(LEFT)])
            .report(2, &[press(LEFT)])
            .idle(32)
            .report(40, &[release(LEFT)])
            .report(41, &[press(LEFT)])
            .report(42, &[release(LEFT)])
            .idle(72);
        assert_eq!(run.delivered, [(0, press(LEFT)), (40, release(LEFT))]);
        assert_eq!(run.debounce.deadline(), None);
    }

    /// `debounce_bounce_high_delay`: each bounce restarts the timeout, so a
    /// bounce longer than the timeout as a whole is still one change.
    #[test]
    fn every_bounce_restarts_the_timeout() {
        let mut run = Run::new();
        run.report(0, &[press(LEFT)])
            .report(15, &[release(LEFT)])
            .report(30, &[press(LEFT)])
            .idle(60)
            .report(70, &[release(LEFT)])
            .report(85, &[press(LEFT)])
            .report(100, &[release(LEFT)])
            .idle(130);
        assert_eq!(run.delivered, [(0, press(LEFT)), (70, release(LEFT))]);
    }

    /// `debounce_bounce_check_immediate`: a click that does not bounce is
    /// delivered without delay.
    #[test]
    fn a_clean_click_is_not_delayed() {
        let mut run = Run::new();
        run.report(0, &[press(LEFT)])
            .idle(30)
            .report(40, &[release(LEFT)])
            .idle(70);
        assert_eq!(run.delivered, [(0, press(LEFT)), (40, release(LEFT))]);
    }

    /// A click faster than the timeout is not dropped: its release waits
    /// until the button has been still for the timeout.
    #[test]
    fn a_fast_click_delays_its_release() {
        let mut run = Run::new();
        run.report(0, &[press(LEFT)])
            .report(10, &[release(LEFT)])
            .idle(60);
        assert_eq!(run.delivered, [(0, press(LEFT)), (35, release(LEFT))]);
    }

    /// `debounce_spurious`: once on, a release that a press follows within
    /// 12 ms is dropped with it, and a lone release arrives 12 ms late.
    #[test]
    fn spurious_releases_are_dropped_once_detected() {
        let mut run = Run::new();
        run.trigger_spurious();
        for i in 0..3 {
            let t = 200 + 100 * i;
            run.report(t, &[press(LEFT)])
                .idle(t + 30)
                .report(t + 40, &[release(LEFT)])
                .report(t + 41, &[press(LEFT)])
                .report(t + 42, &[release(LEFT)])
                .idle(t + 72);
            assert_eq!(
                run.delivered,
                [(t, press(LEFT)), (t + 54, release(LEFT))],
                "click {i}"
            );
            run.delivered.clear();
        }
    }

    /// `debounce_spurious_multibounce`: a button that chatters while held
    /// is one press and one release.
    #[test]
    fn chatter_while_held_is_one_click() {
        let mut run = Run::new();
        run.trigger_spurious();
        run.report(200, &[press(LEFT)]).idle(230);
        for (i, t) in (240..247).enumerate() {
            let change = if i % 2 == 0 {
                release(LEFT)
            } else {
                press(LEFT)
            };
            run.report(t, &[change]);
        }
        run.idle(276);
        assert_eq!(run.delivered, [(200, press(LEFT)), (258, release(LEFT))]);
    }

    /// `debounce_spurious_trigger_high_delay`: contact lost twice within
    /// 12 ms of each change also turns spurious debouncing on.
    #[test]
    fn a_slow_spurious_bounce_turns_spurious_debouncing_on() {
        let mut run = Run::new();
        run.report(0, &[press(LEFT)])
            .idle(30)
            .report(40, &[release(LEFT)])
            .report(45, &[press(LEFT)])
            .report(50, &[release(LEFT)])
            .report(55, &[press(LEFT)])
            .idle(85)
            .report(90, &[release(LEFT)])
            .report(91, &[press(LEFT)])
            .idle(121)
            .report(130, &[release(LEFT)])
            .idle(160);
        assert_eq!(
            run.delivered,
            [
                (0, press(LEFT)),
                (40, release(LEFT)),
                (67, press(LEFT)),
                (142, release(LEFT)),
            ]
        );
    }

    /// `debounce_spurious_dont_enable_on_otherbutton`: a release and press
    /// with another button's change between them are not spurious.
    #[test]
    fn another_button_between_release_and_press_is_not_spurious() {
        let mut run = Run::new();
        run.report(0, &[press(LEFT)])
            .idle(30)
            .report(40, &[release(LEFT)])
            .report(41, &[press(RIGHT)])
            .report(42, &[press(LEFT)])
            .report(43, &[release(LEFT)])
            .report(44, &[release(RIGHT)])
            .idle(80);
        assert_eq!(
            run.delivered,
            [
                (0, press(LEFT)),
                (40, release(LEFT)),
                (41, press(RIGHT)),
                (42, press(LEFT)),
                (44, release(LEFT)),
                (44, release(RIGHT)),
            ]
        );
        run.delivered.clear();
        run.report(80, &[press(LEFT)])
            .idle(110)
            .report(120, &[release(LEFT)]);
        assert_eq!(
            run.delivered,
            [(80, press(LEFT)), (120, release(LEFT))],
            "spurious debouncing stays off, so the release is immediate"
        );
    }

    /// `debounce_spurious_cancel_debounce_otherbutton`: another button's
    /// change delivers the held release at once.
    #[test]
    fn another_button_delivers_a_held_spurious_release() {
        let mut run = Run::new();
        run.trigger_spurious();
        run.report(200, &[press(LEFT)])
            .idle(230)
            .report(240, &[release(LEFT)])
            .report(241, &[press(RIGHT)])
            .report(242, &[press(LEFT)])
            .report(243, &[release(LEFT)])
            .report(244, &[release(RIGHT)])
            .idle(280);
        assert_eq!(
            run.delivered,
            [
                (200, press(LEFT)),
                (241, release(LEFT)),
                (241, press(RIGHT)),
                (242, press(LEFT)),
                (244, release(LEFT)),
                (256, release(RIGHT)),
            ]
        );
    }

    /// `debounce_spurious_switch_to_otherbutton`: the next button is
    /// debounced on its own.
    #[test]
    fn the_other_button_bounces_on_its_own() {
        let mut run = Run::new();
        run.trigger_spurious();
        run.report(200, &[press(LEFT)])
            .idle(230)
            .report(240, &[release(LEFT)])
            .report(241, &[press(RIGHT)])
            .report(242, &[release(RIGHT)])
            .report(243, &[press(RIGHT)])
            .report(244, &[release(RIGHT)])
            .idle(280);
        assert_eq!(
            run.delivered,
            [
                (200, press(LEFT)),
                (241, release(LEFT)),
                (241, press(RIGHT)),
                (269, release(RIGHT)),
            ]
        );
    }

    /// libinput does not debounce a report that changes several buttons.
    #[test]
    fn several_buttons_in_one_report_pass_at_once() {
        let mut run = Run::new();
        run.report(0, &[press(LEFT), press(RIGHT)])
            .report(5, &[release(LEFT), release(RIGHT)]);
        assert_eq!(
            run.delivered,
            [
                (0, press(LEFT)),
                (0, press(RIGHT)),
                (5, release(LEFT)),
                (5, release(RIGHT)),
            ]
        );
        assert_eq!(run.debounce.deadline(), None);
    }

    /// A report read after a deadline the thread slept through runs that
    /// timer first. Otherwise the delayed release of the first click and
    /// the press of the second would cancel out as a bounce.
    #[test]
    fn a_late_report_runs_the_timers_due_before_it() {
        let mut run = Run::new();
        run.report(0, &[press(LEFT)]).report(10, &[release(LEFT)]);
        run.debounce.changed(press(LEFT));
        let delivered = run.debounce.end_report(run.at(50));
        assert_eq!(delivered, [release(LEFT), press(LEFT)]);
    }

    /// The deadline is the earlier of the two timers.
    #[test]
    fn the_deadline_is_the_next_timer() {
        let mut run = Run::new();
        assert_eq!(run.debounce.deadline(), None);
        run.report(0, &[press(LEFT)]);
        assert_eq!(run.debounce.deadline(), Some(run.at(25)));
        run.idle(30).report(40, &[release(LEFT)]);
        assert_eq!(run.debounce.deadline(), Some(run.at(52)));
    }

    /// libinput's `ModelBouncingKeys` devices pass every change at once.
    #[test]
    fn quirked_keyboards_are_not_debounced() {
        for product in NEVER_DEBOUNCED {
            let mut run = Run::of(&InputId::new(BusType::BUS_USB, 0x046D, product, 0));
            run.report(0, &[press(LEFT)])
                .report(1, &[release(LEFT)])
                .report(2, &[press(LEFT)])
                .report(3, &[release(LEFT)]);
            assert_eq!(
                run.delivered,
                [
                    (0, press(LEFT)),
                    (1, release(LEFT)),
                    (2, press(LEFT)),
                    (3, release(LEFT)),
                ],
                "product {product:#06x}"
            );
            assert_eq!(run.debounce.deadline(), None);
        }
    }

    /// The quirk matches USB only, as libinput's entries do.
    #[test]
    fn a_quirked_product_on_another_bus_is_debounced() {
        let mut run = Run::of(&InputId::new(BusType::BUS_BLUETOOTH, 0x046D, 0x404C, 0));
        run.report(0, &[press(LEFT)])
            .report(1, &[release(LEFT)])
            .report(2, &[press(LEFT)])
            .idle(30);
        assert_eq!(run.delivered, [(0, press(LEFT))]);
    }

    /// A stopping thread gets the release it holds, never a held press.
    #[test]
    fn release_held_delivers_only_a_held_release() {
        let mut run = Run::new();
        run.report(0, &[press(LEFT)]).report(10, &[release(LEFT)]);
        assert_eq!(run.debounce.release_held(run.at(12)), Some(release(LEFT)));
        assert_eq!(run.debounce.deadline(), None);
        assert_eq!(run.debounce.release_held(run.at(13)), None);

        let mut run = Run::new();
        run.report(0, &[press(LEFT)])
            .idle(30)
            .report(40, &[release(LEFT)])
            .idle(55)
            .report(60, &[press(LEFT)]);
        assert_eq!(
            run.delivered,
            [(0, press(LEFT)), (40, release(LEFT))],
            "the press at 60 ms is held as a possible bounce"
        );
        assert_eq!(run.debounce.release_held(run.at(62)), None);
    }

    /// Key events of buttons convert; anything else does not. A repeat
    /// (value 2) is a press, as libinput reads it.
    #[test]
    fn button_changes_convert_from_and_into_key_events() {
        let key = |code: KeyCode, value| InputEvent::new(EventType::KEY.0, code.0, value);
        assert_eq!(ButtonChange::try_from(key(LEFT, 1)), Ok(press(LEFT)));
        assert_eq!(ButtonChange::try_from(key(LEFT, 0)), Ok(release(LEFT)));
        assert_eq!(ButtonChange::try_from(key(LEFT, 2)), Ok(press(LEFT)));
        assert_eq!(
            ButtonChange::try_from(key(KeyCode::BTN_TASK, 1)),
            Ok(press(KeyCode::BTN_TASK))
        );
        for event in [
            key(KeyCode::KEY_A, 1),
            key(KeyCode::BTN_TOUCH, 1),
            key(KeyCode::BTN_TOOL_PEN, 1),
            InputEvent::new(EventType::RELATIVE.0, LEFT.0, 1),
        ] {
            assert_eq!(ButtonChange::try_from(event), Err(()), "{event:?}");
        }
        let event = InputEvent::from(release(RIGHT));
        assert_eq!(
            (event.event_type(), event.code(), event.value()),
            (EventType::KEY, RIGHT.0, 0)
        );
        assert_eq!(InputEvent::from(press(RIGHT)).value(), 1);
    }
}
