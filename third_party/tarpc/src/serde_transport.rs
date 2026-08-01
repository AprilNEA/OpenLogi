// Copyright 2019 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

//! A generic Serde-based `Transport` that can serialize anything supported by `tokio-serde` via any medium that implements `AsyncRead` and `AsyncWrite`.

#![deny(missing_docs)]

use futures::{prelude::*, task::*};
use pin_project::pin_project;
use serde::{Deserialize, Serialize};
use std::{error::Error, io, pin::Pin};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_serde::{Framed as SerdeFramed, *};
use tokio_util::codec::{length_delimited::LengthDelimitedCodec, Framed};

/// A transport that serializes to, and deserializes from, a byte stream.
#[pin_project]
pub struct Transport<S, Item, SinkItem, Codec> {
    #[pin]
    inner: SerdeFramed<Framed<S, LengthDelimitedCodec>, Item, SinkItem, Codec>,
}

impl<S, Item, SinkItem, Codec> Transport<S, Item, SinkItem, Codec> {
    /// Returns the inner transport over which messages are sent and received.
    pub fn get_ref(&self) -> &S {
        self.inner.get_ref().get_ref()
    }
}

impl<S, Item, SinkItem, Codec, CodecError> Stream for Transport<S, Item, SinkItem, Codec>
where
    S: AsyncWrite + AsyncRead,
    Item: for<'a> Deserialize<'a>,
    Codec: Deserializer<Item>,
    CodecError: Into<Box<dyn std::error::Error + Send + Sync>>,
    SerdeFramed<Framed<S, LengthDelimitedCodec>, Item, SinkItem, Codec>:
        Stream<Item = Result<Item, CodecError>>,
{
    type Item = io::Result<Item>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<io::Result<Item>>> {
        self.project()
            .inner
            .poll_next(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

impl<S, Item, SinkItem, Codec, CodecError> Sink<SinkItem> for Transport<S, Item, SinkItem, Codec>
where
    S: AsyncWrite,
    SinkItem: Serialize,
    Codec: Serializer<SinkItem>,
    CodecError: Into<Box<dyn Error + Send + Sync>>,
    SerdeFramed<Framed<S, LengthDelimitedCodec>, Item, SinkItem, Codec>:
        Sink<SinkItem, Error = CodecError>,
{
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project()
            .inner
            .poll_ready(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn start_send(self: Pin<&mut Self>, item: SinkItem) -> io::Result<()> {
        self.project()
            .inner
            .start_send(item)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project()
            .inner
            .poll_flush(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project()
            .inner
            .poll_close(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

/// Constructs a new transport from a framed transport and a serialization codec.
pub fn new<S, Item, SinkItem, Codec>(
    framed_io: Framed<S, LengthDelimitedCodec>,
    codec: Codec,
) -> Transport<S, Item, SinkItem, Codec>
where
    S: AsyncWrite + AsyncRead,
    Item: for<'de> Deserialize<'de>,
    SinkItem: Serialize,
    Codec: Serializer<SinkItem> + Deserializer<Item>,
{
    Transport {
        inner: SerdeFramed::new(framed_io, codec),
    }
}

impl<S, Item, SinkItem, Codec> From<(S, Codec)> for Transport<S, Item, SinkItem, Codec>
where
    S: AsyncWrite + AsyncRead,
    Item: for<'de> Deserialize<'de>,
    SinkItem: Serialize,
    Codec: Serializer<SinkItem> + Deserializer<Item>,
{
    fn from((io, codec): (S, Codec)) -> Self {
        new(Framed::new(io, LengthDelimitedCodec::new()), codec)
    }
}

#[cfg(feature = "tcp")]
#[cfg_attr(docsrs, doc(cfg(feature = "tcp")))]
/// TCP support for generic transport using Tokio.
pub mod tcp {
    use {
        super::*,
        futures::ready,
        std::{marker::PhantomData, net::SocketAddr},
        tokio::net::{TcpListener, TcpStream, ToSocketAddrs},
        tokio_util::codec::length_delimited,
    };

    impl<Item, SinkItem, Codec> Transport<TcpStream, Item, SinkItem, Codec> {
        /// Returns the peer address of the underlying TcpStream.
        pub fn peer_addr(&self) -> io::Result<SocketAddr> {
            self.inner.get_ref().get_ref().peer_addr()
        }
        /// Returns the local address of the underlying TcpStream.
        pub fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.get_ref().get_ref().local_addr()
        }
    }

    /// A connection Future that also exposes the length-delimited framing config.
    #[must_use]
    #[pin_project]
    pub struct TcpConnect<T, Item, SinkItem, CodecFn> {
        #[pin]
        inner: T,
        codec_fn: CodecFn,
        config: length_delimited::Builder,
        ghost: PhantomData<(fn(SinkItem), fn() -> Item)>,
    }

    impl<T, Item, SinkItem, Codec, CodecFn> Future for TcpConnect<T, Item, SinkItem, CodecFn>
    where
        T: Future<Output = io::Result<TcpStream>>,
        Item: for<'de> Deserialize<'de>,
        SinkItem: Serialize,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        type Output = io::Result<Transport<TcpStream, Item, SinkItem, Codec>>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
            let io = ready!(self.as_mut().project().inner.poll(cx))?;
            Poll::Ready(Ok(new(self.config.new_framed(io), (self.codec_fn)())))
        }
    }

    impl<T, Item, SinkItem, CodecFn> TcpConnect<T, Item, SinkItem, CodecFn> {
        /// Returns an immutable reference to the length-delimited codec's config.
        pub fn config(&self) -> &length_delimited::Builder {
            &self.config
        }

        /// Returns a mutable reference to the length-delimited codec's config.
        pub fn config_mut(&mut self) -> &mut length_delimited::Builder {
            &mut self.config
        }
    }

    /// Connects to `addr`, wrapping the connection in a TCP transport.
    pub fn connect<A, Item, SinkItem, Codec, CodecFn>(
        addr: A,
        codec_fn: CodecFn,
    ) -> TcpConnect<impl Future<Output = io::Result<TcpStream>>, Item, SinkItem, CodecFn>
    where
        A: ToSocketAddrs,
        Item: for<'de> Deserialize<'de>,
        SinkItem: Serialize,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        TcpConnect {
            inner: TcpStream::connect(addr),
            codec_fn,
            config: LengthDelimitedCodec::builder(),
            ghost: PhantomData,
        }
    }

    /// Listens on `addr`, wrapping accepted connections in TCP transports.
    pub async fn listen<A, Item, SinkItem, Codec, CodecFn>(
        addr: A,
        codec_fn: CodecFn,
    ) -> io::Result<Incoming<Item, SinkItem, Codec, CodecFn>>
    where
        A: ToSocketAddrs,
        Item: for<'de> Deserialize<'de>,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        listen_on(TcpListener::bind(addr).await?, codec_fn).await
    }

    /// Wrap accepted connections from `listener` in TCP transports.
    pub async fn listen_on<Item, SinkItem, Codec, CodecFn>(
        listener: TcpListener,
        codec_fn: CodecFn,
    ) -> io::Result<Incoming<Item, SinkItem, Codec, CodecFn>>
    where
        Item: for<'de> Deserialize<'de>,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        let local_addr = listener.local_addr()?;
        Ok(Incoming {
            listener,
            codec_fn,
            local_addr,
            config: LengthDelimitedCodec::builder(),
            ghost: PhantomData,
        })
    }

    /// A [`TcpListener`] that wraps connections in [transports](Transport).
    #[pin_project]
    #[derive(Debug)]
    pub struct Incoming<Item, SinkItem, Codec, CodecFn> {
        listener: TcpListener,
        local_addr: SocketAddr,
        codec_fn: CodecFn,
        config: length_delimited::Builder,
        ghost: PhantomData<(fn() -> Item, fn(SinkItem), Codec)>,
    }

    impl<Item, SinkItem, Codec, CodecFn> Incoming<Item, SinkItem, Codec, CodecFn> {
        /// Returns the address being listened on.
        pub fn local_addr(&self) -> SocketAddr {
            self.local_addr
        }

        /// Returns an immutable reference to the length-delimited codec's config.
        pub fn config(&self) -> &length_delimited::Builder {
            &self.config
        }

        /// Returns a mutable reference to the length-delimited codec's config.
        pub fn config_mut(&mut self) -> &mut length_delimited::Builder {
            &mut self.config
        }
    }

    impl<Item, SinkItem, Codec, CodecFn> Stream for Incoming<Item, SinkItem, Codec, CodecFn>
    where
        Item: for<'de> Deserialize<'de>,
        SinkItem: Serialize,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        type Item = io::Result<Transport<TcpStream, Item, SinkItem, Codec>>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let conn: TcpStream =
                ready!(Pin::new(&mut self.as_mut().project().listener).poll_accept(cx)?).0;
            Poll::Ready(Some(Ok(new(
                self.config.new_framed(conn),
                (self.codec_fn)(),
            ))))
        }
    }
}

