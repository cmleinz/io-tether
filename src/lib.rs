#![doc = include_str!("../README.md")]
use std::{cell::RefCell, future::Future, pin::Pin, sync::Arc};

mod implementations;

/// Represents a type which drives reconnects
///
/// Since the disconnected method asynchronous, and is invoked when the underlying stream
/// disconnects, calling asynchronous functions like
/// [`tokio::time::sleep`](https://docs.rs/tokio/latest/tokio/time/fn.sleep.html) from within the
/// body, work.
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
        state: &State,
    ) -> impl Future<Output = bool> + Send;

    fn reconnected(&mut self, _context: &Context) -> impl Future<Output = ()> + Send {
        std::future::ready(())
    }
}

/// Represents an I/O source capable of reconnecting
///
/// This trait is implemented for a number of types in the library, with the implementations placed
/// behind feature flags
pub trait TetherIo<T>: Sized + Unpin {
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

struct DisconnectedFut<T: TetherResolver> {
    context: Arc<RefCell<Context>>,
    state: Arc<RefCell<State>>,
    resolver: Arc<RefCell<T>>,
}

impl<T> DisconnectedFut<T>
where
    T: 'static + TetherResolver,
{
    fn into_pin(self) -> Pin<Box<dyn Future<Output = bool>>> {
        Box::pin(async move {
            let resolver = &mut *self.resolver.borrow_mut();
            let context = &*self.context.borrow();
            let state = &*self.state.borrow();

            resolver.disconnected(context, state).await
        })
    }
}

struct ReconnectedFut<T: TetherResolver> {
    context: Arc<RefCell<Context>>,
    resolver: Arc<RefCell<T>>,
}

impl<T> ReconnectedFut<T>
where
    T: 'static + TetherResolver,
{
    fn into_pin(self) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(async move {
            let resolver = &mut *self.resolver.borrow_mut();
            let context = &*self.context.borrow();

            resolver.reconnected(context).await
        })
    }
}

/// The underlying cause of the I/O disconnect
///
/// Currently this is either an error, or an 'end of file'.
pub enum State {
    /// End of File
    ///
    /// # Note
    ///
    /// This is also emitted when the other half of a TCP connection is closed.
    Eof,
    /// An I/O Error occurred
    Err(std::io::Error),
}

impl From<&State> for std::io::Error {
    fn from(value: &State) -> Self {
        match value {
            State::Eof => std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Eof error"),
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
pub struct Tether<I, T: TetherIo<I>, R: TetherResolver> {
    context: Arc<RefCell<Context>>,
    initializer: I,
    inner: T,
    resolver: Arc<RefCell<R>>,
    state: Arc<RefCell<State>>,
    futs: FutState<T>,
}

impl<I, T: TetherIo<I>, R: TetherResolver> Tether<I, T, R> {
    /// Construct a tether object from an existing I/O source
    ///
    /// # Note
    ///
    /// Often a simpler way to construct a [`Tether`] object is through [`Tether::connect`]
    pub fn new(inner: T, initializer: I, resolver: R) -> Self {
        Self {
            context: Default::default(),
            initializer,
            inner,
            resolver: Arc::new(RefCell::new(resolver)),
            futs: Default::default(),
            state: Arc::new(RefCell::new(State::Eof)),
        }
    }

    fn disconnected_fut(&self) -> DisconnectedFut<R> {
        DisconnectedFut {
            context: self.context.clone(),
            state: self.state.clone(),
            resolver: self.resolver.clone(),
        }
    }

    fn reconnected_fut(&self) -> ReconnectedFut<R> {
        ReconnectedFut {
            context: self.context.clone(),
            resolver: self.resolver.clone(),
        }
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
}

impl<I, T, R> Tether<I, T, R>
where
    R: TetherResolver,
    T: TetherIo<I>,
    I: Clone,
{
    /// Connect to the I/O source
    ///
    /// Invokes [`TetherIo::connect`] to establish the connection, the same method which is called
    /// when Tether attempts to reconnect.
    pub async fn connect(initializer: I, resolver: R) -> Result<Self, std::io::Error> {
        let inner = T::connect(initializer.clone()).await?;

        Ok(Self::new(inner, initializer, resolver))
    }
}

#[derive(Default)]
enum FutState<T> {
    #[default]
    Connected,
    Disconnected(Pin<Box<dyn Future<Output = bool>>>),
    Reconnecting(Pin<Box<dyn Future<Output = Result<T, std::io::Error>>>>),
    Reconnected(Pin<Box<dyn Future<Output = ()>>>),
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
