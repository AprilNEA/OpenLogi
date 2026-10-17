//! The channel's pending requests: what is awaiting a reply, and which
//! headers stay reserved for replies nobody waits for any more.
//!
//! [`PendingQueue`] is that state and its registration rule; a
//! [`PendingRequest`] is one caller's entry in it, parked until its header is
//! free and abandoned on drop. The parent module's read loop resolves incoming
//! reports against the queue, and its module docs say why two requests never
//! share a header.

use std::{
    collections::VecDeque,
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

/// The bytes every reply is matched on before any predicate runs: device
/// index, feature index (HID++1.0: sub id), and function/software id
/// (HID++1.0: register address) — the first three payload bytes of every
/// report, on both report kinds. Two requests that share them get replies
/// nothing on the wire can tell apart.
type CorrelationKey = (u8, u8, u8);

/// The requests awaiting a reply, plus the requests parked because one of
/// those shares their [`CorrelationKey`].
#[derive(Default)]
pub(super) struct PendingQueue {
    /// Sent messages waiting for a response, oldest first.
    pub(super) messages: VecDeque<PendingMessage>,

    /// Requests abandoned unanswered whose replies may still land. Each keeps
    /// its key taken until its replies arrive and are discarded, or its grace
    /// runs out — see [`STALE_REPLY_GRACE`].
    pub(super) stale: Vec<StaleKey>,

    /// Woken whenever `messages` or `stale` changes, so a parked request can
    /// re-check whether its key is free.
    key_waiters: Vec<oneshot::Sender<()>>,
}

/// A request that timed out or was cancelled with replies outstanding.
pub(super) struct StaleKey {
    /// The header bytes the outstanding replies will carry.
    pub(super) key: CorrelationKey,

    /// The request as sent, so a byte-identical re-ask that asks to adopt
    /// the outstanding replies can be recognised.
    pub(super) request: HidppMessage,

    /// Recognises an outstanding reply, so it can be discarded on arrival.
    pub(super) response_predicate: Box<dyn Fn(&HidppMessage) -> bool + Send>,

    /// How many replies are still owed: one per time the request went out
    /// unanswered.
    pub(super) outstanding: usize,

    /// When the key is given up on even without them.
    pub(super) expires: Instant,
}

/// What a request that could not register has to wait for.
enum Wait {
    /// A pending request with the same key; until it leaves the queue.
    InFlight,
    /// An abandoned request's replies with this key are still outstanding;
    /// until they are discarded, or the given instant at the latest.
    Stale(Instant),
}

impl PendingQueue {
    /// Registers `message` if nothing holds its key as of `now`, else hands it
    /// back with what it is waiting for. Prunes stale keys whose grace has
    /// passed.
    ///
    /// A stale key blocks whatever the new request's bytes are: byte equality
    /// with the abandoned request does not by itself make the outstanding
    /// replies its answer, since a write may have gone between the two asks.
    /// Only a request that asks to ([`AbandonedReply::AdoptIdentical`]) and
    /// is byte-identical registers at once and adopts them — it is answered
    /// by whichever reply comes first, and the rest stay owed (see
    /// [`PendingMessage::extra_replies`]).
    fn try_register(
        &mut self,
        mut message: PendingMessage,
        now: Instant,
    ) -> Result<(), (PendingMessage, Wait)> {
        self.stale.retain(|stale| stale.expires > now);
        if self
            .messages
            .iter()
            .any(|pending| pending.key == message.key)
        {
            return Err((message, Wait::InFlight));
        }
        let adopts = |stale: &StaleKey| {
            message.abandoned == AbandonedReply::AdoptIdentical && stale.request == message.request
        };
        if let Some(until) = self
            .stale
            .iter()
            .filter(|stale| stale.key == message.key && !adopts(stale))
            .map(|stale| stale.expires)
            .max()
        {
            return Err((message, Wait::Stale(until)));
        }
        message.extra_replies = self
            .stale
            .iter()
            .filter(|stale| stale.key == message.key)
            .map(|stale| stale.outstanding)
            .sum();
        self.stale.retain(|stale| stale.key != message.key);
        self.messages.push_back(message);
        Ok(())
    }

    /// Gives up on the request with `id`, if it is still awaiting its reply:
    /// it leaves the queue but its key stays taken for [`STALE_REPLY_GRACE`],
    /// so the reply — should it still come — is discarded rather than handed
    /// to the next request with that key. Wakes parked requests so they
    /// re-check what they are waiting for.
    fn abandon(&mut self, id: u64, now: Instant) {
        let Some(pos) = self.messages.iter().position(|message| message.id == id) else {
            return;
        };
        let Some(abandoned) = self.messages.remove(pos) else {
            return;
        };
        // Its own reply, plus any it had adopted from earlier abandoned sends
        // of the same request.
        let outstanding = abandoned.extra_replies + 1;
        self.owe_replies(abandoned, outstanding, now);
        self.wake_key_waiters();
    }

    /// Keeps `message`'s key taken for `outstanding` more replies, for the
    /// grace from `now`.
    fn owe_replies(&mut self, message: PendingMessage, outstanding: usize, now: Instant) {
        let PendingMessage {
            key,
            request,
            response_predicate,
            ..
        } = message;
        self.stale.push(StaleKey {
            key,
            request,
            response_predicate,
            outstanding,
            expires: now + STALE_REPLY_GRACE,
        });
    }

    /// Discards `msg` if it is an outstanding reply of an abandoned request,
    /// freeing that request's key once none is owed.
    pub(super) fn discard_stale_reply(&mut self, msg: &HidppMessage) -> bool {
        let Some(pos) = self
            .stale
            .iter()
            .position(|stale| (stale.response_predicate)(msg))
        else {
            return false;
        };
        self.stale[pos].outstanding -= 1;
        if self.stale[pos].outstanding == 0 {
            self.stale.remove(pos);
            self.wake_key_waiters();
        }
        true
    }

    pub(super) fn wake_key_waiters(&mut self) {
        for waiter in self.key_waiters.drain(..) {
            // A parked request that was cancelled meanwhile has dropped its
            // receiver; nothing to wake.
            let _ = waiter.send(());
        }
    }
}

/// Represents a message that was sent and is waiting for a response.
pub(super) struct PendingMessage {
    /// Unique ID used to remove this request when its waiter goes away.
    pub(super) id: u64,

    /// The header bytes this request's reply will carry.
    pub(super) key: CorrelationKey,

    /// The request as sent, so an abandoned one can be recognised when it is
    /// re-asked.
    pub(super) request: HidppMessage,

    /// What to do about replies still owed to an abandoned request with the
    /// same header.
    abandoned: AbandonedReply,

    /// The predicate that has to match for an incoming message to be classified
    /// as the response.
    pub(super) response_predicate: Box<dyn Fn(&HidppMessage) -> bool + Send>,

    /// The oneshot sender used to provide the response message to the receiving
    /// end.
    pub(super) sender: oneshot::Sender<HidppMessage>,

    /// Replies still owed to earlier, abandoned sends of this same request,
    /// adopted on registration under [`AbandonedReply::AdoptIdentical`].
    /// Whichever reply comes first answers this request; the rest are then
    /// owed under a stale key.
    pub(super) extra_replies: usize,
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
        let mut message = PendingMessage {
            id,
            key,
            request,
            abandoned,
            response_predicate: Box::new(response_predicate),
            sender,
            extra_replies: 0,
        };
        loop {
            let now = Instant::now();
            let (parked, stale_until) = {
                let mut queue = lock(&pending_messages);
                let stale_until = match queue.try_register(message, now) {
                    Ok(()) => break,
                    Err((returned, Wait::InFlight)) => {
                        message = returned;
                        None
                    }
                    Err((returned, Wait::Stale(expires))) => {
                        message = returned;
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
