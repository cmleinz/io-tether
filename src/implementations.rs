use std::{ops::ControlFlow, pin::Pin, task::Poll};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Reason, State, TetherInner};

use super::{ready::ready, Io, Resolver, Tether};

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
                        let init = $me.inner.initializer.clone();
                        let reconnect_fut = $me.inner.connector.reconnect(init);
                        $me.state = State::Reconnecting(reconnect_fut);
                    } else {
                        let err = $me.inner.reason.take().into();
                        return Poll::Ready(Err(err));
                    }
                }
                State::Reconnecting(ref mut fut) => {
                    let result = ready!(fut.as_mut().poll($cx));
                    $me.inner.context.increment_attempts();

                    match result {
                        Ok(new_io) => {
                            $me.inner.io = new_io;
                            let fut = $me.inner.reconnected();
                            $me.state = State::Reconnected(fut);
                        }
                        Err(error) => $me.inner.reason = Reason::Err(error),
                    }
                }
                State::Reconnected(ref mut fut) => {
                    ready!(fut.as_mut().poll($cx));
                    $me.reconnect();
                }
            }
        }
    };
}

impl<I, T, R> TetherInner<I, T, R>
where
    T: Io<I>,
    T::Output: AsyncRead,
    I: Unpin + Clone,
    R: 'static + Resolver,
{
    fn poll_read_inner(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<ControlFlow<std::io::Result<()>, State<T::Output>>> {
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
                me.reason = Reason::Eof;
                let fut = self.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
            Ok(_) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                me.reason = Reason::Err(error);
                let fut = self.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
        }
    }
}

impl<I, T, R> AsyncRead for Tether<I, T, R>
where
    T: Io<I>,
    T::Output: AsyncRead,
    I: Unpin + Clone,
    R: 'static + Resolver,
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

impl<I, T, R> TetherInner<I, T, R>
where
    T: Io<I>,
    T::Output: AsyncWrite,
    I: Unpin + Clone,
    R: 'static + Resolver,
{
    fn poll_write_inner(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<ControlFlow<std::io::Result<usize>, State<T::Output>>> {
        let mut me = self.as_mut();

        let result = {
            let inner_pin = std::pin::pin!(&mut me.io);
            ready!(inner_pin.poll_write(cx, buf))
        };

        match result {
            Ok(0) => {
                me.reason = Reason::Eof;
                let fut = me.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
            Ok(wrote) => Poll::Ready(ControlFlow::Break(Ok(wrote))),
            Err(error) => {
                me.reason = Reason::Err(error);
                let fut = me.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
        }
    }

    fn poll_flush_inner(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<std::io::Result<()>, State<T::Output>>> {
        let mut me = self.as_mut();

        let result = {
            let inner_pin = std::pin::pin!(&mut me.io);
            ready!(inner_pin.poll_flush(cx))
        };

        match result {
            Ok(()) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                me.reason = Reason::Err(error);
                let fut = me.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
        }
    }

    fn poll_shutdown_inner(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<std::io::Result<()>, State<T::Output>>> {
        let mut me = self.as_mut();

        let result = {
            let inner_pin = std::pin::pin!(&mut me.io);
            ready!(inner_pin.poll_shutdown(cx))
        };

        match result {
            Ok(()) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                me.reason = Reason::Err(error);
                let fut = me.disconnected();
                Poll::Ready(ControlFlow::Continue(State::Disconnected(fut)))
            }
        }
    }
}

impl<I, T, R> AsyncWrite for Tether<I, T, R>
where
    T: Io<I>,
    T::Output: AsyncWrite,
    I: Unpin + Clone,
    R: 'static + Resolver,
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
