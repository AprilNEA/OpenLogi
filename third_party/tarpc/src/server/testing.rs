// Copyright 2020 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use crate::{
    cancellations::{cancellations, CanceledRequests, RequestCancellation},
    context,
    server::{Channel, Config, ResponseGuard, TrackedRequest},
    Request, Response,
};
use futures::{task::*, Sink, Stream};
use pin_project::pin_project;
use std::{collections::VecDeque, io, pin::Pin, time::Instant};
use tracing::Span;

#[pin_project]
pub(crate) struct FakeChannel<In, Out> {
    #[pin]
    pub stream: VecDeque<In>,
    #[pin]
    pub sink: VecDeque<Out>,
    pub config: Config,
    pub in_flight_requests: super::in_flight_requests::InFlightRequests,
    pub request_cancellation: RequestCancellation,
    pub canceled_requests: CanceledRequests,
}

impl<In, Out> Stream for FakeChannel<In, Out>
where
    In: Unpin,
{
    type Item = In;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.project().stream.pop_front())
    }
}

impl<In, Resp> Sink<Response<Resp>> for FakeChannel<In, Response<Resp>> {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.project().sink.poll_ready(cx).map_err(|e| match e {})
    }

    fn start_send(mut self: Pin<&mut Self>, response: Response<Resp>) -> Result<(), Self::Error> {
        self.as_mut()
            .project()
            .in_flight_requests
            .remove_request(response.request_id);
        self.project()
            .sink
            .start_send(response)
            .map_err(|e| match e {})
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.project().sink.poll_flush(cx).map_err(|e| match e {})
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.project().sink.poll_close(cx).map_err(|e| match e {})
    }
}

impl<Req, Resp> Channel for FakeChannel<io::Result<TrackedRequest<Req>>, Response<Resp>>
where
    Req: Unpin,
{
    type Req = Req;
    type Resp = Resp;
    type Transport = ();

    fn config(&self) -> &Config {
        &self.config
    }

    fn in_flight_requests(&self) -> usize {
        self.in_flight_requests.len()
    }

    fn transport(&self) -> &() {
        &()
    }
}

impl<Req, Resp> FakeChannel<io::Result<TrackedRequest<Req>>, Response<Resp>> {
    pub fn push_req(&mut self, id: u64, message: Req) {
        let (_, abort_registration) = futures::future::AbortHandle::new_pair();
        let (request_cancellation, _) = cancellations();
        self.stream.push_back(Ok(TrackedRequest {
            request: Request {
                context: context::Context {
                    deadline: Instant::now(),
                    trace_context: Default::default(),
                },
                id,
                message,
            },
            abort_registration,
            span: Span::none(),
            response_guard: ResponseGuard {
                request_cancellation,
                request_id: id,
                cancel: false,
            },
        }));
    }
}

impl FakeChannel<(), ()> {
    pub fn default<Req, Resp>() -> FakeChannel<io::Result<TrackedRequest<Req>>, Response<Resp>> {
        let (request_cancellation, canceled_requests) = cancellations();
        FakeChannel {
            stream: Default::default(),
            sink: Default::default(),
            config: Default::default(),
            in_flight_requests: Default::default(),
            request_cancellation,
            canceled_requests,
        }
    }
}

pub trait PollExt {
    fn is_done(&self) -> bool;
}

impl<T> PollExt for Poll<Option<T>> {
    fn is_done(&self) -> bool {
        matches!(self, Poll::Ready(None))
    }
}

pub fn cx() -> Context<'static> {
    Context::from_waker(noop_waker_ref())
}
