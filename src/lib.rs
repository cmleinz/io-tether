#![doc = include_str!("../README.md")]
use std::{future::Future, task::Poll};

mod implementations;

/// Represents a type which drives reconnects
///
/// Since the disconnected method asynchronous, and is invoked when the underlying stream
/// disconnects things like `tokio::time::sleep` work out of the box.
pub trait TetherResolver: Unpin {
    type Error;

    fn disconnected(
        &mut self,
        context: &Context,
        state: &State<Self::Error>,
    ) -> impl Future<Output = bool> + Send;

    fn eof_triggers_reconnect(&mut self) -> bool;
}

/// Represents an I/O source capable of reconnecting
///
/// This trait is implemented for a number of types in the library, with the implementations placed
/// behind feature flags
pub trait TetherIo<T>: Sized + Unpin {
    type Error;

    fn connect(initializer: &T) -> impl Future<Output = Result<Self, Self::Error>> + Send;
}

enum Status<E> {
    Success,
    Failover(State<E>),
}

/// The
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

/// A wrapper type which contains the underying I/O object, it's initializer, and resolver.
pub struct Tether<I, T, R> {
    context: Context,
    initializer: I,
    inner: T,
    resolver: R,
}

impl<I, T, R> Tether<I, T, R> {
    pub fn get_resolver(&self) -> &R {
        &self.resolver
    }

    pub fn get_resolver_mut(&mut self) -> &mut R {
        &mut self.resolver
    }

    /// Consume the Tether, and return the underlying I/O type
    pub fn into_inner(self) -> T {
        self.inner
    }

    pub fn get_inner(&self) -> &T {
        &self.inner
    }

    pub fn get_inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    pub fn get_context(&self) -> &Context {
        &self.context
    }

    pub fn get_context_mut(&mut self) -> &mut Context {
        &mut self.context
    }
}

impl<I, T, R> Tether<I, T, R>
where
    T: TetherIo<I>,
{
    pub async fn new(initializer: I, resolver: R) -> Result<Self, T::Error> {
        let inner = T::connect(&initializer).await?;

        let me = Self {
            context: Context::default(),
            initializer,
            inner,
            resolver,
        };

        Ok(me)
    }
}

/// Contains metrics about the underlying connection
///
/// Passed to the [`TetherResolver`], with each call to `disconnect`.
///
/// Currently tracks the number of reconnect attempts, but in the future may be expanded to include
/// additional metrics.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
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
            self.context.reconnection_attempts += 1;

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
