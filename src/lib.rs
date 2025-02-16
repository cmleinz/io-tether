#![doc = include_str!("../README.md")]
use std::{future::Future, io::ErrorKind, pin::Pin};

mod implementations;
#[cfg(feature = "net")]
pub mod tcp;
#[cfg(all(feature = "net", target_family = "unix"))]
pub mod unix;

/// A dynamically dispatched static future
pub type PinFut<O> = Pin<Box<dyn Future<Output = O> + 'static + Send>>;

/// Represents a type which drives reconnects
///
/// Since the disconnected method asynchronous, and is invoked when the underlying stream
/// disconnects, calling asynchronous functions like
/// [`tokio::time::sleep`](https://docs.rs/tokio/latest/tokio/time/fn.sleep.html) from within the
/// body, work.
///
/// # Unpin
///
/// Since the method provides `&mut Self`, Self must be [`Unpin`]
///
/// # Return Type
///
/// The return types of the methods are [`PinFut`]. This has the requirement that the returned
/// future be 'static (cannot hold references to self, or any of the arguments). However, you are
/// still free to mutate data outside of the returned future.
///
/// Additionally, this method is invoked each time the I/O fails to establish a connection so
/// writing futures which do not reference their environment is a little easier than it may seem.
///
/// # Example
///
/// A very simple implementation may look something like the following:
///
/// ```no_run
/// # use std::time::Duration;
/// # use io_tether::{Context, Reason, Resolver, PinFut};
/// pub struct RetryResolver(bool);
///
/// impl Resolver for RetryResolver {
///     fn disconnected(&mut self, context: &Context, reason: &Reason) -> PinFut<bool> {
///         println!("WARN: Disconnected from server {:?}", reason);
///         self.0 = true;
///
///         if context.current_reconnect_attempts() >= 5 || context.total_reconnect_attempts() >= 50 {
///             return Box::pin(async move {false});
///         }
///
///         Box::pin(async move {
///             tokio::time::sleep(Duration::from_secs(10)).await;
///             true
///         })
///     }
/// }
/// ```
pub trait Resolver: Unpin {
    /// Invoked by Tether when an error/disconnect is encountered.
    ///
    /// Returning `true` will result in a reconnect being attempted via `<T as Io>::reconnect`,
    /// returning `false` will result in the error being returned from the originating call.
    ///
    /// # Note
    ///
    /// The [`Reason`] will describe the type of the underlying error. It can either be
    /// `State::Eof`, in which case the end of file was reached, or an error. This information can
    /// be leveraged in this function to determine whether to attempt to reconnect.
    fn disconnected(&mut self, context: &Context, state: &Reason) -> PinFut<bool>;

    /// Invoked within [`Tether::connect`] if the initial connection attempt fails
    ///
    /// As with [`Self::disconnected`] the returned boolean determines whether the initial
    /// connection attempt is retried
    fn unreachable(&mut self, context: &Context, state: &Reason) -> PinFut<bool> {
        self.disconnected(context, state)
    }

    /// Invoked within [`Tether::connect`] if the initial connection attempt succeeds
    fn established(&mut self, context: &Context) -> PinFut<()> {
        self.reconnected(context)
    }

    /// Invoked by Tether whenever the connection to the underlying I/O source has been
    /// re-established
    fn reconnected(&mut self, _context: &Context) -> PinFut<()> {
        Box::pin(std::future::ready(()))
    }
}

/// Represents an I/O source capable of reconnecting
///
/// This trait is implemented for a number of types in the library, with the implementations placed
/// behind feature flags
pub trait Io<T>: Sized + Unpin {
    type Output: Unpin;

    /// Initializes the connection to the I/O source
    fn connect(&mut self, initializer: T) -> PinFut<Result<Self::Output, std::io::Error>>;

    /// Re-establishes the connection to the I/O source
    fn reconnect(&mut self, initializer: T) -> PinFut<Result<Self::Output, std::io::Error>> {
        self.connect(initializer)
    }
}

