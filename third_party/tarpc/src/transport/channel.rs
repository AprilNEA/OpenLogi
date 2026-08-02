// Copyright 2018 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

//! Transports backed by in-memory channels.

use futures::{task::*, Sink, Stream};
use pin_project::pin_project;
use std::{error::Error, pin::Pin};
use tokio::sync::mpsc;

/// Errors that occur in the sending or receiving of messages over a channel.
#[derive(thiserror::Error, Debug)]
pub enum ChannelError {
    /// An error occurred readying to send into the channel.
    #[error("an error occurred readying to send into the channel")]
    Ready(#[source] Box<dyn Error + Send + Sync + 'static>),
    /// An error occurred sending into the channel.
    #[error("an error occurred sending into the channel")]
    Send(#[source] Box<dyn Error + Send + Sync + 'static>),
    /// An error occurred receiving from the channel.
    #[error("an error occurred receiving from the channel")]
    Receive(#[source] Box<dyn Error + Send + Sync + 'static>),
}

/// Returns two unbounded channel peers. Each [`Stream`] yields items sent through the other's
/// [`Sink`].
pub fn unbounded<SinkItem, Item>() -> (
    UnboundedChannel<SinkItem, Item>,
    UnboundedChannel<Item, SinkItem>,
) {
    let (tx1, rx2) = mpsc::unbounded_channel();
    let (tx2, rx1) = mpsc::unbounded_channel();
    (
        UnboundedChannel { tx: tx1, rx: rx1 },
        UnboundedChannel { tx: tx2, rx: rx2 },
    )
}

/// A bi-directional channel backed by an [`UnboundedSender`](mpsc::UnboundedSender)
/// and [`UnboundedReceiver`](mpsc::UnboundedReceiver).
#[derive(Debug)]
pub struct UnboundedChannel<Item, SinkItem> {
    rx: mpsc::UnboundedReceiver<Item>,
    tx: mpsc::UnboundedSender<SinkItem>,
}

impl<Item, SinkItem> Stream for UnboundedChannel<Item, SinkItem> {
    type Item = Result<Item, ChannelError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Item, ChannelError>>> {
        self.rx
            .poll_recv(cx)
            .map(|option| option.map(Ok))
            .map_err(ChannelError::Receive)
    }
}

const CLOSED_MESSAGE: &str = "the channel is closed and cannot accept new items for sending";

impl<Item, SinkItem> Sink<SinkItem> for UnboundedChannel<Item, SinkItem> {
    type Error = ChannelError;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(if self.tx.is_closed() {
            Err(ChannelError::Ready(CLOSED_MESSAGE.into()))
        } else {
            Ok(())
        })
    }

    fn start_send(self: Pin<&mut Self>, item: SinkItem) -> Result<(), Self::Error> {
        self.tx
            .send(item)
            .map_err(|_| ChannelError::Send(CLOSED_MESSAGE.into()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // UnboundedSender requires no flushing.
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // UnboundedSender can't initiate closure.
        Poll::Ready(Ok(()))
    }
}

/// Returns two channel peers with buffer equal to `capacity`. Each [`Stream`] yields items sent
/// through the other's [`Sink`].
pub fn bounded<SinkItem, Item>(
    capacity: usize,
) -> (Channel<SinkItem, Item>, Channel<Item, SinkItem>) {
    let (tx1, rx2) = futures::channel::mpsc::channel(capacity);
    let (tx2, rx1) = futures::channel::mpsc::channel(capacity);
    (Channel { tx: tx1, rx: rx1 }, Channel { tx: tx2, rx: rx2 })
}

/// A bi-directional channel backed by a [`Sender`](futures::channel::mpsc::Sender)
/// and [`Receiver`](futures::channel::mpsc::Receiver).
#[pin_project]
#[derive(Debug)]
pub struct Channel<Item, SinkItem> {
    #[pin]
    rx: futures::channel::mpsc::Receiver<Item>,
    #[pin]
    tx: futures::channel::mpsc::Sender<SinkItem>,
}

impl<Item, SinkItem> Stream for Channel<Item, SinkItem> {
    type Item = Result<Item, ChannelError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Item, ChannelError>>> {
        self.project()
            .rx
            .poll_next(cx)
            .map(|option| option.map(Ok))
            .map_err(ChannelError::Receive)
    }
}

impl<Item, SinkItem> Sink<SinkItem> for Channel<Item, SinkItem> {
    type Error = ChannelError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project()
            .tx
            .poll_ready(cx)
            .map_err(|e| ChannelError::Ready(Box::new(e)))
    }

    fn start_send(self: Pin<&mut Self>, item: SinkItem) -> Result<(), Self::Error> {
        self.project()
            .tx
            .start_send(item)
            .map_err(|e| ChannelError::Send(Box::new(e)))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project()
            .tx
            .poll_flush(cx)
            .map_err(|e| ChannelError::Send(Box::new(e)))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project()
            .tx
            .poll_close(cx)
            .map_err(|e| ChannelError::Send(Box::new(e)))
    }
}

#[cfg(all(test, feature = "tokio1"))]
mod tests {
    use crate::{
        client::{self, RpcError},
        context,
        server::{incoming::Incoming, serve, BaseChannel},
        transport::{
            self,
            channel::{Channel, UnboundedChannel},
        },
        ServerError,
    };
    use assert_matches::assert_matches;
    use futures::{prelude::*, stream};
    use std::io;
    use tracing::trace;

    #[test]
    fn ensure_is_transport() {
        fn is_transport<SinkItem, Item, T: crate::Transport<SinkItem, Item>>() {}
        is_transport::<(), (), UnboundedChannel<(), ()>>();
        is_transport::<(), (), Channel<(), ()>>();
    }

    #[tokio::test]
    async fn integration() -> anyhow::Result<()> {
        let _ = tracing_subscriber::fmt::try_init();

        let (client_channel, server_channel) = transport::channel::unbounded();
        tokio::spawn(
            stream::once(future::ready(server_channel))
                .map(BaseChannel::with_defaults)
                .execute(serve(|_ctx, request: String| async move {
                    request.parse::<u64>().map_err(|_| {
                        ServerError::new(
                            io::ErrorKind::InvalidInput,
                            format!("{request:?} is not an int"),
                        )
                    })
                }))
                .for_each(|channel| async move {
                    tokio::spawn(channel.for_each(|response| response));
                }),
        );

        let client = client::new(client::Config::default(), client_channel).spawn();

        let response1 = client.call(context::current(), "123".into()).await;
        let response2 = client.call(context::current(), "abc".into()).await;

        trace!("response1: {:?}, response2: {:?}", response1, response2);

        assert_matches!(response1, Ok(123));
        assert_matches!(response2, Err(RpcError::Server(e)) if e.kind == io::ErrorKind::InvalidInput);

        Ok(())
    }
}
