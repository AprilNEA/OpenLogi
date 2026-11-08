//! The channel's pending requests: what is awaiting a reply, and which
//! headers stay reserved for replies nobody waits for any more.
//!
//! [`PendingQueue`] owns the lifecycle of every [`CorrelationKey`] — free, in
//! flight, or stale ([`KeyState`]) — and is the only code that moves a key
//! between those states. A [`PendingRequest`] is one caller's entry in it,
//! parked until its key is free and abandoned on drop. The parent module's
//! read loop hands every incoming report to [`PendingQueue::resolve`], and its
//! module docs say why two requests never share a header.

use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Instant,
};

use futures::{FutureExt, channel::oneshot, select};
use tracing::trace;

use super::{
    AbandonedReply, ChannelError, HidppMessage, RequestOutcome, STALE_REPLY_GRACE,
    observation::RequestObservation,
};
use crate::sync::lock;

#[cfg(test)]
mod tests;

/// The bytes every reply is matched on before any predicate runs: device
/// index, feature index (HID++1.0: sub id), and function/software id
/// (HID++1.0: register address) — the first three payload bytes of every
/// report, on both report kinds. Two requests that share them get replies
/// nothing on the wire can tell apart.
type CorrelationKey = (u8, u8, u8);

/// Every correlation key that is not free, plus the requests parked until
/// theirs is.
#[derive(Default)]
pub(super) struct PendingQueue {
    /// Each held key with its state, in the order it entered that state —
    /// the order replies are matched in: a reply answers the oldest in-flight
    /// request that accepts it, or else is discarded against the oldest stale
    /// key that accepts it. A key absent here is free. One present is present
    /// once: entering a state goes through [`PendingQueue::hold`].
    keys: VecDeque<HeldKey>,

    /// Woken whenever a key leaves the in-flight state or its last owed reply
    /// is discarded, so a parked request can re-check whether its key is
    /// free.
    key_waiters: Vec<oneshot::Sender<()>>,
}

/// A key the queue holds: the request it was taken for, and where in its
/// lifecycle it is.
struct HeldKey {
    correlation: Correlation,
    state: KeyState,
}

/// How a request's replies are recognised. A key keeps this from the request
/// that took it for as long as it is held.
struct Correlation {
    /// The header bytes the replies carry.
    key: CorrelationKey,

    /// The request as sent, so a byte-identical re-ask that asks to adopt
    /// the outstanding replies can be recognised.
    request: HidppMessage,

    /// The predicate that has to match for an incoming message to be
    /// classified as a reply.
    response_predicate: Box<dyn Fn(&HidppMessage) -> bool + Send>,
}

/// Where a held key is in its lifecycle. A key the queue does not hold is
/// free.
///
/// Every transition is a [`PendingQueue`] method:
///
/// - free → in flight: a request registers ([`PendingQueue::try_register`]).
/// - in flight → free: its reply lands with nothing more owed
///   ([`PendingQueue::resolve`]).
/// - in flight → stale: the requester gives up — a timeout, a cancellation,
///   a failed write ([`PendingQueue::abandon`]) — or its reply lands while
///   adopted replies are still owed ([`PendingQueue::resolve`]).
/// - stale → in flight: a byte-identical re-ask under
///   [`AbandonedReply::AdoptIdentical`] adopts the owed replies
///   ([`PendingQueue::try_register`]).
/// - stale → stale: one of several owed replies is discarded
///   ([`PendingQueue::resolve`]).
/// - stale → free: the last owed reply is discarded
///   ([`PendingQueue::resolve`]), or the grace runs out
///   ([`PendingQueue::try_register`] prunes it).
enum KeyState {
    /// A registered request awaits its reply.
    InFlight(InFlight),
    /// No request waits for the replies with this key, but some are still
    /// owed.
    Stale(Stale),
}

/// The in-flight state: the request awaiting the reply.
struct InFlight {
    /// Unique ID used to find this request when its waiter goes away.
    id: u64,

    /// Hands the reply to the waiting request.
    sender: oneshot::Sender<HidppMessage>,

    /// Replies still owed to earlier, abandoned sends of this same request,
    /// adopted on registration under [`AbandonedReply::AdoptIdentical`].
    /// Whichever reply comes first answers this request; the rest are then
    /// owed under a stale key.
    extra_replies: usize,
}

