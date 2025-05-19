#![doc = include_str!("../README.md")]
use std::{ future::Future, io::ErrorKind, pin::Pin};

pub mod config;
#[cfg(feature = "fs")]
pub mod fs;
mod implementations;
#[cfg(feature = "net")]
pub mod tcp;
#[cfg(all(feature = "net", target_family = "unix"))]
pub mod unix;

use config::Config;

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
/// impl<C> Resolver<C> for RetryResolver {
///     fn disconnected(&mut self, context: &Context, _: &mut C) -> PinFut<bool> {
///         let reason = context.reason();
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
pub trait Resolver<C> {
    /// Invoked by Tether when an error/disconnect is encountered.
    ///
    /// Returning `true` will result in a reconnect being attempted via `<T as Io>::reconnect`,
    /// returning `false` will result in the error being returned from the originating call.
    fn disconnected(&mut self, context: &Context, connector: &mut C) -> PinFut<bool>;

    /// Invoked within [`Tether::connect`] if the initial connection attempt fails
    ///
    /// As with [`Self::disconnected`] the returned boolean determines whether the initial
    /// connection attempt is retried
    ///
    /// Defaults to invoking [`Self::disconnected`]
    fn unreachable(&mut self, context: &Context, connector: &mut C) -> PinFut<bool> {
        self.disconnected(context, connector)
    }

    /// Invoked within [`Tether::connect`] if the initial connection attempt succeeds
    ///
    /// Defaults to invoking [`Self::reconnected`]
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
pub trait Io {
    type Output;

    /// Initializes the connection to the I/O source
    fn connect(&mut self) -> PinFut<Result<Self::Output, std::io::Error>>;

