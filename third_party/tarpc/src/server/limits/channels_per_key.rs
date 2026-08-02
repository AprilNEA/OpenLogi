// Copyright 2018 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use crate::{
    server::{self, Channel},
    util::Compact,
};
use fnv::FnvHashMap;
use futures::{prelude::*, ready, stream::Fuse, task::*};
use pin_project::pin_project;
use std::sync::{Arc, Weak};
use std::{
    collections::hash_map::Entry, convert::TryFrom, fmt, hash::Hash, marker::Unpin, pin::Pin,
};
use tokio::sync::mpsc;
use tracing::{debug, info, trace};

/// An [`Incoming`](crate::server::incoming::Incoming) stream that drops new channels based on
/// per-key limits.
///
/// The decision to drop a Channel is made once at the time the Channel materializes. Once a
/// Channel is yielded, it will not be prematurely dropped.
#[pin_project]
#[derive(Debug)]
pub struct MaxChannelsPerKey<S, K, F>
where
    K: Eq + Hash,
{
    #[pin]
    listener: Fuse<S>,
    channels_per_key: u32,
    dropped_keys: mpsc::UnboundedReceiver<K>,
    dropped_keys_tx: mpsc::UnboundedSender<K>,
    key_counts: FnvHashMap<K, Weak<Tracker<K>>>,
    keymaker: F,
}

/// A channel that is tracked by [`MaxChannelsPerKey`].
#[pin_project]
#[derive(Debug)]
pub struct TrackedChannel<C, K> {
    #[pin]
    inner: C,
    tracker: Arc<Tracker<K>>,
}

#[derive(Debug)]
struct Tracker<K> {
    key: Option<K>,
    dropped_keys: mpsc::UnboundedSender<K>,
}

impl<K> Drop for Tracker<K> {
    fn drop(&mut self) {
        // Don't care if the listener is dropped.
        let _ = self.dropped_keys.send(self.key.take().unwrap());
    }
}

impl<C, K> Stream for TrackedChannel<C, K>
where
    C: Stream,
{
    type Item = <C as Stream>::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.inner_pin_mut().poll_next(cx)
    }
}

impl<C, I, K> Sink<I> for TrackedChannel<C, K>
where
    C: Sink<I>,
{
    type Error = C::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.inner_pin_mut().poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: I) -> Result<(), Self::Error> {
        self.inner_pin_mut().start_send(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.inner_pin_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.inner_pin_mut().poll_close(cx)
    }
}

impl<C, K> AsRef<C> for TrackedChannel<C, K> {
    fn as_ref(&self) -> &C {
        &self.inner
    }
}

impl<C, K> Channel for TrackedChannel<C, K>
where
    C: Channel,
{
    type Req = C::Req;
    type Resp = C::Resp;
    type Transport = C::Transport;

    fn config(&self) -> &server::Config {
        self.inner.config()
    }

    fn in_flight_requests(&self) -> usize {
        self.inner.in_flight_requests()
    }

    fn transport(&self) -> &Self::Transport {
        self.inner.transport()
    }
}

impl<C, K> TrackedChannel<C, K> {
    /// Returns the inner channel.
    pub fn get_ref(&self) -> &C {
        &self.inner
    }

    /// Returns the pinned inner channel.
    fn inner_pin_mut<'a>(self: &'a mut Pin<&mut Self>) -> Pin<&'a mut C> {
        self.as_mut().project().inner
    }
}

impl<S, K, F> MaxChannelsPerKey<S, K, F>
where
    K: Eq + Hash,
    S: Stream,
    F: Fn(&S::Item) -> K,
{
    /// Sheds new channels to stay under configured limits.
    pub(crate) fn new(listener: S, channels_per_key: u32, keymaker: F) -> Self {
        let (dropped_keys_tx, dropped_keys) = mpsc::unbounded_channel();
        MaxChannelsPerKey {
            listener: listener.fuse(),
            channels_per_key,
            dropped_keys,
            dropped_keys_tx,
            key_counts: FnvHashMap::default(),
            keymaker,
        }
    }
}

