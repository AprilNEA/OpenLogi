use super::{
    limits::{channels_per_key::MaxChannelsPerKey, requests_per_channel::MaxRequestsPerChannel},
    Channel, RequestName, Serve,
};
use futures::prelude::*;
use std::{fmt, hash::Hash};

/// An extension trait for [streams](futures::prelude::Stream) of [`Channels`](Channel).
pub trait Incoming<C>
where
    Self: Sized + Stream<Item = C>,
    C: Channel,
{
    /// Enforces channel per-key limits.
    fn max_channels_per_key<K, KF>(self, n: u32, keymaker: KF) -> MaxChannelsPerKey<Self, K, KF>
    where
        K: fmt::Display + Eq + Hash + Clone + Unpin,
        KF: Fn(&C) -> K,
    {
        MaxChannelsPerKey::new(self, n, keymaker)
    }

    /// Caps the number of concurrent requests per channel.
    fn max_concurrent_requests_per_channel(self, n: usize) -> MaxRequestsPerChannel<Self> {
        MaxRequestsPerChannel::new(self, n)
    }

    /// Returns a stream of channels in execution. Each channel in execution is a stream of
    /// futures, where each future is an in-flight request being rsponded to.
    fn execute<S>(
        self,
        serve: S,
    ) -> impl Stream<Item = impl Stream<Item = impl Future<Output = ()>>>
    where
        C::Req: RequestName,
        S: Serve<Req = C::Req, Resp = C::Resp> + Clone,
    {
        self.map(move |channel| channel.execute(serve.clone()))
    }
}

#[cfg(feature = "tokio1")]
/// Spawns all channels-in-execution, delegating to the tokio runtime to manage their completion.
/// Each channel is spawned, and each request from each channel is spawned.
/// Note that this function is generic over any stream-of-streams-of-futures, but it is intended
/// for spawning streams of channels.
///
/// # Example
/// ```rust
/// use tarpc::{
///     context,
///     client::{self, NewClient},
///     server::{self, BaseChannel, Channel, incoming::{Incoming, spawn_incoming}, serve},
///     transport,
/// };
/// use futures::prelude::*;
///
/// #[tokio::main]
/// async fn main() {
///     let (tx, rx) = transport::channel::unbounded();
///     let NewClient { client, dispatch } = client::new(client::Config::default(), tx);
///     tokio::spawn(dispatch);
///
///     let incoming = stream::once(async move {
///         BaseChannel::new(server::Config::default(), rx)
///     }).execute(serve(|_, i| async move { Ok(i + 1) }));
///     tokio::spawn(spawn_incoming(incoming));
///     assert_eq!(client.call(context::current(), 1).await.unwrap(), 2);
/// }
/// ```
pub async fn spawn_incoming(
    incoming: impl Stream<
        Item = impl Stream<Item = impl Future<Output = ()> + Send + 'static> + Send + 'static,
    >,
) {
    use futures::pin_mut;
    pin_mut!(incoming);
    while let Some(channel) = incoming.next().await {
        tokio::spawn(async move {
            pin_mut!(channel);
            while let Some(request) = channel.next().await {
                tokio::spawn(request);
            }
        });
    }
}

impl<S, C> Incoming<C> for S
where
    S: Sized + Stream<Item = C>,
    C: Channel,
{
}
