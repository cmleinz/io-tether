#![doc = include_str!("../README.md")]
use std::{future::Future, task::Poll};

mod implementations;

/// Represents a type which drives reconnects
///
/// Since the disconnected method asynchronous, and is invoked when the underlying stream
/// disconnects, calling asynchronous functions like `tokio::time::sleep` from within the body,
/// work.
///
/// # Example
///
/// A very simple implementation may look something like the following:
///
/// ```ignore
/// # use io_tether::{Context, State, TetherResolver};
/// pub struct RetryResolver;
///
/// impl TetherResolver for RetryResolver {
///     type Error = std::io::Error;
///
///     async fn disconnected(&mut self, context: &Context, state: &State<Self::Error>) -> bool {
///         tracing::warn!(?state, "Disconnected from server");

///         if context.reconnect_count() >= 5 {
///             return false;
///         }
///
///         tokio::time::sleep(Duration::from_secs(10)).await;
///         true
///     }
/// }
/// ```
// TODO: Remove the Unpin restriction
pub trait TetherResolver: Unpin {
    type Error;

    /// Invoked by Tether when an error/disconnect is encountered.
    ///
    /// Returning `true` will result in a reconnect being attempted via `<T as TetherIo>::reconnect`,
    /// returning `false` will result in the error being returned from the originating call.
    ///
    /// # Note
    ///
    /// The [`State`] will describe the type of the underlying error. It can either be `State::Eof`,
    /// in which case the end of file was reached, or an error. This information can be leveraged
    /// in this function to determine whether to attempt to reconnect.
    fn disconnected(
        &mut self,
        context: &Context,
        state: &State<Self::Error>,
    ) -> impl Future<Output = bool> + Send;
}

/// Represents an I/O source capable of reconnecting
///
/// This trait is implemented for a number of types in the library, with the implementations placed
/// behind feature flags
pub trait TetherIo<T>: Sized + Unpin {
    type Error;

    /// Initializes the connection to the I/O source
    fn connect(initializer: &T) -> impl Future<Output = Result<Self, Self::Error>> + Send;

    /// Re-establishes the connection to the I/O source
    fn reconnect(initializer: &T) -> impl Future<Output = Result<Self, Self::Error>> + Send {
        Self::connect(initializer)
    }
}

enum Status<E> {
    Success,
    Failover(State<E>),
}

/// Disconnect state
///
/// Currently this is either an error, or an 'end of file'
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State<E> {
    /// End of File
    ///
    /// # Note
    ///
    /// This is also emitted when the other half of a TCP connection is closed.
    Eof,
    /// An I/O Error occurred
    Err(E),
}

impl From<State<std::io::Error>> for std::io::Error {
    fn from(value: State<std::io::Error>) -> Self {
        match value {
            State::Eof => std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Eof error"),
            State::Err(error) => error,
        }
    }
}

/// A wrapper type which contains the underlying I/O object, it's initializer, and resolver.
///
/// This in the main type exposed by the library. It implements [`AsyncRead`](tokio::io::AsyncRead)
/// and [`AsyncWrite`](tokio::io::AsyncWrite) whenever the underlying I/O object implements them.
///
/// Calling things like `read_buf` will result in the I/O automatically reconnecting
/// if an error is detected during the underlying I/O call.
///
/// # Note
///
/// Currently, there is no way to obtain a reference into the underlying I/O object. And the only
/// way to reclaim the inner I/O type is by calling [`Tether::into_inner`]. This is by design, since
/// in the future there may be reason to add unsafe code which cannot be guaranteed if outside
/// callers can obtain references. In the future I may add these as unsafe functions if those cases
/// can be described.
pub struct Tether<I, T, R> {
    context: Context,
    initializer: I,
    inner: T,
    resolver: R,
}

impl<I, T, R> Tether<I, T, R> {
    /// Construct a tether object from an existing I/O source
    pub fn new(inner: T, initializer: I, resolver: R) -> Self {
        Self {
            context: Context::default(),
            initializer,
            inner,
            resolver,
        }
    }

    /// Returns a reference to the resolver
    pub fn get_resolver(&self) -> &R {
        &self.resolver
    }

    /// Returns a mutable reference to the resolver
    pub fn get_resolver_mut(&mut self) -> &mut R {
        &mut self.resolver
    }

    /// Returns a reference to the initializer
    pub fn get_initializer(&self) -> &I {
        &self.initializer
    }

    /// Returns a mutable reference to the initializer
    pub fn get_initializer_mut(&mut self) -> &mut I {
        &mut self.initializer
    }

    /// Consume the Tether, and return the underlying I/O type
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Returns a reference to the context
    pub fn get_context(&self) -> &Context {
        &self.context
    }

    /// Returns a mutable reference to the context
    pub fn get_context_mut(&mut self) -> &mut Context {
        &mut self.context
    }
}

impl<I, T, R> Tether<I, T, R>
where
    T: TetherIo<I>,
{
    /// Connect to the I/O source
    ///
    /// Invokes TetherIo::connect to establish the connection, the same method which is called
    /// when Tether attempts to reconnect.
    pub async fn connect(initializer: I, resolver: R) -> Result<Self, T::Error> {
        let inner = T::connect(&initializer).await?;

        Ok(Self::new(inner, initializer, resolver))
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
    /// The number of times a reconnect has been attempted.
    ///
    /// The first time [`TetherResolver::disconnected`] is invoked this will return `1`.
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

            let fut = T::reconnect(&self.initializer);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::mpsc,
    };

    async fn create_tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (client, server) = tokio::join!(TcpStream::connect(addr), listener.accept());
        (client.unwrap(), server.unwrap().0)
    }

    pub struct CallbackResolver {
        inner: mpsc::Sender<()>,
    }

    impl TetherResolver for CallbackResolver {
        type Error = std::io::Error;

        async fn disconnected(&mut self, _context: &Context, _state: &State<Self::Error>) -> bool {
            self.inner.send(()).await.unwrap();
            false
        }
    }

    #[cfg(feature = "net")]
    #[tokio::test]
    async fn disconnect_triggers_callback() {
        let (tx, mut rx) = mpsc::channel(1);
        let (client, mut server) = create_tcp_pair().await;
        let resolver = CallbackResolver { inner: tx };

        let mut tether = Tether::new(client, "", resolver);
        let mut buf = Vec::new();

        server.write_all(b"foo-bar").await.unwrap();
        tether.read_buf(&mut buf).await.unwrap();

        assert_eq!(&buf, b"foo-bar");
        server.shutdown().await.unwrap();
        tether.read_buf(&mut buf).await.unwrap();
        assert!(rx.recv().await.is_some());
    }
}