    /// Re-establishes the connection to the I/O source
    fn reconnect(&mut self) -> PinFut<Result<Self::Output, std::io::Error>> {
        self.connect()
    }
}

/// Enum representing reasons for a disconnect
#[derive(Debug)]
#[non_exhaustive]
pub enum Reason {
    /// Represents the end of the file for the underlying io
    ///
    /// This can occur when the end of a file is read from the file system, when the remote socket
    /// on a TCP connection is closed, etc. Generally it indicates a successful end of the
    /// connection
    Eof,
    /// An I/O Error occurred
    Err(std::io::Error),
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reason::Eof => f.write_str("End of file detected"),
            Reason::Err(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Reason {}

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
/// # Example
///
/// ## Basic Resolver
///
/// Below is an example of a basic resolver which just logs the error and retries
///
/// ```no_run
/// # use io_tether::*;
/// # async fn foo() -> Result<(), Box<dyn std::error::Error>> {
/// struct MyResolver;
///
/// impl<C> Resolver<C> for MyResolver {
///     fn disconnected(&mut self, context: &Context, _: &mut C) -> PinFut<bool> {
///         println!("WARN(disconnect): {:?}", context);
///         Box::pin(async move { true }) // always immediately retry the connection
///     }
/// }
///
/// let stream = Tether::connect_tcp("localhost:8080", MyResolver).await?;
///
/// // Regardless of which half detects the disconnect, a reconnect will be attempted
/// let (read, write) = tokio::io::split(stream);
/// # Ok(()) }
/// ```
///
/// # Specialized Resolver
///
/// For more specialized use cases we can implement [`Resolver`] only for certain connectors to give
/// us extra control over the reconnect process.
///
/// ```
/// # use io_tether::{*, tcp::TcpConnector};
/// # use std::net::{SocketAddrV4, Ipv4Addr};
/// struct MyResolver;
///
/// type Connector = TcpConnector<SocketAddrV4>;
///
/// impl Resolver<Connector> for MyResolver {
///     fn disconnected(&mut self, context: &Context, conn: &mut Connector) -> PinFut<bool> {
///         // Because we've specialized our resolver to act on TcpConnector for IPv4, we can alter
///         // the address in between the disconnect, and the reconnect, to try a different host
///         conn.get_addr_mut().set_ip(Ipv4Addr::LOCALHOST);
///         conn.get_addr_mut().set_port(8082);
///
///         Box::pin(async move { true }) // always immediately retry the connection
///     }
/// }
/// ```
///
/// # Note
///
/// Currently, there is no way to obtain a reference into the underlying I/O object. And the only
/// way to reclaim the inner I/O type is by calling [`Tether::into_inner`].
pub struct Tether<C: Io, R> {
    state: State<C::Output>,
    inner: TetherInner<C, R>,
}

/// The inner type for tether.
///
/// Helps satisfy the borrow checker when we need to mutate this while holding a mutable ref to the
/// larger futs state machine
struct TetherInner<C: Io, R> {
    config: Config,
    connector: C,
    context: Context,
    io: C::Output,
    resolver: R,
    // Should only be acted on when Config::keep_data_on_failed_write is false
    last_write: Option<Reason>,
}

impl<C: Io, R: Resolver<C>> TetherInner<C, R> {
    fn set_connected(&mut self, state: &mut State<C::Output>) {
        *state = State::Connected;
        self.context.reset();
    }

    fn set_reconnected(&mut self, state: &mut State<C::Output>, new_io: <C as Io>::Output) {
        self.io = new_io;
        let fut = self.resolver.reconnected(&self.context);
        *state = State::Reconnected(fut);
    }

    fn set_reconnecting(&mut self, state: &mut State<C::Output>) {
        let fut = self.connector.reconnect();
        *state = State::Reconnecting(fut);
    }

    fn set_disconnected(&mut self, state: &mut State<C::Output>, reason: Reason, source: Source) {
        self.context.reason = Some((reason, source));
        let fut = self
            .resolver
            .disconnected(&self.context, &mut self.connector);
        *state = State::Disconnected(fut);
    }
}

impl<C: Io, R> Tether<C, R> {
    /// Returns a reference to the inner resolver
    pub fn resolver(&self) -> &R {
        &self.inner.resolver
    }

    /// Returns a reference to the inner connector
    pub fn connector(&self) -> &C {
        &self.inner.connector
    }

    /// Returns a reference to the context
    pub fn context(&self) -> &Context {
        &self.inner.context
    }
}

impl<C, R> Tether<C, R>
where
    C: Io,
    R: Resolver<C>,
{
    /// Construct a tether object from an existing I/O source
    ///
    /// # Warning
    ///
    /// Unlike [`Tether::connect`], this method does not invoke the resolver's `established` method.
    /// It is generally recommended that you use [`Tether::connect`].
    pub fn new(connector: C, io: C::Output, resolver: R) -> Self {
        Self::new_with_config(connector, io, resolver, Config::default())
    }

    pub fn new_with_config(connector: C, io: C::Output, resolver: R, config: Config) -> Self {
        Self::new_with_context(connector, io, resolver, Context::default(), config)
    }

    fn new_with_context(
        connector: C,
        io: C::Output,
        resolver: R,
        context: Context,
        config: Config,
    ) -> Self {
        Self {
            state: Default::default(),
            inner: TetherInner {
                config,
                connector,
                context,
                io,
                resolver,
                last_write: None,
            },
        }
    }

    /// Overrides the default configuration of the Tether object
    pub fn set_config(&mut self, config: Config) {
        self.inner.config = config;
    }

    /// Consume the Tether, and return the underlying I/O type
    #[inline]
    pub fn into_inner(self) -> C::Output {
        self.inner.io
    }

    /// Connect to the I/O source, retrying on a failure.
    pub async fn connect(mut connector: C, mut resolver: R) -> Result<Self, std::io::Error> {
        let mut context = Context::default();

        loop {
            let state = match connector.connect().await {
                Ok(io) => {
                    resolver.established(&context).await;
                    context.reset();
                    return Ok(Self::new_with_context(
                        connector,
                        io,
                        resolver,
                        context,
                        Config::default(),
                    ));
                }
                Err(error) => error,
            };

            context.reason = Some((Reason::Err(state), Source::Reconnect));
            context.increment_attempts();

            if !resolver.unreachable(&context, &mut connector).await {
                let Some((Reason::Err(error), _)) = context.reason else {
                    unreachable!("state is immutable and established as Err above");
                };

                return Err(error);
            }
        }
    }

    /// Connect to the I/O source, bypassing [`Resolver::unreachable`] implementation on a failure.
    ///
    /// This does still invoke [`Resolver::established`] if the connection is made successfully.
    /// To bypass both, construct the IO source and pass it to [`Self::new`].
    pub async fn connect_without_retry(
        mut connector: C,
        mut resolver: R,
    ) -> Result<Self, std::io::Error> {
        let context = Context::default();

        let io = connector.connect().await?;
        resolver.established(&context).await;
        Ok(Self::new_with_context(
            connector,
            io,
            resolver,
            context,
            Config::default(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Io,
    Reconnect,
}

/// Contains additional information about the disconnect
///
/// This type internally tracks the number of times a disconnect has occurred, and the reason for
/// the disconnect.
#[derive(Default, Debug)]
pub struct Context {
    total_attempts: usize,
    current_attempts: usize,
    reason: Option<(Reason, Source)>,
}

impl Context {
    /// The number of reconnect attempts since the last successful connection. Reset each time
    /// the connection is established
    #[inline]
    pub fn current_reconnect_attempts(&self) -> usize {
        self.current_attempts
    }

    /// The total number of times a reconnect has been attempted.
    ///
    /// The first time [`Resolver::disconnected`] or [`Resolver::unreachable`] is invoked this will
    /// return `0`, each subsequent time it will be incremented by 1.
    #[inline]
    pub fn total_reconnect_attempts(&self) -> usize {
        self.total_attempts
    }

    fn increment_attempts(&mut self) {
        self.current_attempts += 1;
        self.total_attempts += 1;
    }

    /// Get the current reason for the disconnect
    ///
    /// # Panics
    ///
    /// Might, panic if called outside of the methods in resolver. Will also panic if called AFTER
    /// and error has been returned
    #[inline]
    pub fn reason(&self) -> &Reason {
        self.try_reason().unwrap()
    }

    /// Get the current optional reason for the disconnect
    #[inline]
    pub fn try_reason(&self) -> Option<&Reason> {
        self.reason.as_ref().map(|val| &val.0)
    }

    /// Resets the current attempts, leaving the total reconnect attempts unchanged
    #[inline]
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
        io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_test::io::{Builder, Mock};

    use super::*;

    struct Value(bool);

    impl<T> Resolver<T> for Value {
        fn disconnected(&mut self, _context: &Context, _connector: &mut T) -> PinFut<bool> {
            let val = self.0;
            Box::pin(async move { val })
        }
    }

    struct Once;

    impl<T> Resolver<T> for Once {
        fn disconnected(&mut self, context: &Context, _connector: &mut T) -> PinFut<bool> {
            let retry = context.total_reconnect_attempts() < 1;

            Box::pin(async move { retry })
        }
    }

    fn other(err: &'static str) -> std::io::Error {
        std::io::Error::other(err)
    }

    trait ReadWrite: 'static + AsyncRead + AsyncWrite + Unpin {}
    impl<T: 'static + AsyncRead + AsyncWrite + Unpin> ReadWrite for T {}

    struct MockConnector<F>(F);

    impl<F: FnMut() -> Mock> Io for MockConnector<F> {
        type Output = Mock;

        fn connect(&mut self) -> PinFut<Result<Self::Output, std::io::Error>> {
            let value = self.0();

            Box::pin(async move { Ok(value) })
        }
    }

    async fn tester<A>(test: A, mock: impl ReadWrite, tether: impl ReadWrite)
    where
        A: AsyncFn(Box<dyn ReadWrite>) -> String,
    {
        let mock_result = (test)(Box::new(mock)).await;
        let tether_result = (test)(Box::new(tether)).await;

        assert_eq!(mock_result, tether_result);
    }

    async fn mock_acts_as_tether_mock<F, A>(mut gener: F, test: A)
    where
        F: FnMut() -> Mock + 'static + Unpin,
        A: AsyncFn(Box<dyn ReadWrite>) -> String,
    {
        let mock = gener();
        let tether_mock = Tether::connect(MockConnector(gener), Value(false))
            .await
            .unwrap();

        tester(test, mock, tether_mock).await
    }

    #[tokio::test]
    async fn single_read_then_eof() {
        let test = async |mut reader: Box<dyn ReadWrite>| {
            let mut output = String::new();
            reader.read_to_string(&mut output).await.unwrap();
            output
        };

        mock_acts_as_tether_mock(|| Builder::new().read(b"foobar").read(b"").build(), test).await;
    }

    #[tokio::test]
    async fn two_read_then_eof() {
        let test = async |mut reader: Box<dyn ReadWrite>| {
            let mut output = String::new();
            reader.read_to_string(&mut output).await.unwrap();
            output
        };

        let builder = || Builder::new().read(b"foo").read(b"bar").read(b"").build();

        mock_acts_as_tether_mock(builder, test).await;
    }

    #[tokio::test]
    async fn immediate_error() {
        let test = async |mut reader: Box<dyn ReadWrite>| {
            let mut output = String::new();
            let result = reader.read_to_string(&mut output).await;
            format!("{:?}", result)
        };

        let builder = || {
            Builder::new()
                .read_error(std::io::Error::other("oops!"))
                .build()
        };

        mock_acts_as_tether_mock(builder, test).await;
    }

    #[tokio::test]
    async fn basic_write() {
        let mock = || Builder::new().write(b"foo").write(b"bar").build();

        let mut tether = Tether::connect(MockConnector(mock), Once).await.unwrap();
        tether.write_all(b"foo").await.unwrap();
        tether.write_all(b"bar").await.unwrap(); // should trigger error which is propagated
    }

    #[tokio::test]
    async fn failure_to_connect_doesnt_panic() {
        struct Unreachable;
        impl<T> Resolver<T> for Unreachable {
            fn disconnected(&mut self, context: &Context, _connector: &mut T) -> PinFut<bool> {
                let _reason = context.reason(); // This should not panic
                Box::pin(async move { false })
            }
        }

        let result = Tether::connect_tcp("0.0.0.0:3150", Unreachable).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_then_disconnect() {
        struct AllowEof;
        impl<T> Resolver<T> for AllowEof {
            fn disconnected(&mut self, context: &Context, _connector: &mut T) -> PinFut<bool> {
                let value = !matches!(context.reason(), Reason::Eof); // Don't reconnect on EoF
                Box::pin(async move { value })
            }
        }

        let mock = Builder::new().read(b"foobarbaz").read(b"").build();
        let mut count = 0;
        // After each read call we error
        let b = move |v: &[u8]| Builder::new().read(v).read_error(other("error")).build();
        let gener = move || {
            let result = match count {
                0 => b(b"foo"),
                1 => b(b"bar"),
                2 => b(b"baz"),
                _ => Builder::new().read(b"").build(),
            };

            count += 1;
            result
        };

        let test = async |mut reader: Box<dyn ReadWrite>| {
            let mut output = String::new();
            reader.read_to_string(&mut output).await.unwrap();
            output
        };

        let tether_mock = Tether::connect(MockConnector(gener), AllowEof)
            .await
            .unwrap();

        tester(test, mock, tether_mock).await
    }

    #[tokio::test]
    async fn split_works() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut stream, _addr) = listener.accept().await.unwrap();
                stream.write_all(b"foobar").await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });

        let stream = Tether::connect_tcp(addr, Once).await.unwrap();
        let (mut read, mut write) = tokio::io::split(stream);
        let mut buf = [0u8; 6];
        read.read_exact(&mut buf).await.unwrap(); // Disconnect happens here
        assert_eq!(&buf, b"foobar");
        write.write_all(b"foobar").await.unwrap(); // Reconnect is triggered
    }

    #[tokio::test]
    async fn reconnect_value_is_respected() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _addr) = listener.accept().await.unwrap();
            stream.write_all(b"foobar").await.unwrap();
            stream.shutdown().await.unwrap();
        });

