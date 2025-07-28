use std::{ops::ControlFlow, pin::Pin, task::Poll};

use futures_sink::Sink;

use crate::{
    Connector, Reason, Resolver, Source, State, Tether, TetherInner,
    implementations::{IoInto, connected::connected},
    ready::ready,
};

impl<C, R> TetherInner<C, R>
where
    C: Connector + Unpin,
    R: Resolver<C> + Unpin,
{
    fn poll_sink_ready_inner<I>(
        &mut self,
        state: &mut State<C::Output>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<Result<(), <C::Output as Sink<I>>::Error>>>
    where
        C::Output: Sink<I, Error = std::io::Error> + Unpin,
        I: AsRef<[u8]>,
    {
        let item = {
            let inner_pin = std::pin::pin!(&mut self.io);
            ready!(inner_pin.poll_ready(cx))
        };

        match item {
            Ok(_) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                self.set_disconnected(state, Reason::Err(error), Source::Io);
                Poll::Ready(ControlFlow::Continue(()))
            }
        }
    }

    fn poll_sink_flush_inner<I>(
        &mut self,
        state: &mut State<C::Output>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<Result<(), <C::Output as Sink<I>>::Error>>>
    where
        C::Output: Sink<I, Error = std::io::Error> + Unpin,
        I: AsRef<[u8]>,
    {
        let item = {
            let inner_pin = std::pin::pin!(&mut self.io);
            ready!(inner_pin.poll_flush(cx))
        };

        match item {
            Ok(_) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                self.set_disconnected(state, Reason::Err(error), Source::Io);
                Poll::Ready(ControlFlow::Continue(()))
            }
        }
    }

    fn poll_sink_close_inner<I>(
        &mut self,
        state: &mut State<C::Output>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<Result<(), <C::Output as Sink<I>>::Error>>>
    where
        C::Output: Sink<I, Error = std::io::Error> + Unpin,
        I: AsRef<[u8]>,
    {
        let item = {
            let inner_pin = std::pin::pin!(&mut self.io);
            ready!(inner_pin.poll_close(cx))
        };

        match item {
            Ok(_) => Poll::Ready(ControlFlow::Break(Ok(()))),
            Err(error) => {
                self.set_disconnected(state, Reason::Err(error), Source::Io);
                Poll::Ready(ControlFlow::Continue(()))
            }
        }
    }
}

impl<C, R, I> Sink<I> for Tether<C, R>
where
    C: Connector + Unpin,
    C::Output: Sink<I, Error = std::io::Error> + Unpin,
    R: Resolver<C> + Unpin,
    I: AsRef<[u8]>,
{
    type Error = std::io::Error;

    fn poll_ready(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let me = self.get_mut();

        connected!(me, poll_sink_ready_inner, cx, Ok(()),)
    }

    fn start_send(self: std::pin::Pin<&mut Self>, item: I) -> Result<(), Self::Error> {
        let me = self.get_mut();
        let pin_sink = Pin::new(&mut me.inner.io);
        pin_sink.start_send(item)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let me = self.get_mut();

        connected!(me, poll_sink_flush_inner, cx, Ok(()),)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let me = self.get_mut();

        connected!(me, poll_sink_close_inner, cx, Ok(()),)
    }
}
