use std::{future::Future, io, pin::Pin, task::Poll};

use pin_project_lite::pin_project;

mod implementations;

enum Status<E> {
    Success,
    Failover(Option<E>),
}

pub struct Tether<I, T, R> {
    context: Context,
    initializer: I,
    inner: T,
    resolver: R,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    reconnection_attempts: usize,
}

impl<I, T, R> Tether<I, T, R>
where
    T: TetherIo<I, Error = R::Error>,
    R: TetherResolver,
{
    pub(crate) fn poll_reconnect(
        &mut self,
        cx: &mut std::task::Context<'_>,
        mut error: Option<R::Error>,
    ) -> Poll<Status<R::Error>> {
        loop {
            // NOTE: Prevent holding the ref to error outside this block
            let retry = {
                let mut resolver_pin = std::pin::pin!(&mut self.resolver);
                let resolver_fut = resolver_pin.disconnected(&self.context, error.as_ref());
                let resolver_fut_pin = std::pin::pin!(resolver_fut);
                ready::ready!(resolver_fut_pin.poll(cx))
            };

            if !retry {
                return Poll::Ready(Status::Failover(error));
            }

            let fut = T::connect(&self.initializer);
            let fut_pin = std::pin::pin!(fut);
            match ready::ready!(fut_pin.poll(cx)) {
                Ok(new_stream) => {
                    // NOTE: This is why we need the underlying stream to be Unpin
                    self.inner = new_stream;
                    return Poll::Ready(Status::Success);
                }
                Err(new_error) => error = Some(new_error),
            }
        }
    }
}

pub trait TetherResolver: Unpin {
    type Error;

    async fn disconnected(&mut self, context: &Context, event: Option<&Self::Error>) -> bool;

    fn eof_triggers_reconnect(&mut self) -> bool;
}

pub trait TetherIo<T>: Sized + Unpin {
    type Error;

    fn connect(
        initializer: &T,
    ) -> impl std::future::Future<Output = Result<Self, Self::Error>> + Send;
}

pub(crate) mod ready {
    macro_rules! ready {
        ($e:expr $(,)?) => {
            match $e {
                std::task::Poll::Ready(t) => t,
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        };
    }

    pub(crate) use ready;
}
