use std::{ops::ControlFlow, pin::Pin, task::Poll};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Reason, Source, State, TetherInner};

use super::{Connector, Resolver, Tether, ready::ready};

#[cfg(feature = "sink")]
mod sink;
#[cfg(feature = "stream")]
mod stream;

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

pub(crate) mod connected {
    macro_rules! connected {
        ($me:expr, $poll_method:ident, $cx:expr, $default:expr, $($args:expr),*) => {
            loop {
                match $me.state {
                    State::Connected => {
                        let cont = ready!($me.inner.$poll_method(&mut $me.state, $cx, $($args),*));

                        if let ControlFlow::Break(val) = cont {
                            return Poll::Ready(val);
                        }
                    }
                    State::Disconnected(ref mut fut) => {
                        let retry = ready!(fut.as_mut().poll($cx));

                        match retry {
                            crate::Action::AttemptReconnect => {
                                $me.inner.set_reconnecting(&mut $me.state);
                                continue;
                            }
                            crate::Action::Exhaust => {
                                let opt_reason = $me.inner.context.reason.take();
                                let reason = opt_reason.expect("Can only enter Disconnected state with Reason");

                                let reason = match ($me.inner.config.error_propagation_on_no_retry, reason.1) {
                                    (crate::config::ErrorPropagation::IoOperations, Source::Io) => reason.0.io_into(),
                                    (crate::config::ErrorPropagation::All, _) => reason.0.io_into(),
                                    _ => $default,
                                };

                                return Poll::Ready(reason);
                            }
                            crate::Action::Ignore => {
                                $me.inner.set_connected(&mut $me.state);
                                continue;
                            }
                        }

                    }
                    State::Reconnecting(ref mut fut) => {
                        let result = ready!(fut.as_mut().poll($cx));
                        $me.inner.context.increment_attempts();

                        match result {
                            Ok(new_io) => {
                                $me.inner.set_reconnected(&mut $me.state, new_io);
                            }
                            Err(error) => {
                                $me.inner.set_disconnected(&mut $me.state, Reason::Err(error), Source::Reconnect);
                            },
                        }
                    }
                    State::Reconnected(ref mut fut) => {
                        ready!(fut.as_mut().poll($cx));
                        $me.inner.set_connected(&mut $me.state);
                    }
                }
            }
        };
    }

    pub(crate) use connected;
}

use connected::connected;

impl<C, R> TetherInner<C, R>
where
    C: Connector + Unpin,
    C::Output: AsyncRead + Unpin,
    R: Resolver<C> + Unpin,
{
    fn poll_read_inner(
        &mut self,
        state: &mut State<C::Output>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<ControlFlow<std::io::Result<()>>> {
        let result = {
            let depth = buf.filled().len();
            let inner_pin = std::pin::pin!(&mut self.io);
            let result = ready!(inner_pin.poll_read(cx, buf));
            let read_bytes = buf.filled().len().saturating_sub(depth);
            result.map(|_| read_bytes)
        };

        match result {
            Ok(0) => {
                self.set_disconnected(state, Reason::Eof, Source::Io);
                Poll::Ready(ControlFlow::Continue(()))
            }
            Ok(_) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                self.set_disconnected(state, Reason::Err(error), Source::Io);
                Poll::Ready(ControlFlow::Continue(()))
            }
        }
    }
}

impl<C, R> AsyncRead for Tether<C, R>
where
    C: Connector + Unpin,
    C::Output: AsyncRead + Unpin,
    R: Resolver<C> + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();

        connected!(me, poll_read_inner, cx, Ok(()), buf);
    }
}

impl<C, R> TetherInner<C, R>
where
    C: Connector + Unpin,
    C::Output: AsyncWrite + Unpin,
    R: Resolver<C> + Unpin,
{
    fn poll_write_inner(
        &mut self,
        state: &mut State<C::Output>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<ControlFlow<std::io::Result<usize>>> {
        if let Some(reason) = self.last_write.take() {
            self.set_disconnected(state, reason, Source::Io);
            return Poll::Ready(ControlFlow::Continue(()));
        }

        let result = {
            let inner_pin = std::pin::pin!(&mut self.io);
            ready!(inner_pin.poll_write(cx, buf))
        };

        // NOTE: It is important that in error branches we return ControlFlow::Continue. Otherwise,
        // we will break out of the reconnect loop, and drop the data that was written by the caller
        let reason = match result {
            Ok(0) => Reason::Eof,
            Ok(wrote) => return Poll::Ready(ControlFlow::Break(Ok(wrote))),
            Err(error) => Reason::Err(error),
        };

        if !self.config.keep_data_on_failed_write {
            self.last_write = Some(reason);
            // NOTE: We have no control over the buffer that is passed to us. The only way we can
            // ensure we are not passed the same buffer the next call, is by reporting that we
            // successfully wrote the data to the underlying object.
            //
            // This is not ideal, but it is the best we can do for now
            return Poll::Ready(ControlFlow::Break(Ok(buf.len())));
        }

        self.set_disconnected(state, reason, Source::Io);
        Poll::Ready(ControlFlow::Continue(()))
    }

    fn poll_flush_inner(
        &mut self,
        state: &mut State<C::Output>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<std::io::Result<()>>> {
        let result = {
            let inner_pin = std::pin::pin!(&mut self.io);
            ready!(inner_pin.poll_flush(cx))
        };

        match result {
            Ok(()) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                self.set_disconnected(state, Reason::Err(error), Source::Io);
                Poll::Ready(ControlFlow::Continue(()))
            }
        }
    }

    fn poll_shutdown_inner(
        &mut self,
        state: &mut State<C::Output>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<std::io::Result<()>>> {
        let result = {
            let inner_pin = std::pin::pin!(&mut self.io);
            ready!(inner_pin.poll_shutdown(cx))
        };

        match result {
            Ok(()) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                self.set_disconnected(state, Reason::Err(error), Source::Io);
                Poll::Ready(ControlFlow::Continue(()))
            }
        }
    }
}

impl<C, R> AsyncWrite for Tether<C, R>
where
    C: Connector + Unpin,
    C::Output: AsyncWrite + Unpin,
    R: Resolver<C> + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let me = self.get_mut();

        connected!(me, poll_write_inner, cx, Ok(0), buf);
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let me = self.get_mut();

        connected!(me, poll_flush_inner, cx, Ok(()),);
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let me = self.get_mut();

        connected!(me, poll_shutdown_inner, cx, Ok(()),);
    }
}