impl<S, K, F> MaxChannelsPerKey<S, K, F>
where
    S: Stream,
    K: fmt::Display + Eq + Hash + Clone + Unpin,
    F: Fn(&S::Item) -> K,
{
    fn listener_pin_mut<'a>(self: &'a mut Pin<&mut Self>) -> Pin<&'a mut Fuse<S>> {
        self.as_mut().project().listener
    }

    fn handle_new_channel(
        mut self: Pin<&mut Self>,
        stream: S::Item,
    ) -> Result<TrackedChannel<S::Item, K>, K> {
        let key = (self.as_mut().keymaker)(&stream);
        let tracker = self.as_mut().increment_channels_for_key(key.clone())?;

        trace!(
            channel_filter_key = %key,
            open_channels = Arc::strong_count(&tracker),
            max_open_channels = self.channels_per_key,
            "Opening channel");

        Ok(TrackedChannel {
            tracker,
            inner: stream,
        })
    }

    fn increment_channels_for_key(self: Pin<&mut Self>, key: K) -> Result<Arc<Tracker<K>>, K> {
        let self_ = self.project();
        let dropped_keys = self_.dropped_keys_tx;
        match self_.key_counts.entry(key.clone()) {
            Entry::Vacant(vacant) => {
                let tracker = Arc::new(Tracker {
                    key: Some(key),
                    dropped_keys: dropped_keys.clone(),
                });

                vacant.insert(Arc::downgrade(&tracker));
                Ok(tracker)
            }
            Entry::Occupied(mut o) => {
                let count = o.get().strong_count();
                if count >= usize::try_from(*self_.channels_per_key).unwrap() {
                    info!(
                        channel_filter_key = %key,
                        open_channels = count,
                        max_open_channels = *self_.channels_per_key,
                        "At open channel limit");
                    Err(key)
                } else {
                    Ok(o.get().upgrade().unwrap_or_else(|| {
                        let tracker = Arc::new(Tracker {
                            key: Some(key),
                            dropped_keys: dropped_keys.clone(),
                        });

                        *o.get_mut() = Arc::downgrade(&tracker);
                        tracker
                    }))
                }
            }
        }
    }

    fn poll_listener(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TrackedChannel<S::Item, K>, K>>> {
        match ready!(self.listener_pin_mut().poll_next_unpin(cx)) {
            Some(codec) => Poll::Ready(Some(self.handle_new_channel(codec))),
            None => Poll::Ready(None),
        }
    }

    fn poll_closed_channels(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let self_ = self.project();
        match ready!(self_.dropped_keys.poll_recv(cx)) {
            Some(key) => {
                debug!(
                    channel_filter_key = %key,
                    "All channels dropped");
                self_.key_counts.remove(&key);
                self_.key_counts.compact(0.1);
                Poll::Ready(())
            }
            None => unreachable!("Holding a copy of closed_channels and didn't close it."),
        }
    }
}

impl<S, K, F> Stream for MaxChannelsPerKey<S, K, F>
where
    S: Stream,
    K: fmt::Display + Eq + Hash + Clone + Unpin,
    F: Fn(&S::Item) -> K,
{
    type Item = TrackedChannel<S::Item, K>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<TrackedChannel<S::Item, K>>> {
        loop {
            match (
                self.as_mut().poll_listener(cx),
                self.as_mut().poll_closed_channels(cx),
            ) {
                (Poll::Ready(Some(Ok(channel))), _) => {
                    return Poll::Ready(Some(channel));
                }
                (Poll::Ready(Some(Err(_))), _) => {
                    continue;
                }
                (_, Poll::Ready(())) => continue,
                (Poll::Pending, Poll::Pending) => return Poll::Pending,
                (Poll::Ready(None), Poll::Pending) => {
                    trace!("Shutting down listener.");
                    return Poll::Ready(None);
                }
            }
        }
    }
}
#[cfg(test)]
fn ctx() -> Context<'static> {
    use futures::task::*;

    Context::from_waker(noop_waker_ref())
}

#[test]
fn tracker_drop() {
    use assert_matches::assert_matches;

    let (tx, mut rx) = mpsc::unbounded_channel();
    Tracker {
        key: Some(1),
        dropped_keys: tx,
    };
    assert_matches!(rx.poll_recv(&mut ctx()), Poll::Ready(Some(1)));
}

#[test]
fn tracked_channel_stream() {
    use assert_matches::assert_matches;
    use pin_utils::pin_mut;

    let (chan_tx, chan) = futures::channel::mpsc::unbounded();
    let (dropped_keys, _) = mpsc::unbounded_channel();
    let channel = TrackedChannel {
        inner: chan,
        tracker: Arc::new(Tracker {
            key: Some(1),
            dropped_keys,
        }),
    };

    chan_tx.unbounded_send("test").unwrap();
    pin_mut!(channel);
    assert_matches!(channel.poll_next(&mut ctx()), Poll::Ready(Some("test")));
}

#[test]
fn tracked_channel_sink() {
    use assert_matches::assert_matches;
    use pin_utils::pin_mut;

    let (chan, mut chan_rx) = futures::channel::mpsc::unbounded();
    let (dropped_keys, _) = mpsc::unbounded_channel();
    let channel = TrackedChannel {
        inner: chan,
        tracker: Arc::new(Tracker {
            key: Some(1),
            dropped_keys,
        }),
    };

    pin_mut!(channel);
    assert_matches!(channel.as_mut().poll_ready(&mut ctx()), Poll::Ready(Ok(())));
    assert_matches!(channel.as_mut().start_send("test"), Ok(()));
    assert_matches!(channel.as_mut().poll_flush(&mut ctx()), Poll::Ready(Ok(())));
    assert_matches!(chan_rx.try_next(), Ok(Some("test")));
}

