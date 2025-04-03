use std::{ops::ControlFlow, pin::Pin, task::Poll};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Reason, State, TetherInner};

use super::{Io, Resolver, Tether, ready::ready};

/// I want to avoid implementing From<Reason> for Result<T, std::io::Error>, because it's not a
/// generally applicable transformation. In the specific case of AsyncRead and AsyncWrite, we can
/// map to those, but there's no guarantee that Ok(0) always implies Eof for some arbitrary
/// Result<usize, std::io::Error>
trait IoInto<T>: Sized {
    fn io_into(self) -> T;
}

impl IoInto<Result<usize, std::io::Error>> for Reason {
    fn io_into(self) -> Result<usize, std::io::Error> {
        match self {
            Reason::Eof => Ok(0),
            Reason::Err(error) => Err(error),
        }
    }
}

impl IoInto<Result<(), std::io::Error>> for Reason {
    fn io_into(self) -> Result<(), std::io::Error> {
        match self {
            Reason::Eof => Ok(()),
            Reason::Err(error) => Err(error),
        }
    }
}

macro_rules! connected {
    ($me:expr, $poll_method:ident, $cx:expr, $($args:expr),*) => {
        loop {
            match $me.state {
                State::Connected => {
                    let new = Pin::new(&mut $me.inner);
                    let cont = ready!(new.$poll_method($cx, $($args),*));

                    match cont {
                        ControlFlow::Continue(fut) => $me.state = fut,
                        ControlFlow::Break(val) => return Poll::Ready(val),
                    }
                }
                State::Disconnected(ref mut fut) => {
                    let retry = ready!(fut.as_mut().poll($cx));

                    if retry {
                        $me.set_reconnecting();
                    } else {
                        let opt_reason = $me.inner.context.reason.take();
                        let reason = opt_reason.expect("Can only enter Disconnected state with Reason");
                        return Poll::Ready(reason.io_into());
                    }
                }
                State::Reconnecting(ref mut fut) => {
                    let result = ready!(fut.as_mut().poll($cx));
                    $me.inner.context.increment_attempts();

                    match result {
                        Ok(new_io) => {
                            $me.inner.io = new_io;
                            $me.set_reconnected();
                        }
                        Err(error) => {
                            $me.set_disconnected(Reason::Err(error));
                        },
                    }
                }
                State::Reconnected(ref mut fut) => {
                    ready!(fut.as_mut().poll($cx));
                    $me.set_connected();
                }
            }
        }
    };
}

impl<C, R> TetherInner<C, R>
where
    C: Io + Unpin,
    C::Output: AsyncRead + Unpin,
    R: Resolver<C> + Unpin,
{
    fn poll_read_inner(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<ControlFlow<std::io::Result<()>, State<C::Output>>> {
        let mut me = self.as_mut();

        let result = {
            let depth = buf.filled().len();
            let inner_pin = std::pin::pin!(&mut me.io);
            let result = ready!(inner_pin.poll_read(cx, buf));
            let read_bytes = buf.filled().len().saturating_sub(depth);
            result.map(|_| read_bytes)
        };

        match result {
            Ok(0) => {
                me.context.reason = Some(Reason::Eof);
                let fut = self.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
            Ok(_) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                me.context.reason = Some(Reason::Err(error));
                let fut = self.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
        }
    }
}

impl<C, R> AsyncRead for Tether<C, R>
where
    C: Io + Unpin,
    C::Output: AsyncRead + Unpin,
    R: Resolver<C> + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut me = self.as_mut();

        connected!(me, poll_read_inner, cx, buf);
    }
}

impl<C, R> TetherInner<C, R>
where
    C: Io + Unpin,
    C::Output: AsyncWrite + Unpin,
    R: Resolver<C> + Unpin,
{
    fn poll_write_inner(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<ControlFlow<std::io::Result<usize>, State<C::Output>>> {
        let mut me = self.as_mut();

        let result = {
            let inner_pin = std::pin::pin!(&mut me.io);
            ready!(inner_pin.poll_write(cx, buf))
        };

        match result {
            Ok(0) => {
                me.context.reason = Some(Reason::Eof);
                let fut = me.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
            Ok(wrote) => Poll::Ready(ControlFlow::Break(Ok(wrote))),
            Err(error) => {
                me.context.reason = Some(Reason::Err(error));
                let fut = me.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
        }
    }

    fn poll_flush_inner(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<std::io::Result<()>, State<C::Output>>> {
        let mut me = self.as_mut();

        let result = {
            let inner_pin = std::pin::pin!(&mut me.io);
            ready!(inner_pin.poll_flush(cx))
        };

        match result {
            Ok(()) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                me.context.reason = Some(Reason::Err(error));
                let fut = me.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
        }
    }

    fn poll_shutdown_inner(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<std::io::Result<()>, State<C::Output>>> {
        let mut me = self.as_mut();

        let result = {
            let inner_pin = std::pin::pin!(&mut me.io);
            ready!(inner_pin.poll_shutdown(cx))
        };

        match result {
            Ok(()) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                me.context.reason = Some(Reason::Err(error));
                let fut = me.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
        }
    }
}

impl<C, R> AsyncWrite for Tether<C, R>
where
    C: Io + Unpin,
    C::Output: AsyncWrite + Unpin,
    R: Resolver<C> + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let mut me = self.as_mut();

        connected!(me, poll_write_inner, cx, buf);
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let mut me = self.as_mut();

        connected!(me, poll_flush_inner, cx,);
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let mut me = self.as_mut();

        connected!(me, poll_shutdown_inner, cx,);
    }
}
