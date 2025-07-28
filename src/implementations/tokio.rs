use std::{ops::ControlFlow, pin::Pin, task::Poll};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Reason, Source, State, TetherInner, ready::ready};

use super::{Connector, IoInto, Resolver, Tether};

use super::connected::connected;

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