/// Represents the reason the [`Resolver`] is invoked
///
/// Currently this is either an error, or an 'end of file'.
#[derive(Debug)]
#[non_exhaustive]
pub enum Reason {
    /// Represents the end of the file for the underlying io
    ///
    /// This can occur when the end of a file is read from the filesystem, when the remote socket on
    /// a TCP connection is closed, etc. Generally it indicates a successful end of the connection
    Eof,
    /// An I/O Error occurred
    Err(std::io::Error),
}

impl Reason {
    /// A convenience function which returns whether the original error is capable of being retried
    pub fn retryable(&self) -> bool {
        use std::io::ErrorKind as Kind;

        match self {
            Reason::Eof => true,
            Reason::Err(error) => matches!(
                error.kind(),
                Kind::NotFound
                    | Kind::PermissionDenied
                    | Kind::ConnectionRefused
                    | Kind::ConnectionAborted
                    | Kind::ConnectionReset
                    | Kind::NotConnected
                    | Kind::AlreadyExists
                    | Kind::HostUnreachable
                    | Kind::AddrNotAvailable
                    | Kind::NetworkDown
                    | Kind::BrokenPipe
                    | Kind::TimedOut
                    | Kind::UnexpectedEof
                    | Kind::NetworkUnreachable
                    | Kind::AddrInUse
            ),
        }
    }

    fn take(&mut self) -> Self {
        std::mem::replace(self, Self::Eof)
    }
}

impl From<Reason> for std::io::Error {
    fn from(value: Reason) -> Self {
        match value {
            Reason::Eof => std::io::Error::new(ErrorKind::UnexpectedEof, "Eof error"),
            Reason::Err(error) => error,
        }
    }
}

/// A wrapper type which contains the underlying I/O object, it's initializer, and resolver.
///
/// This in the main type exposed by the library. It implements [`AsyncRead`](tokio::io::AsyncRead)
/// and [`AsyncWrite`](tokio::io::AsyncWrite) whenever the underlying I/O object implements them.
///
/// Calling things like
/// [`read_buf`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncReadExt.html#method.read_buf) will
/// result in the I/O automatically reconnecting if an error is detected during the underlying I/O
/// call.
///
/// # Note
///
/// Currently, there is no way to obtain a reference into the underlying I/O object. And the only
/// way to reclaim the inner I/O type is by calling [`Tether::into_inner`]. This is by design, since
/// in the future there may be reason to add unsafe code which cannot be guaranteed if outside
/// callers can obtain references. In the future I may add these as unsafe functions if those cases
/// can be described.
pub struct Tether<I, T: Io<I>, R> {
    state: State<T::Output>,
    inner: TetherInner<I, T, R>,
}

/// The inner type for tether.
///
/// Helps satisfy the borrow checker when we need to mutate this while holding a mutable ref to the
/// larger futs state machine
struct TetherInner<I, T: Io<I>, R> {
    context: Context,
    initializer: I,
    connector: T,
    io: T::Output,
    resolver: R,
    reason: Reason,
}

impl<I, T: Io<I>, R: Resolver> TetherInner<I, T, R> {
    fn disconnected(&mut self) -> PinFut<bool> {
        self.resolver.disconnected(&self.context, &self.reason)
    }

    fn reconnected(&mut self) -> PinFut<()> {
        self.resolver.reconnected(&self.context)
    }
}

impl<I, T: Io<I>, R: Resolver> Tether<I, T, R> {
    /// Construct a tether object from an existing I/O source
    ///
    /// # Note
    ///
    /// Often a simpler way to construct a [`Tether`] object is through [`Tether::connect`]
    pub fn new(connector: T, io: T::Output, initializer: I, resolver: R) -> Self {
        Self::new_with_context(connector, io, initializer, resolver, Context::default())
    }

    fn new_with_context(
        connector: T,
        io: T::Output,
        initializer: I,
        resolver: R,
        context: Context,
    ) -> Self {
        Self {
            state: Default::default(),
            inner: TetherInner {
                context,
                initializer,
                io,
                resolver,
                reason: Reason::Eof,
                connector,
            },
        }
    }

