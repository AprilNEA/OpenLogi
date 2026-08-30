use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Notify, mpsc};

use super::schema::{RequestMatch, normalize_hidpp20};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum RequestKey {
    Exact(Vec<u8>),
    Hidpp20(Vec<u8>),
}

impl RequestKey {
    pub(super) fn from_exchange(request_match: RequestMatch, request: &[u8]) -> Self {
        match request_match {
            RequestMatch::Exact => Self::Exact(request.to_vec()),
            RequestMatch::Hidpp20 => Self::Hidpp20(normalize_hidpp20(request)),
        }
    }
}

#[derive(Default)]
pub(super) struct ResponseGates {
    pending: Mutex<HashMap<RequestKey, VecDeque<Arc<ResponseBarrierState>>>>,
}

impl ResponseGates {
    pub(super) fn hold(
        &self,
        request_match: RequestMatch,
        request: &[u8],
    ) -> ReplayResponseBarrier {
        let state = Arc::new(ResponseBarrierState::default());
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(RequestKey::from_exchange(request_match, request))
            .or_default()
            .push_back(Arc::clone(&state));
        ReplayResponseBarrier { state }
    }

    pub(super) fn take(&self, actual: &[u8]) -> Option<Arc<ResponseBarrierState>> {
        let exact = RequestKey::Exact(actual.to_vec());
        let hidpp20 = RequestKey::Hidpp20(normalize_hidpp20(actual));
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending
            .get_mut(&exact)
            .and_then(VecDeque::pop_front)
            .or_else(|| pending.get_mut(&hidpp20).and_then(VecDeque::pop_front))
    }
}

#[derive(Default)]
struct ResponseBarrierRuntime {
    request_written: bool,
    released: bool,
    pending_response: Option<(mpsc::UnboundedSender<Vec<u8>>, Vec<u8>)>,
}

#[derive(Default)]
pub(super) struct ResponseBarrierState {
    runtime: Mutex<ResponseBarrierRuntime>,
    written: Notify,
}

impl ResponseBarrierState {
    pub(super) fn request_written(
        &self,
        incoming: mpsc::UnboundedSender<Vec<u8>>,
        response: Option<Vec<u8>>,
    ) {
        let response_to_send = {
            let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
            runtime.request_written = true;
            if runtime.released {
                response.map(|response| (incoming, response))
            } else {
                runtime.pending_response = response.map(|response| (incoming, response));
                None
            }
        };
        self.written.notify_waiters();
        if let Some((incoming, response)) = response_to_send {
            let _ = incoming.send(response);
        }
    }

    fn is_request_written(&self) -> bool {
        self.runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .request_written
    }

    fn release(&self) {
        let pending = {
            let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
            runtime.released = true;
            runtime.pending_response.take()
        };
        if let Some((incoming, response)) = pending {
            let _ = incoming.send(response);
        }
    }
}

/// One explicitly held replay response and its request-written barrier.
///
/// Holding affects only the next matching request. The raw write completes,
/// but its cassette response is not delivered to the production channel until
/// [`Self::release`] is called.
#[derive(Clone)]
pub struct ReplayResponseBarrier {
    state: Arc<ResponseBarrierState>,
}

impl ReplayResponseBarrier {
    /// Wait until the matching request has been written and consumed from its cassette.
    pub async fn request_written(&self) {
        loop {
            let notified = self.state.written.notified();
            if self.state.is_request_written() {
                return;
            }
            notified.await;
        }
    }

    /// Whether the matching request has already reached the raw channel.
    #[must_use]
    pub fn is_request_written(&self) -> bool {
        self.state.is_request_written()
    }

    /// Release the held response. Releasing before the request is written is supported.
    pub fn release(&self) {
        self.state.release();
    }
}