#[test]
fn channel_filter_increment_channels_for_key() {
    use assert_matches::assert_matches;
    use pin_utils::pin_mut;

    struct TestChannel {
        key: &'static str,
    }
    let (_, listener) = futures::channel::mpsc::unbounded();
    let filter = MaxChannelsPerKey::new(listener, 2, |chan: &TestChannel| chan.key);
    pin_mut!(filter);
    let tracker1 = filter.as_mut().increment_channels_for_key("key").unwrap();
    assert_eq!(Arc::strong_count(&tracker1), 1);
    let tracker2 = filter.as_mut().increment_channels_for_key("key").unwrap();
    assert_eq!(Arc::strong_count(&tracker1), 2);
    assert_matches!(filter.increment_channels_for_key("key"), Err("key"));
    drop(tracker2);
    assert_eq!(Arc::strong_count(&tracker1), 1);
}

#[test]
fn channel_filter_handle_new_channel() {
    use assert_matches::assert_matches;
    use pin_utils::pin_mut;

    #[derive(Debug)]
    struct TestChannel {
        key: &'static str,
    }
    let (_, listener) = futures::channel::mpsc::unbounded();
    let filter = MaxChannelsPerKey::new(listener, 2, |chan: &TestChannel| chan.key);
    pin_mut!(filter);
    let channel1 = filter
        .as_mut()
        .handle_new_channel(TestChannel { key: "key" })
        .unwrap();
    assert_eq!(Arc::strong_count(&channel1.tracker), 1);

    let channel2 = filter
        .as_mut()
        .handle_new_channel(TestChannel { key: "key" })
        .unwrap();
    assert_eq!(Arc::strong_count(&channel1.tracker), 2);

    assert_matches!(
        filter.handle_new_channel(TestChannel { key: "key" }),
        Err("key")
    );
    drop(channel2);
    assert_eq!(Arc::strong_count(&channel1.tracker), 1);
}

#[test]
fn channel_filter_poll_listener() {
    use assert_matches::assert_matches;
    use pin_utils::pin_mut;

    #[derive(Debug)]
    struct TestChannel {
        key: &'static str,
    }
    let (new_channels, listener) = futures::channel::mpsc::unbounded();
    let filter = MaxChannelsPerKey::new(listener, 2, |chan: &TestChannel| chan.key);
    pin_mut!(filter);

    new_channels
        .unbounded_send(TestChannel { key: "key" })
        .unwrap();
    let channel1 =
        assert_matches!(filter.as_mut().poll_listener(&mut ctx()), Poll::Ready(Some(Ok(c))) => c);
    assert_eq!(Arc::strong_count(&channel1.tracker), 1);

    new_channels
        .unbounded_send(TestChannel { key: "key" })
        .unwrap();
    let _channel2 =
        assert_matches!(filter.as_mut().poll_listener(&mut ctx()), Poll::Ready(Some(Ok(c))) => c);
    assert_eq!(Arc::strong_count(&channel1.tracker), 2);

    new_channels
        .unbounded_send(TestChannel { key: "key" })
        .unwrap();
    let key =
        assert_matches!(filter.as_mut().poll_listener(&mut ctx()), Poll::Ready(Some(Err(k))) => k);
    assert_eq!(key, "key");
    assert_eq!(Arc::strong_count(&channel1.tracker), 2);
}

#[test]
fn channel_filter_poll_closed_channels() {
    use assert_matches::assert_matches;
    use pin_utils::pin_mut;

    #[derive(Debug)]
    struct TestChannel {
        key: &'static str,
    }
    let (new_channels, listener) = futures::channel::mpsc::unbounded();
    let filter = MaxChannelsPerKey::new(listener, 2, |chan: &TestChannel| chan.key);
    pin_mut!(filter);

    new_channels
        .unbounded_send(TestChannel { key: "key" })
        .unwrap();
    let channel =
        assert_matches!(filter.as_mut().poll_listener(&mut ctx()), Poll::Ready(Some(Ok(c))) => c);
    assert_eq!(filter.key_counts.len(), 1);

    drop(channel);
    assert_matches!(
        filter.as_mut().poll_closed_channels(&mut ctx()),
        Poll::Ready(())
    );
    assert!(filter.key_counts.is_empty());
}

#[test]
fn channel_filter_stream() {
    use assert_matches::assert_matches;
    use pin_utils::pin_mut;

    #[derive(Debug)]
    struct TestChannel {
        key: &'static str,
    }
    let (new_channels, listener) = futures::channel::mpsc::unbounded();
    let filter = MaxChannelsPerKey::new(listener, 2, |chan: &TestChannel| chan.key);
    pin_mut!(filter);

    new_channels
        .unbounded_send(TestChannel { key: "key" })
        .unwrap();
    let channel = assert_matches!(filter.as_mut().poll_next(&mut ctx()), Poll::Ready(Some(c)) => c);
    assert_eq!(filter.key_counts.len(), 1);

    drop(channel);
    assert_matches!(filter.as_mut().poll_next(&mut ctx()), Poll::Pending);
    assert!(filter.key_counts.is_empty());
}