/// The stale state: replies owed to a request nobody waits for any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stale {
    /// How many replies are still owed: one per time the request went out
    /// unanswered.
    outstanding: NonZeroUsize,

    /// When the key is given up on even without them.
    expires: Instant,
}

/// A request asking to take its key; registering it puts the key in flight.
struct Registration {
    id: u64,
    correlation: Correlation,

    /// What to do about replies still owed to an abandoned request with the
    /// same header.
    abandoned: AbandonedReply,

    sender: oneshot::Sender<HidppMessage>,
}

/// What a request that could not register has to wait for.
#[derive(Debug, PartialEq, Eq)]
enum Wait {
    /// A pending request with the same key; until it leaves the queue.
    InFlight,
    /// An abandoned request's replies with this key are still outstanding;
    /// until they are discarded, or the given instant at the latest.
    Stale(Instant),
}

/// What an incoming report was to the pending requests.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Resolution {
    /// The reply of the in-flight request with this ID, now handed to it.
    Answered(u64),
    /// A reply still owed to an abandoned request, discarded.
    Discarded,
    /// Nothing a request of this channel waits for or is owed.
    Unmatched,
}

impl PendingQueue {
    /// Registers `registration` if nothing holds its key as of `now`, else
    /// hands it back with what it is waiting for. Prunes stale keys whose
    /// grace has passed.
    ///
    /// A stale key blocks whatever the new request's bytes are: byte equality
    /// with the abandoned request does not by itself make the outstanding
    /// replies its answer, since a write may have gone between the two asks.
    /// Only a request that asks to ([`AbandonedReply::AdoptIdentical`]) and
    /// is byte-identical registers at once and adopts them — it is answered
    /// by whichever reply comes first, and the rest stay owed (see
    /// [`InFlight::extra_replies`]).
    fn try_register(
        &mut self,
        registration: Registration,
        now: Instant,
    ) -> Result<(), (Registration, Wait)> {
        self.keys.retain(|held| !held.state.expired(now));
        let key = registration.correlation.key;
        let adopted = match self
            .keys
            .iter()
            .position(|held| held.correlation.key == key)
        {
            None => None,
            Some(pos) => {
                let held = &self.keys[pos];
                let stale = match held.state {
                    KeyState::InFlight(_) => return Err((registration, Wait::InFlight)),
                    KeyState::Stale(stale) => stale,
                };
                if !registration.adopts(&held.correlation) {
                    return Err((registration, Wait::Stale(stale.expires)));
                }
                self.keys.remove(pos);
                Some(stale)
            }
        };
        self.hold(registration.register(adopted));
        Ok(())
    }

    /// Gives up on the request with `id`, if it is still awaiting its reply:
    /// it leaves the queue but its key stays stale for [`STALE_REPLY_GRACE`],
    /// so the reply — should it still come — is discarded rather than handed
    /// to the next request with that key. Wakes parked requests so they
    /// re-check what they are waiting for.
    ///
    /// A request whose write failed is abandoned the same way. The transport's
    /// error does not say whether the report left —
    /// [`RawHidChannel::write_report`] promises nothing either way — and a
    /// reply it did provoke would answer the next request with this key.
    /// Replies it adopted are owed regardless. The price is one grace window
    /// for the next same-header request on a channel whose write just failed.
    ///
    /// [`RawHidChannel::write_report`]: super::RawHidChannel::write_report
    fn abandon(&mut self, id: u64, now: Instant) {
        let found = self
            .keys
            .iter()
            .enumerate()
            .find_map(|(pos, held)| match &held.state {
                KeyState::InFlight(in_flight) if in_flight.id == id => {
                    Some((pos, in_flight.abandoned(now)))
                }
                KeyState::InFlight(_) | KeyState::Stale(_) => None,
            });
        let Some((pos, stale)) = found else {
            return;
        };
        if let Some(mut held) = self.keys.remove(pos) {
            // Drops the request's sender: its requester has given up.
            held.state = KeyState::Stale(stale);
            self.hold(held);
        }
        self.wake_key_waiters();
    }

