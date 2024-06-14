use std::{pin::Pin, task::Poll};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{State, Status};

use super::{ready::ready, Tether, TetherIo, TetherResolver};

macro_rules! reconnect {
    ($cx:ident, $init_error:expr, $error:ident) => {
        match ready!(me.poll_reconnect($cx, $error)) {
            Status::Success => continue,
            Status::Failover(error) => return Poll::Ready(Err($error.unwrap())),
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
                Ok(0) if me.resolver.eof_triggers_reconnect() => {
                    match ready!(me.poll_reconnect(cx, State::Eof)) {
                        Status::Success => continue,
                        Status::Failover(error) => return Poll::Ready(Err(error.into())),
                    }
                }
                Ok(_) => return Poll::Ready(Ok(())),
                Err(error) => match ready!(me.poll_reconnect(cx, State::Err(error))) {
                    Status::Success => continue,
                    Status::Failover(error) => return Poll::Ready(Err(error.into())),
                },
            }
        }
    }
}

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
        todo!()
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        todo!()
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        todo!()
    }
}
