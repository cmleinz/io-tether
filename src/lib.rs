use std::{future::Future, task::Poll};

mod implementations;

enum Status<E> {
    Success,
    Failover(State<E>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State<E> {
    Eof,
    Err(E),
}

impl Into<std::io::Error> for State<std::io::Error> {
    fn into(self) -> std::io::Error {
        match self {
            State::Eof => std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Eof error"),
            State::Err(error) => error,
        }
    }
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

impl Context {
    pub fn reconnect_count(&self) -> usize {
        self.reconnection_attempts
    }
}

impl<I, T, R> Tether<I, T, R>
where
    T: TetherIo<I, Error = R::Error>,
    R: TetherResolver,
{
    pub(crate) fn poll_reconnect(
        &mut self,
        cx: &mut std::task::Context<'_>,
        mut state: State<R::Error>,
    ) -> Poll<Status<R::Error>> {
        loop {
            // NOTE: Prevent holding the ref to error outside this block
            let retry = {
                let mut resolver_pin = std::pin::pin!(&mut self.resolver);
                let resolver_fut = resolver_pin.disconnected(&self.context, &state);
                let resolver_fut_pin = std::pin::pin!(resolver_fut);
                ready::ready!(resolver_fut_pin.poll(cx))
            };

            if !retry {
                return Poll::Ready(Status::Failover(state));
            }

            let fut = T::connect(&self.initializer);
            let fut_pin = std::pin::pin!(fut);
            match ready::ready!(fut_pin.poll(cx)) {
                Ok(new_stream) => {
                    // NOTE: This is why we need the underlying stream to be Unpin, since we swap
                    // it with a new one of the same type. Not aware of a safe alternative
                    self.inner = new_stream;
                    return Poll::Ready(Status::Success);
                }
                Err(new_error) => state = State::Err(new_error),
            }
        }
    }
}

pub trait TetherResolver: Unpin {
    type Error;

    fn disconnected(
        &mut self,
        context: &Context,
        state: &State<Self::Error>,
    ) -> impl Future<Output = bool> + Send;

    fn eof_triggers_reconnect(&mut self) -> bool;
}

pub trait TetherIo<T>: Sized + Unpin {
    type Error;

    fn connect(initializer: &T) -> impl Future<Output = Result<Self, Self::Error>> + Send;
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