    /// Resolves an incoming report: the reply of the oldest in-flight request
    /// that accepts it, or else an owed reply to the oldest stale key that
    /// accepts it, discarded. Discarding the last owed reply is what frees a
    /// stale key's header for the next request.
    pub(super) fn resolve(&mut self, reply: HidppMessage, now: Instant) -> Resolution {
        let accepts = |held: &HeldKey| (held.correlation.response_predicate)(&reply);
        let Some(pos) = self
            .keys
            .iter()
            .position(|held| matches!(held.state, KeyState::InFlight(_)) && accepts(held))
            .or_else(|| self.keys.iter().position(accepts))
        else {
            return Resolution::Unmatched;
        };
        let Some(HeldKey { correlation, state }) = self.keys.remove(pos) else {
            return Resolution::Unmatched;
        };
        let resolution = match state {
            KeyState::InFlight(in_flight) => {
                let id = in_flight.id;
                if let Some(stale) = in_flight.answer(reply, now) {
                    // Answered by one of several replies it was owed: the
                    // others are still coming and must not answer the next
                    // request with this header.
                    self.hold(HeldKey {
                        correlation,
                        state: KeyState::Stale(stale),
                    });
                }
                Resolution::Answered(id)
            }
            KeyState::Stale(stale) => {
                if let Some(rest) = stale.discard_one() {
                    // Still stale: it keeps its place, and nothing was freed
                    // to wake for.
                    self.keys.insert(
                        pos,
                        HeldKey {
                            correlation,
                            state: KeyState::Stale(rest),
                        },
                    );
                    return Resolution::Discarded;
                }
                Resolution::Discarded
            }
        };
        self.wake_key_waiters();
        resolution
    }

    /// How many requests are awaiting their reply.
    pub(super) fn in_flight_count(&self) -> usize {
        self.keys
            .iter()
            .filter(|held| matches!(held.state, KeyState::InFlight(_)))
            .count()
    }

    /// How many keys are stale.
    #[cfg(test)]
    pub(super) fn stale_count(&self) -> usize {
        self.keys
            .iter()
            .filter(|held| matches!(held.state, KeyState::Stale(_)))
            .count()
    }

    /// Puts `held` at the back: its key has just entered its state. Every
    /// caller has just found the key free or taken it out, so a key is never
    /// held twice.
    fn hold(&mut self, held: HeldKey) {
        debug_assert!(
            self.keys
                .iter()
                .all(|other| other.correlation.key != held.correlation.key),
            "correlation key {:?} is already held",
            held.correlation.key
        );
        self.keys.push_back(held);
    }

    fn wake_key_waiters(&mut self) {
        for waiter in self.key_waiters.drain(..) {
            // A parked request that was cancelled meanwhile has dropped its
            // receiver; nothing to wake.
            let _ = waiter.send(());
        }
    }
}

impl KeyState {
    /// Whether the key is stale and its grace has passed as of `now`.
    fn expired(&self, now: Instant) -> bool {
        matches!(self, Self::Stale(stale) if stale.expires <= now)
    }
}

impl InFlight {
    /// The stale state an abandoned request leaves its key in: its own reply
    /// is owed, plus any it had adopted from earlier abandoned sends of the
    /// same request.
    fn abandoned(&self, now: Instant) -> Stale {
        Stale::owing(NonZeroUsize::MIN.saturating_add(self.extra_replies), now)
    }

    /// Hands `reply` to the waiting request, and returns the stale state the
    /// key stays in while adopted replies are still owed.
    fn answer(self, reply: HidppMessage, now: Instant) -> Option<Stale> {
        // The waiter may have given up meanwhile; the reply is consumed
        // either way.
        let _ = self.sender.send(reply);
        NonZeroUsize::new(self.extra_replies).map(|outstanding| Stale::owing(outstanding, now))
    }
}

impl Stale {
    /// A key owing `outstanding` replies, kept for the grace from `now` —
    /// the one place a stale key's expiry is decided.
    fn owing(outstanding: NonZeroUsize, now: Instant) -> Self {
        Self {
            outstanding,
            expires: now + STALE_REPLY_GRACE,
        }
    }

    /// The state once one owed reply has been discarded, or `None` when that
    /// was the last.
    fn discard_one(self) -> Option<Self> {
        NonZeroUsize::new(self.outstanding.get() - 1).map(|outstanding| Self {
            outstanding,
            ..self
        })
    }
}

impl Registration {
    /// Whether this request takes the replies owed under `held` as its own.
    fn adopts(&self, held: &Correlation) -> bool {
        self.abandoned == AbandonedReply::AdoptIdentical && held.request == self.correlation.request
    }

    /// Puts the key in flight for this request, taking over the replies
    /// still owed under `adopted`, if any.
    fn register(self, adopted: Option<Stale>) -> HeldKey {
        HeldKey {
            correlation: self.correlation,
            state: KeyState::InFlight(InFlight {
                id: self.id,
                sender: self.sender,
                extra_replies: adopted.map_or(0, |stale| stale.outstanding.get()),
            }),
        }
    }
}

