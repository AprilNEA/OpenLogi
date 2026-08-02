// Copyright 2018 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

//! Provides a [`Transport`](sealed::Transport) trait as well as implementations.
//!
//! The rpc crate is transport- and protocol-agnostic. Any transport that impls [`Transport`](sealed::Transport)
//! can be plugged in, using whatever protocol it wants.

pub mod channel;

pub(crate) mod sealed {
    use futures::prelude::*;
    use std::error::Error;

    /// A bidirectional stream ([`Sink`] + [`Stream`]) of messages.
    pub trait Transport<SinkItem, Item>
    where
        Self: Stream<Item = Result<Item, <Self as Sink<SinkItem>>::Error>>,
        Self: Sink<SinkItem, Error = <Self as Transport<SinkItem, Item>>::TransportError>,
        <Self as Sink<SinkItem>>::Error: Error,
    {
        /// Associated type where clauses are not elaborated; this associated type allows users
        /// bounding types by Transport to avoid having to explicitly add `T::Error: Error` to their
        /// bounds.
        type TransportError: Error + Send + Sync + 'static;
    }

    impl<T, SinkItem, Item, E> Transport<SinkItem, Item> for T
    where
        T: ?Sized,
        T: Stream<Item = Result<Item, E>>,
        T: Sink<SinkItem, Error = E>,
        T::Error: Error + Send + Sync + 'static,
    {
        type TransportError = E;
    }
}
