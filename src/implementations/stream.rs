use std::{ops::ControlFlow, task::Poll};

use futures_core::Stream;

use crate::{
    Connector, Reason, Resolver, Source, State, Tether, TetherInner,
    implementations::{IoInto, connected::connected},
    ready::ready,
};

impl<T> IoInto<Option<Result<T, std::io::Error>>> for Reason {
    fn io_into(self) -> Option<Result<T, std::io::Error>> {
        match self {
            Reason::Eof => None,
            Reason::Err(error) => Some(Err(error)),
        }
    }
}

impl<C, R, T, E> TetherInner<C, R>
where
    C: Connector + Unpin,
    C::Output: Stream<Item = Result<T, E>> + Unpin,
    R: Resolver<C> + Unpin,
    E: Into<std::io::Error>,
{
    fn poll_stream_inner(
        &mut self,
        state: &mut State<C::Output>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ControlFlow<Option<std::io::Result<T>>>> {
        let item = {
            let inner_pin = std::pin::pin!(&mut self.io);
            ready!(inner_pin.poll_next(cx))
        };

        match item {
            None => {
                self.set_disconnected(state, Reason::Eof, Source::Io);
                Poll::Ready(ControlFlow::Continue(()))
            }
            Some(Ok(value)) => Poll::Ready(ControlFlow::Break(Some(Ok(value)))),
            Some(Err(error)) => {
                self.set_disconnected(state, Reason::Err(error.into()), Source::Io);
                Poll::Ready(ControlFlow::Continue(()))
            }
        }
    }
}

impl<C, R, T, E> Stream for Tether<C, R>
where
    C: Connector + Unpin,
    C::Output: Stream<Item = Result<T, E>> + Unpin,
    R: Resolver<C> + Unpin,
    E: Into<std::io::Error>,
{
    type Item = Result<T, std::io::Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let me = self.get_mut();

        connected!(me, poll_stream_inner, cx, None,)
    }
}
