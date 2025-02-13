#![doc = include_str!("../README.md")]
use std::{future::Future, io::ErrorKind, pin::Pin};

mod implementations;

pub type PinFut<O> = Pin<Box<dyn Future<Output = O> + 'static + Send>>;

/// Represents a type which drives reconnects
///
/// Since the disconnected method asynchronous, and is invoked when the underlying stream
/// disconnects, calling asynchronous functions like
/// [`tokio::time::sleep`](https://docs.rs/tokio/latest/tokio/time/fn.sleep.html) from within the
/// body, work.
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
/// # use io_tether::{Context, State, Resolver, PinFut};
/// pub struct RetryResolver(bool);
///
/// impl Resolver for RetryResolver {
///     fn disconnected(&mut self, context: &Context, state: &State) -> PinFut<bool> {
///         println!("WARN: Disconnected from server {:?}", state);
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
    /// The [`State`] will describe the type of the underlying error. It can either be `State::Eof`,
    /// in which case the end of file was reached, or an error. This information can be leveraged
    /// in this function to determine whether to attempt to reconnect.
    ///
    fn disconnected(&mut self, context: &Context, state: &State) -> PinFut<bool>;

    /// Invoked within [`Tether::connect`] if the initial connection attempt fails
    ///
    /// As with [`Self::disconnected`] the returned boolean determines whether the initial
    /// connection attempt is retried
    fn unreachable(&mut self, context: &Context, state: &State) -> PinFut<bool> {
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
    /// Initializes the connection to the I/O source
    fn connect(
        initializer: T,
    ) -> impl Future<Output = Result<Self, std::io::Error>> + 'static + Send;

    /// Re-establishes the connection to the I/O source
    fn reconnect(
        initializer: T,
    ) -> impl Future<Output = Result<Self, std::io::Error>> + 'static + Send {
        Self::connect(initializer)
    }
}

/// The underlying cause of the I/O disconnect
///
/// Currently this is either an error, or an 'end of file'.
#[derive(Debug)]
pub enum State {
    /// Represents the end of the file for the underlying io
    ///
    /// This can occur when the end of a file is read from the filesystem, when the remote socket on
    /// a TCP connection is closed, etc. Generally it indicates a successful end of the connection
    Eof,
    /// An I/O Error occurred
    Err(std::io::Error),
}

impl State {
    /// A convenience function which returns whether the original error is capable of being retried
    pub fn retryable(&self) -> bool {
        use std::io::ErrorKind as Kind;

        match self {
            State::Eof => true,
            State::Err(error) => matches!(
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
}

impl From<&State> for std::io::Error {
    fn from(value: &State) -> Self {
        match value {
            State::Eof => std::io::Error::new(ErrorKind::UnexpectedEof, "Eof error"),
            State::Err(error) => {
                // TODO: This is pretty hacky there's probably a better way
                let kind = error.kind();
                let error = error.to_string();
                std::io::Error::new(kind, error)
            }
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
    state: StateMachine<T>,
    inner: TetherInner<I, T, R>,
}

/// The inner type for tether.
///
/// Helps satisfy the borrow checker when we need to mutate this while holding a mutable ref to the
/// larger futs state machine
struct TetherInner<I, T: Io<I>, R> {
    context: Context,
    initializer: I,
    io: T,
    resolver: R,
    state: State,
}

impl<I, T: Io<I>, R: Resolver> TetherInner<I, T, R> {
    fn disconnected(&mut self) -> PinFut<bool> {
        self.resolver.disconnected(&self.context, &self.state)
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
    pub fn new(inner: T, initializer: I, resolver: R, context: Context) -> Self {
        Self {
            state: Default::default(),
            inner: TetherInner {
                context,
                initializer,
                io: inner,
                resolver,
                state: State::Eof,
            },
        }
    }

    fn reconnect(&mut self) {
        self.state = StateMachine::Connected;
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
    pub fn into_inner(self) -> T {
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
    pub async fn connect(initializer: I, mut resolver: R) -> Result<Self, std::io::Error> {
        let mut context = Context::default();

        loop {
            let state = match T::connect(initializer.clone()).await {
                Ok(io) => {
                    resolver.established(&context).await;
                    context.reset();
                    return Ok(Self::new(io, initializer, resolver, context));
                }
                Err(error) => State::Err(error),
            };

            context.increment_attempts();

            if !resolver.unreachable(&context, &state).await {
                let State::Err(error) = state else {
                    unreachable!("state is immutable and established as Err above");
                };

                return Err(error);
            }
        }
    }
}

#[derive(Default)]
enum StateMachine<T> {
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
    /// The total number of times a reconnect has been attempted.
    ///
    /// The first time [`Resolver::disconnected`] or [`Resolver::unreachable`] is invoked this will
    /// return `1`.
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

    /// The number of reconnect attempts since the last successful connection. Reset each time
    /// the connection is established
    pub fn current_reconnect_attempts(&self) -> usize {
        self.current_attempts
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