/// One registered request and the receiver waiting for its response.
///
/// Dropping this value unregisters the request, including when an outer async
/// deadline cancels [`HidppChannel::send_with_timeout`] during its write.
///
/// [`HidppChannel::send_with_timeout`]: super::HidppChannel::send_with_timeout
pub(super) struct PendingRequest {
    id: u64,
    pending_messages: Arc<Mutex<PendingQueue>>,
    receiver: oneshot::Receiver<HidppMessage>,
}

impl PendingRequest {
    /// Registers the request once nothing holds its `key`: no pending request
    /// shares it, and no abandoned request's reply with it is still expected.
    ///
    /// Until then the request is parked and woken each time the queue changes
    /// — and, while the key is merely stale, at the end of the grace at the
    /// latest. The check and the registration happen under one lock, so two
    /// parked requests woken together cannot both slip in.
    pub(super) async fn register_when_key_free(
        id: u64,
        pending_messages: Arc<Mutex<PendingQueue>>,
        request: HidppMessage,
        abandoned: AbandonedReply,
        response_predicate: impl Fn(&HidppMessage) -> bool + Send + 'static,
    ) -> Self {
        let (sender, receiver) = oneshot::channel();
        let key = request.header();
        let mut registration = Registration {
            id,
            correlation: Correlation {
                key,
                request,
                response_predicate: Box::new(response_predicate),
            },
            abandoned,
            sender,
        };
        loop {
            let now = Instant::now();
            let (parked, stale_until) = {
                let mut queue = lock(&pending_messages);
                let stale_until = match queue.try_register(registration, now) {
                    Ok(()) => break,
                    Err((returned, Wait::InFlight)) => {
                        registration = returned;
                        None
                    }
                    Err((returned, Wait::Stale(expires))) => {
                        registration = returned;
                        Some(expires)
                    }
                };
                let (wake, parked) = oneshot::channel();
                queue.key_waiters.push(wake);
                (parked, stale_until)
            };
            let (dev, feat, func) = key;
            match stale_until {
                None => {
                    trace!(
                        dev,
                        feat, func, "hidpp request parked — same header in flight"
                    );
                    // A dropped waker only means the queue changed; re-check
                    // either way.
                    let _ = parked.await;
                }
                Some(expires) => {
                    trace!(
                        dev,
                        feat,
                        func,
                        "hidpp request parked — replies with its header are still outstanding"
                    );
                    let mut parked = parked.fuse();
                    let mut grace =
                        futures_timer::Delay::new(expires.saturating_duration_since(now)).fuse();
                    select! {
                        _ = parked => {}
                        () = grace => {}
                    }
                }
            }
        }
        Self {
            id,
            pending_messages,
            receiver,
        }
    }

    pub(super) async fn receive(mut self) -> Option<HidppMessage> {
        (&mut self.receiver).await.ok()
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        lock(&self.pending_messages).abandon(self.id, Instant::now());
    }
}

pub(super) enum PendingRequestCompletion {
    Response(HidppMessage),
    WriteFailed(ChannelError),
    NoResponse,
    TimedOut,
}

pub(super) fn finish_request(
    completion: PendingRequestCompletion,
    mut observation: RequestObservation<'_>,
    dev: u8,
    feat: u8,
) -> Result<HidppMessage, ChannelError> {
    match completion {
        PendingRequestCompletion::Response(response) => {
            observation.complete(RequestOutcome::Succeeded);
            trace!(dev, feat, "hidpp response");
            Ok(response)
        }
        PendingRequestCompletion::WriteFailed(error) => {
            observation.complete(RequestOutcome::WriteFailed);
            trace!(dev, feat, error = ?error, "hidpp no response");
            Err(error)
        }
        PendingRequestCompletion::NoResponse => {
            observation.complete(RequestOutcome::NoResponse);
            trace!(dev, feat, error = ?ChannelError::NoResponse, "hidpp no response");
            Err(ChannelError::NoResponse)
        }
        PendingRequestCompletion::TimedOut => {
            observation.complete(RequestOutcome::TimedOut);
            trace!(dev, feat, error = ?ChannelError::Timeout, "hidpp no response");
            Err(ChannelError::Timeout)
        }
    }
}