#[cfg(all(unix, feature = "unix"))]
#[cfg_attr(docsrs, doc(cfg(all(unix, feature = "unix"))))]
/// Unix Domain Socket support for generic transport using Tokio.
pub mod unix {
    use {
        super::*,
        futures::ready,
        std::{marker::PhantomData, path::Path},
        tokio::net::{unix::SocketAddr, UnixListener, UnixStream},
        tokio_util::codec::length_delimited,
    };

    impl<Item, SinkItem, Codec> Transport<UnixStream, Item, SinkItem, Codec> {
        /// Returns the socket address of the remote half of the underlying [`UnixStream`].
        pub fn peer_addr(&self) -> io::Result<SocketAddr> {
            self.inner.get_ref().get_ref().peer_addr()
        }
        /// Returns the socket address of the local half of the underlying [`UnixStream`].
        pub fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.get_ref().get_ref().local_addr()
        }
    }

    /// A connection Future that also exposes the length-delimited framing config.
    #[must_use]
    #[pin_project]
    pub struct UnixConnect<T, Item, SinkItem, CodecFn> {
        #[pin]
        inner: T,
        codec_fn: CodecFn,
        config: length_delimited::Builder,
        ghost: PhantomData<(fn(SinkItem), fn() -> Item)>,
    }

    impl<T, Item, SinkItem, Codec, CodecFn> Future for UnixConnect<T, Item, SinkItem, CodecFn>
    where
        T: Future<Output = io::Result<UnixStream>>,
        Item: for<'de> Deserialize<'de>,
        SinkItem: Serialize,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        type Output = io::Result<Transport<UnixStream, Item, SinkItem, Codec>>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
            let io = ready!(self.as_mut().project().inner.poll(cx))?;
            Poll::Ready(Ok(new(self.config.new_framed(io), (self.codec_fn)())))
        }
    }

    impl<T, Item, SinkItem, CodecFn> UnixConnect<T, Item, SinkItem, CodecFn> {
        /// Returns an immutable reference to the length-delimited codec's config.
        pub fn config(&self) -> &length_delimited::Builder {
            &self.config
        }

        /// Returns a mutable reference to the length-delimited codec's config.
        pub fn config_mut(&mut self) -> &mut length_delimited::Builder {
            &mut self.config
        }
    }

    /// Connects to socket named by `path`, wrapping the connection in a Unix Domain Socket
    /// transport.
    pub fn connect<P, Item, SinkItem, Codec, CodecFn>(
        path: P,
        codec_fn: CodecFn,
    ) -> UnixConnect<impl Future<Output = io::Result<UnixStream>>, Item, SinkItem, CodecFn>
    where
        P: AsRef<Path>,
        Item: for<'de> Deserialize<'de>,
        SinkItem: Serialize,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        UnixConnect {
            inner: UnixStream::connect(path),
            codec_fn,
            config: LengthDelimitedCodec::builder(),
            ghost: PhantomData,
        }
    }

    /// Listens on the socket named by `path`, wrapping accepted connections in Unix Domain Socket
    /// transports.
    pub async fn listen<P, Item, SinkItem, Codec, CodecFn>(
        path: P,
        codec_fn: CodecFn,
    ) -> io::Result<Incoming<Item, SinkItem, Codec, CodecFn>>
    where
        P: AsRef<Path>,
        Item: for<'de> Deserialize<'de>,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        listen_on(UnixListener::bind(path)?, codec_fn).await
    }

    /// Wrap accepted connections from `listener` in Unix Domain Socket transports.
    pub async fn listen_on<Item, SinkItem, Codec, CodecFn>(
        listener: UnixListener,
        codec_fn: CodecFn,
    ) -> io::Result<Incoming<Item, SinkItem, Codec, CodecFn>>
    where
        Item: for<'de> Deserialize<'de>,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        let local_addr = listener.local_addr()?;
        Ok(Incoming {
            listener,
            codec_fn,
            local_addr,
            config: LengthDelimitedCodec::builder(),
            ghost: PhantomData,
        })
    }

    /// A [`UnixListener`] that wraps connections in [transports](Transport).
    #[pin_project]
    #[derive(Debug)]
    pub struct Incoming<Item, SinkItem, Codec, CodecFn> {
        listener: UnixListener,
        local_addr: SocketAddr,
        codec_fn: CodecFn,
        config: length_delimited::Builder,
        ghost: PhantomData<(fn() -> Item, fn(SinkItem), Codec)>,
    }

    impl<Item, SinkItem, Codec, CodecFn> Incoming<Item, SinkItem, Codec, CodecFn> {
        /// Returns the the socket address being listened on.
        pub fn local_addr(&self) -> &SocketAddr {
            &self.local_addr
        }

        /// Returns an immutable reference to the length-delimited codec's config.
        pub fn config(&self) -> &length_delimited::Builder {
            &self.config
        }

        /// Returns a mutable reference to the length-delimited codec's config.
        pub fn config_mut(&mut self) -> &mut length_delimited::Builder {
            &mut self.config
        }
    }

    impl<Item, SinkItem, Codec, CodecFn> Stream for Incoming<Item, SinkItem, Codec, CodecFn>
    where
        Item: for<'de> Deserialize<'de>,
        SinkItem: Serialize,
        Codec: Serializer<SinkItem> + Deserializer<Item>,
        CodecFn: Fn() -> Codec,
    {
        type Item = io::Result<Transport<UnixStream, Item, SinkItem, Codec>>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let conn: UnixStream = ready!(self.as_mut().project().listener.poll_accept(cx)?).0;
            Poll::Ready(Some(Ok(new(
                self.config.new_framed(conn),
                (self.codec_fn)(),
            ))))
        }
    }

    /// A temporary `PathBuf` that lives in `std::env::temp_dir` and is removed on drop.
    pub struct TempPathBuf(std::path::PathBuf);

    impl TempPathBuf {
        /// A named socket that results in `<tempdir>/<name>`
        pub fn new<S: AsRef<str>>(name: S) -> Self {
            let mut sock = std::env::temp_dir();
            sock.push(name.as_ref());
            Self(sock)
        }

        /// Appends a random hex string to the socket name resulting in
        /// `<tempdir>/<name>_<xxxxx>`
        pub fn with_random<S: AsRef<str>>(name: S) -> Self {
            Self::new(format!("{}_{:016x}", name.as_ref(), rand::random::<u64>()))
        }
    }

    impl AsRef<std::path::Path> for TempPathBuf {
        fn as_ref(&self) -> &std::path::Path {
            self.0.as_path()
        }
    }

    impl Drop for TempPathBuf {
        fn drop(&mut self) {
            // This will remove the file pointed to by this PathBuf if it exists, however Err's can
            // be returned such as attempting to remove a non-existing file, or one which we don't
            // have permission to remove. In these cases the Err is swallowed
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tokio_serde::formats::SymmetricalJson;

        #[test]
        fn temp_path_buf_non_random() {
            let sock = TempPathBuf::new("test");
            let mut good = std::env::temp_dir();
            good.push("test");
            assert_eq!(sock.as_ref(), good);
            assert_eq!(sock.as_ref().file_name().unwrap(), "test");
        }

        #[test]
        fn temp_path_buf_random() {
            let sock = TempPathBuf::with_random("test");
            let good = std::env::temp_dir();
            assert!(sock.as_ref().starts_with(good));
            // Since there are 16 random characters we just assert the file_name has the right name
            // and starts with the correct string 'test_'
            // file name: test_xxxxxxxxxxxxxxxx
            // test  = 4
            // _     = 1
            // <hex> = 16
            // total = 21
            let fname = sock.as_ref().file_name().unwrap().to_string_lossy();
            assert!(fname.starts_with("test_"));
            assert_eq!(fname.len(), 21);
        }

        #[test]
        fn temp_path_buf_non_existing() {
            let sock = TempPathBuf::with_random("test");
            let sock_path = std::path::PathBuf::from(sock.as_ref());

            // No actual file has been created yet
            assert!(!sock_path.exists());
            // Should not panic
            std::mem::drop(sock);
            assert!(!sock_path.exists());
        }

        #[test]
        fn temp_path_buf_existing_file() {
            let sock = TempPathBuf::with_random("test");
            let sock_path = std::path::PathBuf::from(sock.as_ref());
            let _file = std::fs::File::create(&sock).unwrap();
            assert!(sock_path.exists());
            std::mem::drop(sock);
            assert!(!sock_path.exists());
        }

        #[test]
        fn temp_path_buf_preexisting_file() {
            let mut pre_existing = std::env::temp_dir();
            pre_existing.push("test");
            let _file = std::fs::File::create(&pre_existing).unwrap();
            let sock = TempPathBuf::new("test");
            let sock_path = std::path::PathBuf::from(sock.as_ref());
            assert!(sock_path.exists());
            std::mem::drop(sock);
            assert!(!sock_path.exists());
        }

        #[tokio::test]
        async fn temp_path_buf_for_socket() {
            let sock = TempPathBuf::with_random("test");
            // Save path for testing after drop
            let sock_path = std::path::PathBuf::from(sock.as_ref());
            // create the actual socket
            let _ = listen(&sock, SymmetricalJson::<String>::default).await;
            assert!(sock_path.exists());
            std::mem::drop(sock);
            assert!(!sock_path.exists());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Transport;
    use assert_matches::assert_matches;
    use futures::{task::*, Sink, Stream};
    #[cfg(any(feature = "tcp", all(unix, feature = "unix")))]
    use futures::{SinkExt, StreamExt};
    use pin_utils::pin_mut;
    use std::{
        io::{self, Cursor},
        pin::Pin,
    };
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio_serde::formats::SymmetricalJson;

    fn ctx() -> Context<'static> {
        Context::from_waker(noop_waker_ref())
    }

    struct TestIo(Cursor<Vec<u8>>);

    impl AsyncRead for TestIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            AsyncRead::poll_read(Pin::new(&mut self.0), cx, buf)
        }
    }

    impl AsyncWrite for TestIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            AsyncWrite::poll_write(Pin::new(&mut self.0), cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            AsyncWrite::poll_flush(Pin::new(&mut self.0), cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            AsyncWrite::poll_shutdown(Pin::new(&mut self.0), cx)
        }
    }

    #[test]
    fn close() {
        let (tx, _rx) = crate::transport::channel::bounded::<(), ()>(0);
        pin_mut!(tx);
        assert_matches!(tx.as_mut().poll_close(&mut ctx()), Poll::Ready(Ok(())));
        assert_matches!(tx.as_mut().start_send(()), Err(_));
    }

    #[test]
    fn test_stream() {
        let data: &[u8] = b"\x00\x00\x00\x18\"Test one, check check.\"";
        let transport = Transport::from((
            TestIo(Cursor::new(Vec::from(data))),
            SymmetricalJson::<String>::default(),
        ));
        pin_mut!(transport);

        assert_matches!(
            transport.as_mut().poll_next(&mut ctx()),
            Poll::Ready(Some(Ok(ref s))) if s == "Test one, check check.");
        assert_matches!(transport.as_mut().poll_next(&mut ctx()), Poll::Ready(None));
    }

    #[test]
    fn test_sink() {
        let writer = Cursor::new(vec![]);
        let mut transport = Box::pin(Transport::from((
            TestIo(writer),
            SymmetricalJson::<String>::default(),
        )));

        assert_matches!(
            transport.as_mut().poll_ready(&mut ctx()),
            Poll::Ready(Ok(()))
        );
        assert_matches!(
            transport
                .as_mut()
                .start_send("Test one, check check.".into()),
            Ok(())
        );
        assert_matches!(
            transport.as_mut().poll_flush(&mut ctx()),
            Poll::Ready(Ok(()))
        );
        assert_eq!(
            transport.get_ref().0.get_ref(),
            b"\x00\x00\x00\x18\"Test one, check check.\""
        );
    }

    #[cfg(feature = "tcp")]
    #[tokio::test]
    async fn tcp() -> io::Result<()> {
        use super::tcp;

        let mut listener = tcp::listen("0.0.0.0:0", SymmetricalJson::<String>::default).await?;
        let addr = listener.local_addr();
        tokio::spawn(async move {
            let mut transport = listener.next().await.unwrap().unwrap();
            let message = transport.next().await.unwrap().unwrap();
            transport.send(message).await.unwrap();
        });
        let mut transport = tcp::connect(addr, SymmetricalJson::<String>::default).await?;
        transport.send(String::from("test")).await?;
        assert_matches!(transport.next().await, Some(Ok(s)) if s == "test");
        assert_matches!(transport.next().await, None);
        Ok(())
    }

    #[cfg(feature = "tcp")]
    #[tokio::test]
    async fn tcp_on_existing_transport() -> io::Result<()> {
        use super::tcp;

        let transport = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
        let mut listener = tcp::listen_on(transport, SymmetricalJson::<String>::default).await?;
        let addr = listener.local_addr();
        tokio::spawn(async move {
            let mut transport = listener.next().await.unwrap().unwrap();
            let message = transport.next().await.unwrap().unwrap();
            transport.send(message).await.unwrap();
        });
        let mut transport = tcp::connect(addr, SymmetricalJson::<String>::default).await?;
        transport.send(String::from("test")).await?;
        assert_matches!(transport.next().await, Some(Ok(s)) if s == "test");
        assert_matches!(transport.next().await, None);
        Ok(())
    }

    #[cfg(all(unix, feature = "unix"))]
    #[tokio::test]
    async fn uds() -> io::Result<()> {
        use super::unix;

        let sock = unix::TempPathBuf::with_random("uds");
        let mut listener = unix::listen(&sock, SymmetricalJson::<String>::default).await?;
        tokio::spawn(async move {
            let mut transport = listener.next().await.unwrap().unwrap();
            let message = transport.next().await.unwrap().unwrap();
            transport.send(message).await.unwrap();
        });
        let mut transport = unix::connect(&sock, SymmetricalJson::<String>::default).await?;
        transport.send(String::from("test")).await?;
        assert_matches!(transport.next().await, Some(Ok(s)) if s == "test");
        assert_matches!(transport.next().await, None);
        Ok(())
    }

    #[cfg(all(unix, feature = "unix"))]
    #[tokio::test]
    async fn uds_on_existing_transport() -> io::Result<()> {
        use super::unix;

        let sock = unix::TempPathBuf::with_random("uds");
        let transport = tokio::net::UnixListener::bind(&sock)?;
        let mut listener = unix::listen_on(transport, SymmetricalJson::<String>::default).await?;
        tokio::spawn(async move {
            let mut transport = listener.next().await.unwrap().unwrap();
            let message = transport.next().await.unwrap().unwrap();
            transport.send(message).await.unwrap();
        });
        let mut transport = unix::connect(&sock, SymmetricalJson::<String>::default).await?;
        transport.send(String::from("test")).await?;
        assert_matches!(transport.next().await, Some(Ok(s)) if s == "test");
        assert_matches!(transport.next().await, None);
        Ok(())
    }
}
