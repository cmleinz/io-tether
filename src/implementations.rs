use std::{pin::Pin, task::Poll};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpStream, ToSocketAddrs},
};

use crate::{State, Status};

use super::{ready::ready, Tether, TetherIo, TetherResolver};

macro_rules! reconnect {
    ($me:ident, $cx:ident, $state:expr) => {
        match ready!($me.poll_reconnect($cx, $state)) {
            Status::Success => continue,
            Status::Failover(error) => return Poll::Ready(Err(error.into())),
        }
    };
    ($me:ident, $cx:ident, $state:expr, $eof:expr) => {
        match ready!($me.poll_reconnect($cx, $state)) {
            Status::Success => continue,
            Status::Failover(State::Err(error)) => return Poll::Ready(Err(error)),
            Status::Failover(State::Eof) => return Poll::Ready($eof),
        }
    };
}

impl<I, T, R> AsyncRead for Tether<I, T, R>
where
    T: AsyncRead + TetherIo<I, Error = std::io::Error>,
    I: Unpin,
    R: TetherResolver<Error = std::io::Error>,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();

        loop {
            let result = {
                let depth = buf.filled().len();
                let inner_pin = std::pin::pin!(&mut me.inner);
                let result = ready!(inner_pin.poll_read(cx, buf));
                let read_bytes = buf.filled().len().saturating_sub(depth);
                result.map(|_| read_bytes)
            };

            match result {
                Ok(0) => reconnect!(me, cx, State::Eof, Ok(())),
                Ok(_) => return Poll::Ready(Ok(())),
                Err(error) => reconnect!(me, cx, State::Err(error)),
            }
        }
    }
}

// TODO: Create generic version of this to avoid duplication
impl<I, T, R> AsyncWrite for Tether<I, T, R>
where
    T: AsyncWrite + TetherIo<I, Error = std::io::Error>,
    I: Unpin,
    R: TetherResolver<Error = std::io::Error>,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let me = self.get_mut();

        loop {
            let inner_pin = std::pin::pin!(&mut me.inner);
            let result = ready!(inner_pin.poll_write(cx, buf));

            match result {
                Ok(n) => return Poll::Ready(Ok(n)),
                Err(error) => reconnect!(me, cx, State::Err(error)),
            }
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let me = self.get_mut();

        loop {
            let inner_pin = std::pin::pin!(&mut me.inner);
            let result = ready!(inner_pin.poll_flush(cx));

            match result {
                Ok(()) => return Poll::Ready(Ok(())),
                Err(error) => reconnect!(me, cx, State::Err(error)),
            }
        }
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let me = self.get_mut();

        loop {
            let inner_pin = std::pin::pin!(&mut me.inner);
            let result = ready!(inner_pin.poll_shutdown(cx));

            match result {
                Ok(()) => return Poll::Ready(Ok(())),
                Err(error) => reconnect!(me, cx, State::Err(error)),
            }
        }
    }
}

impl<T> TetherIo<T> for TcpStream
where
    T: ToSocketAddrs + Clone + Send + Sync,
{
    type Error = std::io::Error;

    async fn connect(initializer: &T) -> Result<Self, Self::Error> {
        let addr = initializer.clone();
        TcpStream::connect(addr).await
    }
}