    fn reconnect(&mut self) {
        self.state = State::Connected;
        self.inner.context.reset();
    }

    /// Returns a reference to the initializer
    pub fn get_initializer(&self) -> &I {
        &self.inner.initializer
    }

    /// Returns a mutable reference to the initializer
    pub fn get_initializer_mut(&mut self) -> &mut I {
        &mut self.inner.initializer
    }

    /// Consume the Tether, and return the underlying I/O type
    pub fn into_inner(self) -> T::Output {
        self.inner.io
    }
}

impl<I, T, R> Tether<I, T, R>
where
    R: Resolver,
    T: Io<I>,
    I: Clone,
{
    /// Connect to the I/O source, retrying on a failure.
    pub async fn connect(
        mut connector: T,
        initializer: I,
        mut resolver: R,
    ) -> Result<Self, std::io::Error> {
        let mut context = Context::default();

        loop {
            let state = match connector.connect(initializer.clone()).await {
                Ok(io) => {
                    resolver.established(&context).await;
                    context.reset();
                    return Ok(Self::new_with_context(
                        connector,
                        io,
                        initializer,
                        resolver,
                        context,
                    ));
                }
                Err(error) => Reason::Err(error),
            };

            context.increment_attempts();

            if !resolver.unreachable(&context, &state).await {
                let Reason::Err(error) = state else {
                    unreachable!("state is immutable and established as Err above");
                };

                return Err(error);
            }
        }
    }

    /// Connect to the I/O source, bypassing [`Resolver::unreachable`] implementation on a failure.
    ///
    /// This does still invoke [`Resolver::established`] if the connection is made successfully
    pub async fn connect_without_retry(
        mut connector: T,
        initializer: I,
        mut resolver: R,
    ) -> Result<Self, std::io::Error> {
        let context = Context::default();

        let io = connector.connect(initializer.clone()).await?;
        resolver.established(&context).await;
        Ok(Self::new_with_context(
            connector,
            io,
            initializer,
            resolver,
            context,
        ))
    }
}

/// The internal state machine which drives the connection and reconnect logic
#[derive(Default)]
enum State<T> {
    #[default]
    Connected,
    Disconnected(PinFut<bool>),
    Reconnecting(PinFut<Result<T, std::io::Error>>),
    Reconnected(PinFut<()>),
}

/// Contains metrics about the underlying connection
///
/// Passed to the [`Resolver`], with each call to `disconnect`.
///
/// Currently tracks the number of reconnect attempts, but in the future may be expanded to include
/// additional metrics.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Context {
    total_attempts: usize,
    current_attempts: usize,
}

impl Context {
    /// The number of reconnect attempts since the last successful connection. Reset each time
    /// the connection is established
    pub fn current_reconnect_attempts(&self) -> usize {
        self.current_attempts
    }

    /// The total number of times a reconnect has been attempted.
    ///
    /// The first time [`Resolver::disconnected`] or [`Resolver::unreachable`] is invoked this will
    /// return `0`, each subsequent time it will be incremented by 1.
    pub fn total_reconnect_attempts(&self) -> usize {
        self.total_attempts
    }

    fn increment_attempts(&mut self) {
        self.current_attempts += 1;
        self.total_attempts += 1;
    }

    fn reset(&mut self) {
        self.current_attempts = 0;
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
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    struct Once;

    impl Resolver for Once {
        fn disconnected(&mut self, context: &Context, _state: &Reason) -> PinFut<bool> {
            let retry = context.total_reconnect_attempts() < 1;

            Box::pin(async move { retry })
        }
    }

    #[tokio::test]
    async fn disconnect_is_retried() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut connections = 0;
            loop {
                let (mut stream, _addr) = listener.accept().await.unwrap();
                stream.write_u8(connections).await.unwrap();
                connections += 1;
            }
        });

        let mut stream = Tether::connect_tcp(addr, Once).await.unwrap();
        let mut buf = Vec::new();
        assert!(stream.read_to_end(&mut buf).await.is_err());
        assert_eq!(buf.as_slice(), &[0, 1])
    }
}