        // We set it to not reconnect, thus we expect this to work exactly as though we had not
        // wrapped the connector in a tether at all
        let mut stream = Tether::connect_tcp(addr, Value(false)).await.unwrap();
        let mut output = String::new();
        stream.read_to_string(&mut output).await.unwrap();
        assert_eq!(&output, "foobar");
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
        stream.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf.as_slice(), &[0, 1])
    }

    #[tokio::test]
    async fn error_is_consumed_when_set() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _addr) = listener.accept().await.unwrap();
            stream.write_all(b"foobar").await.unwrap();
            stream.shutdown().await.unwrap();
        });

        // The Once resolver will attempt to reconnect one time after the socket has been closed.
        // That attempt will produce a connection refused error which without
        // `propegate_error_to_callsite_when_not_reconnecting: false` would be returned to the
        // read_to_end callsite. But with this value set, read_to_end completes successfully
        let mut stream = Tether::connect_tcp(addr, Once).await.unwrap();
        stream.set_config(Config {
            error_propagation_on_no_retry: config::ErrorPropagation::IoOperations,
            ..Default::default()
        });
        let mut buf = Vec::new();

        stream.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"foobar".as_slice())
    }

    #[tokio::test]
    async fn write_data_is_silently_dropped_when_set() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 3];

            let (mut stream, _addr) = listener.accept().await.unwrap();
            stream.read_exact(&mut buf[..]).await.unwrap();
            stream.shutdown().await.unwrap();

            buf
        });

        let mut stream = Tether::connect_tcp(addr, Value(false)).await.unwrap();
        stream.set_config(Config {
            keep_data_on_failed_write: false,
            ..Default::default()
        });

        stream.write_all(b"foo").await.unwrap();

        let buf = handle.await.unwrap();

        // This call succeeds due to TCP shutdown only closing the read half of the socket. This
        // call will trigger a TCP RST packet from the remote, which will cause future writes to
        // fail
        stream.write_all(b"bar").await.unwrap();

        // Give the kernel some time to flush the buffer and receive RST
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // This calls only succeeds due to `keep_data_on_failed_write` being set to false
        stream.write_all(b"baz").await.unwrap();

        assert_eq!(b"foo".as_slice(), buf)
    }
}
