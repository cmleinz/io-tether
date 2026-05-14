use std::collections::VecDeque;

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
};
use tokio_test::io::{Builder, Mock};

use super::*;

struct Value(Action);

impl<T> Resolver<T> for Value {
    fn disconnected(&mut self, _context: &Context, _connector: &mut T) -> PinFut<Action> {
        let val = self.0;
        Box::pin(async move { val })
    }
}

struct Once;

impl<T> Resolver<T> for Once {
    fn disconnected(&mut self, context: &Context, _connector: &mut T) -> PinFut<Action> {
        let retry = if context.total_reconnect_attempts() < 1 {
            Action::AttemptReconnect
        } else {
            Action::Exhaust
        };

        Box::pin(async move { retry })
    }
}

fn other(err: &'static str) -> std::io::Error {
    std::io::Error::other(err)
}

trait ReadWrite: 'static + AsyncRead + AsyncWrite + Unpin {}
impl<T: 'static + AsyncRead + AsyncWrite + Unpin> ReadWrite for T {}

struct MockConnector<F>(F);

impl<F: FnMut() -> Mock> Connector for MockConnector<F> {
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
    let tether_mock = Tether::connect(MockConnector(gener), Value(Action::Exhaust))
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
    tether.write_all(b"bar").await.unwrap();
}

#[tokio::test]
async fn failure_to_connect_doesnt_panic() {
    struct Unreachable;
    impl<T> Resolver<T> for Unreachable {
        fn disconnected(&mut self, context: &Context, _connector: &mut T) -> PinFut<Action> {
            let _reason = context.reason(); // This should not panic
            Box::pin(async move { Action::Exhaust })
        }
    }

    let result = Tether::connect_tcp("0.0.0.0:3150", Unreachable).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn read_then_disconnect() {
    struct AllowEof;
    impl<T> Resolver<T> for AllowEof {
        fn disconnected(&mut self, context: &Context, _connector: &mut T) -> PinFut<Action> {
            // Don't reconnect on EoF
            let value = if !matches!(context.reason(), Reason::Eof) {
                Action::AttemptReconnect
            } else {
                Action::Exhaust
            };
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
        let (mut stream, _addr) = listener.accept().await.unwrap();
        stream.write_all(b"foobar").await.unwrap();
        stream.shutdown().await.unwrap();
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
    let mut stream = Tether::connect_tcp(addr, Value(Action::Exhaust))
        .await
        .unwrap();
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

    let mut stream = Tether::connect_tcp(addr, Value(Action::Exhaust))
        .await
        .unwrap();
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

// After exhaust on EOF, subsequent reads return EOF (not an error).
#[tokio::test]
async fn exhausted_eof_returns_eof_on_subsequent_read() {
    let mock = || Builder::new().read(b"").build();
    let mut tether = Tether::connect(MockConnector(mock), Value(Action::Exhaust))
        .await
        .unwrap();

    let mut buf = Vec::new();
    tether.read_to_end(&mut buf).await.unwrap();
    assert!(buf.is_empty());

    let mut buf2 = Vec::new();
    tether.read_to_end(&mut buf2).await.unwrap();
    assert!(buf2.is_empty());
}

// After exhaust on an IO error with ErrorPropagation::All, subsequent reads return the same
// error kind — not a silent EOF.
#[tokio::test]
async fn exhausted_error_returns_same_error_kind_on_subsequent_read() {
    let mock = || {
        Builder::new()
            .read_error(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            .build()
    };
    let mut tether = Tether::connect(MockConnector(mock), Value(Action::Exhaust))
        .await
        .unwrap();
    tether.set_config(Config {
        error_propagation_on_no_retry: config::ErrorPropagation::All,
        ..Default::default()
    });

    let mut buf = Vec::new();
    let first = tether.read_to_end(&mut buf).await;
    assert_eq!(first.unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);

    let mut buf2 = Vec::new();
    let second = tether.read_to_end(&mut buf2).await;
    assert_eq!(second.unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
}

// `disconnected` must be called exactly once per disconnect event. Previously the macro called
// `set_disconnected` a second time inside the Exhaust branch, causing a spurious second call.
#[tokio::test]
async fn disconnected_called_exactly_once_on_exhaust() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct CountingResolver(Arc<AtomicUsize>);

    impl<T> Resolver<T> for CountingResolver {
        fn disconnected(&mut self, _context: &Context, _: &mut T) -> PinFut<Action> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Action::Exhaust })
        }
    }

    let count = Arc::new(AtomicUsize::new(0));
    let mock = || Builder::new().read(b"").build();
    let mut tether = Tether::connect(MockConnector(mock), CountingResolver(count.clone()))
        .await
        .unwrap();

    let mut buf = Vec::new();
    tether.read_to_end(&mut buf).await.unwrap();

    // Poll again to confirm Exhausted state does not re-invoke the resolver.
    let mut buf2 = Vec::new();
    tether.read_to_end(&mut buf2).await.unwrap();

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

// ===== FallibleMockConnector: connect can succeed or return error =====

struct FallibleMockConnector(VecDeque<Result<Mock, std::io::Error>>);

impl Connector for FallibleMockConnector {
    type Output = Mock;

    fn connect(&mut self) -> PinFut<Result<Mock, std::io::Error>> {
        let result = self
            .0
            .pop_front()
            .unwrap_or_else(|| Err(other("exhausted")));
        Box::pin(async move { result })
    }
}

// ===== Reason =====

#[test]
fn reason_retryable_eof() {
    assert!(Reason::Eof.retryable());
}

#[test]
fn reason_retryable_broken_pipe() {
    assert!(Reason::Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)).retryable());
}

#[test]
fn reason_retryable_connection_reset() {
    assert!(Reason::Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset)).retryable());
}

#[test]
fn reason_retryable_timed_out() {
    assert!(Reason::Err(std::io::Error::from(std::io::ErrorKind::TimedOut)).retryable());
}

#[test]
fn reason_retryable_connection_refused() {
    assert!(Reason::Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)).retryable());
}

#[test]
fn reason_not_retryable_other() {
    assert!(!Reason::Err(std::io::Error::other("mystery")).retryable());
}

#[test]
fn reason_not_retryable_invalid_input() {
    assert!(!Reason::Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)).retryable());
}

#[test]
fn reason_not_retryable_would_block() {
    assert!(!Reason::Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)).retryable());
}

#[test]
fn reason_display_eof() {
    assert_eq!(Reason::Eof.to_string(), "End of file detected");
}

#[test]
fn reason_display_wraps_inner_error() {
    assert!(
        Reason::Err(std::io::Error::other("badness"))
            .to_string()
            .contains("badness")
    );
}

#[test]
fn reason_into_io_error_eof_gives_unexpected_eof() {
    let err: std::io::Error = Reason::Eof.into();
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn reason_into_io_error_preserves_kind() {
    let err: std::io::Error =
        Reason::Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)).into();
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
}

// ===== Context =====

#[test]
fn context_try_reason_none_by_default() {
    assert!(Context::default().try_reason().is_none());
}

// current_reconnect_attempts resets to 0 after each successful reconnect;
// total_reconnect_attempts keeps growing.
#[tokio::test]
async fn context_current_attempts_reset_after_reconnect() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    let recorded: Arc<Mutex<Vec<(usize, usize)>>> = Arc::new(Mutex::new(Vec::new()));

    struct RecordAttempts(Arc<Mutex<Vec<(usize, usize)>>>, Arc<AtomicUsize>);
    impl<T> Resolver<T> for RecordAttempts {
        fn disconnected(&mut self, context: &Context, _: &mut T) -> PinFut<Action> {
            self.0.lock().unwrap().push((
                context.current_reconnect_attempts(),
                context.total_reconnect_attempts(),
            ));
            let n = self.1.fetch_add(1, Ordering::SeqCst);
            let action = if n < 2 {
                Action::AttemptReconnect
            } else {
                Action::Exhaust
            };
            Box::pin(async move { action })
        }
    }

    // mock1 → io error, mock2 → io error, mock3 → EOF
    let mut i = 0usize;
    let mock = move || {
        i += 1;
        match i {
            1 | 2 => Builder::new().read_error(other("disc")).build(),
            _ => Builder::new().read(b"").build(),
        }
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let mut tether = Tether::connect(MockConnector(mock), RecordAttempts(recorded.clone(), calls))
        .await
        .unwrap();
    let mut buf = Vec::new();
    tether.read_to_end(&mut buf).await.unwrap();

    let r = recorded.lock().unwrap();
    // Each disconnect: current resets to 0 after every successful reconnect
    assert_eq!(r[0], (0, 0));
    assert_eq!(r[1], (0, 1));
    assert_eq!(r[2], (0, 2));
}

// current_reconnect_attempts increments when reconnect attempts themselves fail.
#[tokio::test]
async fn context_current_attempts_increment_on_failed_reconnect() {
    use std::sync::{Arc, Mutex};

    let recorded: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));

    struct RecordCurrent(Arc<Mutex<Vec<usize>>>);
    impl Resolver<FallibleMockConnector> for RecordCurrent {
        fn disconnected(
            &mut self,
            context: &Context,
            _: &mut FallibleMockConnector,
        ) -> PinFut<Action> {
            let cur = context.current_reconnect_attempts();
            self.0.lock().unwrap().push(cur);
            let len = self.0.lock().unwrap().len();
            let action = if len < 3 {
                Action::AttemptReconnect
            } else {
                Action::Exhaust
            };
            Box::pin(async move { action })
        }
    }

    let mocks = FallibleMockConnector(VecDeque::from([
        Ok(Builder::new().read_error(other("io error")).build()),
        Err(other("reconnect fail 1")),
        Err(other("reconnect fail 2")),
    ]));

    let mut tether = Tether::connect(mocks, RecordCurrent(recorded.clone()))
        .await
        .unwrap();
    let mut buf = Vec::new();
    tether.read_to_end(&mut buf).await.unwrap();

    let r = recorded.lock().unwrap();
    assert_eq!(r[0], 0); // first disconnect
    assert_eq!(r[1], 1); // after one failed reconnect attempt
    assert_eq!(r[2], 2); // after two failed reconnect attempts
}

// ===== Action::Ignore =====

struct IgnoreOnce(bool);
impl<T> Resolver<T> for IgnoreOnce {
    fn disconnected(&mut self, _: &Context, _: &mut T) -> PinFut<Action> {
        let a = if !self.0 {
            self.0 = true;
            Action::Ignore
        } else {
            Action::Exhaust
        };
        Box::pin(async move { a })
    }
}

// Ignore keeps the same IO instance; subsequent poll sees the next item in sequence.
#[tokio::test]
async fn ignore_preserves_io_and_continues() {
    let mock = || {
        Builder::new()
            .read(b"foo")
            .read_error(other("transient"))
            .read(b"bar")
            .read(b"")
            .build()
    };
    let mut tether = Tether::connect(MockConnector(mock), IgnoreOnce(false))
        .await
        .unwrap();
    let mut buf = String::new();
    tether.read_to_string(&mut buf).await.unwrap();
    assert_eq!(buf, "foobar");
}

// ===== Resolver callbacks =====

#[tokio::test]
async fn established_called_once_on_connect() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let count = Arc::new(AtomicUsize::new(0));

    struct TrackEstablished(Arc<AtomicUsize>);
    impl<T> Resolver<T> for TrackEstablished {
        fn disconnected(&mut self, _: &Context, _: &mut T) -> PinFut<Action> {
            Box::pin(async { Action::Exhaust })
        }
        fn established(&mut self, _: &Context) -> PinFut<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    let mock = || Builder::new().build();
    let _tether = Tether::connect(MockConnector(mock), TrackEstablished(count.clone()))
        .await
        .unwrap();

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reconnected_called_after_each_successful_reconnect() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let count = Arc::new(AtomicUsize::new(0));

    struct TrackReconnected(Arc<AtomicUsize>);
    impl<T> Resolver<T> for TrackReconnected {
        fn disconnected(&mut self, context: &Context, _: &mut T) -> PinFut<Action> {
            let action = if context.total_reconnect_attempts() < 1 {
                Action::AttemptReconnect
            } else {
                Action::Exhaust
            };
            Box::pin(async move { action })
        }
        // Override established so it doesn't fall through to reconnected.
        fn established(&mut self, _: &Context) -> PinFut<()> {
            Box::pin(async {})
        }
        fn reconnected(&mut self, _: &Context) -> PinFut<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    let mut i = 0usize;
    let mock = move || {
        i += 1;
        if i == 1 {
            Builder::new().read_error(other("disc")).build()
        } else {
            Builder::new().read(b"").build()
        }
    };

    let mut tether = Tether::connect(MockConnector(mock), TrackReconnected(count.clone()))
        .await
        .unwrap();
    let mut buf = Vec::new();
    tether.read_to_end(&mut buf).await.unwrap();

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

// ===== Tether::connect_without_retry =====

#[tokio::test]
async fn connect_without_retry_fails_without_retrying() {
    struct NeverCalled;
    impl<T> Resolver<T> for NeverCalled {
        fn disconnected(&mut self, _: &Context, _: &mut T) -> PinFut<Action> {
            panic!("disconnected must not be called")
        }
        fn unreachable(&mut self, _: &Context, _: &mut T) -> PinFut<bool> {
            panic!("unreachable must not be called")
        }
    }

    use crate::tcp::TcpConnector;
    let connector = TcpConnector::new("0.0.0.0:39871");
    let result = Tether::connect_without_retry(connector, NeverCalled).await;
    assert!(result.is_err());
}

// ===== Tether::new + into_inner =====

#[tokio::test]
async fn into_inner_returns_underlying_io() {
    let mock = Builder::new().read(b"hello").build();
    let connector = MockConnector(|| Builder::new().build());
    let tether = Tether::new(connector, mock, Value(Action::Exhaust));
    let mut inner = tether.into_inner();

    let mut buf = Vec::new();
    inner.read_to_end(&mut buf).await.unwrap();
    assert_eq!(buf, b"hello");
}

// ===== ErrorPropagation::None =====

#[tokio::test]
async fn error_propagation_none_swallows_exhaust_error() {
    let mock = || {
        Builder::new()
            .read_error(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            .build()
    };
    let mut tether = Tether::connect(MockConnector(mock), Value(Action::Exhaust))
        .await
        .unwrap();
    tether.set_config(Config {
        error_propagation_on_no_retry: config::ErrorPropagation::None,
        ..Default::default()
    });

    let mut buf = Vec::new();
    tether.read_to_end(&mut buf).await.unwrap();
    assert!(buf.is_empty());
}

// ===== ErrorPropagation::All with Source::Reconnect =====

// When a reconnect attempt itself fails and ErrorPropagation::All is set,
// the reconnect error propagates. With IoOperations it would be swallowed.
#[tokio::test]
async fn error_propagation_all_returns_reconnect_failure_error() {
    let mocks = FallibleMockConnector(VecDeque::from([
        Ok(Builder::new().read_error(other("io error")).build()),
        Err(other("reconnect failed")),
    ]));

    let mut tether = Tether::connect(mocks, Once).await.unwrap();
    tether.set_config(Config {
        error_propagation_on_no_retry: config::ErrorPropagation::All,
        ..Default::default()
    });

    let mut buf = Vec::new();
    let result = tether.read_to_end(&mut buf).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("reconnect failed"));
}

#[tokio::test]
async fn error_propagation_io_operations_swallows_reconnect_failure() {
    let mocks = FallibleMockConnector(VecDeque::from([
        Ok(Builder::new().read_error(other("io error")).build()),
        Err(other("reconnect failed")),
    ]));

    let mut tether = Tether::connect(mocks, Once).await.unwrap();
    tether.set_config(Config {
        error_propagation_on_no_retry: config::ErrorPropagation::IoOperations,
        ..Default::default()
    });

    let mut buf = Vec::new();
    tether.read_to_end(&mut buf).await.unwrap();
    assert!(buf.is_empty());
}

// ===== Flush / Shutdown =====

#[tokio::test]
async fn flush_propagates_to_inner() {
    let mock = || Builder::new().build();
    let mut tether = Tether::connect(MockConnector(mock), Value(Action::Exhaust))
        .await
        .unwrap();
    tether.flush().await.unwrap();
}

#[tokio::test]
async fn shutdown_propagates_to_inner() {
    let mock = || Builder::new().build();
    let mut tether = Tether::connect(MockConnector(mock), Value(Action::Exhaust))
        .await
        .unwrap();
    tether.shutdown().await.unwrap();
}

// ===== Write error triggers reconnect and retries buffered data =====

#[tokio::test]
async fn write_error_triggers_reconnect_and_retries_data() {
    let mut i = 0usize;
    let mock = move || {
        i += 1;
        match i {
            1 => Builder::new().write_error(other("write failed")).build(),
            _ => Builder::new().write(b"hello").build(),
        }
    };

    let mut tether = Tether::connect(MockConnector(mock), Once).await.unwrap();
    tether.write_all(b"hello").await.unwrap();
}

// ===== Stream impl =====

#[cfg(feature = "stream")]
mod stream_tests {
    use std::future::poll_fn;
    use std::pin::Pin;
    use std::task::{Context as TaskContext, Poll};

    use futures_core::Stream;

    use super::*;

    struct VecStream(VecDeque<Result<u32, std::io::Error>>);

    impl Stream for VecStream {
        type Item = Result<u32, std::io::Error>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            _: &mut TaskContext<'_>,
        ) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.0.pop_front())
        }
    }

    struct VecStreamConnector(VecDeque<VecStream>);

    impl Connector for VecStreamConnector {
        type Output = VecStream;

        fn connect(&mut self) -> PinFut<Result<VecStream, std::io::Error>> {
            let s = self
                .0
                .pop_front()
                .unwrap_or_else(|| VecStream(VecDeque::new()));
            Box::pin(async move { Ok(s) })
        }
    }

    async fn stream_next<S: Stream + Unpin>(s: &mut S) -> Option<S::Item> {
        poll_fn(|cx| Pin::new(&mut *s).poll_next(cx)).await
    }

    #[tokio::test]
    async fn stream_yields_all_items_then_none() {
        let stream = VecStream(VecDeque::from([Ok(1u32), Ok(2), Ok(3)]));
        let mut tether = Tether::connect(
            VecStreamConnector(VecDeque::from([stream])),
            Value(Action::Exhaust),
        )
        .await
        .unwrap();

        assert_eq!(stream_next(&mut tether).await.unwrap().unwrap(), 1);
        assert_eq!(stream_next(&mut tether).await.unwrap().unwrap(), 2);
        assert_eq!(stream_next(&mut tether).await.unwrap().unwrap(), 3);
        assert!(stream_next(&mut tether).await.is_none());
    }

    #[tokio::test]
    async fn stream_reconnects_on_eof_and_continues() {
        let s1 = VecStream(VecDeque::from([Ok(1u32), Ok(2)]));
        let s2 = VecStream(VecDeque::from([Ok(3u32), Ok(4)]));
        let mut tether = Tether::connect(VecStreamConnector(VecDeque::from([s1, s2])), Once)
            .await
            .unwrap();

        let mut items = Vec::new();
        while let Some(item) = stream_next(&mut tether).await {
            items.push(item.unwrap());
        }
        assert_eq!(items, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn stream_error_returned_with_propagation_all() {
        let stream = VecStream(VecDeque::from([
            Ok(1u32),
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
        ]));
        let mut tether = Tether::connect(
            VecStreamConnector(VecDeque::from([stream])),
            Value(Action::Exhaust),
        )
        .await
        .unwrap();
        tether.set_config(Config {
            error_propagation_on_no_retry: config::ErrorPropagation::All,
            ..Default::default()
        });

        assert_eq!(stream_next(&mut tether).await.unwrap().unwrap(), 1);
        let err = stream_next(&mut tether).await.unwrap().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn stream_none_with_propagation_none() {
        let stream = VecStream(VecDeque::from([Err(std::io::Error::from(
            std::io::ErrorKind::BrokenPipe,
        ))]));
        let mut tether = Tether::connect(
            VecStreamConnector(VecDeque::from([stream])),
            Value(Action::Exhaust),
        )
        .await
        .unwrap();
        tether.set_config(Config {
            error_propagation_on_no_retry: config::ErrorPropagation::None,
            ..Default::default()
        });

        assert!(stream_next(&mut tether).await.is_none());
    }
}

// ===== Sink impl =====

#[cfg(feature = "sink")]
mod sink_tests {
    use std::future::poll_fn;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context as TaskContext, Poll};

    use futures_sink::Sink;

    use super::*;

    struct VecSink {
        received: Arc<Mutex<Vec<u32>>>,
        ready_errors: VecDeque<std::io::Error>,
    }

    impl VecSink {
        fn new(received: Arc<Mutex<Vec<u32>>>) -> Self {
            Self {
                received,
                ready_errors: VecDeque::new(),
            }
        }

        fn with_ready_error(received: Arc<Mutex<Vec<u32>>>, err: std::io::Error) -> Self {
            let mut s = Self::new(received);
            s.ready_errors.push_back(err);
            s
        }
    }

    impl Sink<u32> for VecSink {
        type Error = std::io::Error;

        fn poll_ready(
            mut self: Pin<&mut Self>,
            _: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            match self.ready_errors.pop_front() {
                Some(e) => Poll::Ready(Err(e)),
                None => Poll::Ready(Ok(())),
            }
        }

        fn start_send(self: Pin<&mut Self>, item: u32) -> Result<(), Self::Error> {
            self.get_mut().received.lock().unwrap().push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    struct VecSinkConnector(VecDeque<VecSink>);

    impl Connector for VecSinkConnector {
        type Output = VecSink;

        fn connect(&mut self) -> PinFut<Result<VecSink, std::io::Error>> {
            let sink = self.0.pop_front().unwrap();
            Box::pin(async move { Ok(sink) })
        }
    }

    async fn send_item(
        tether: &mut (impl Sink<u32, Error = std::io::Error> + Unpin),
        item: u32,
    ) -> std::io::Result<()> {
        poll_fn(|cx| Pin::new(&mut *tether).poll_ready(cx)).await?;
        Pin::new(&mut *tether).start_send(item)?;
        poll_fn(|cx| Pin::new(&mut *tether).poll_flush(cx)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn sink_sends_items_to_inner() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = VecSink::new(received.clone());

        let mut tether = Tether::connect(
            VecSinkConnector(VecDeque::from([sink])),
            Value(Action::Exhaust),
        )
        .await
        .unwrap();

        send_item(&mut tether, 1).await.unwrap();
        send_item(&mut tether, 2).await.unwrap();
        send_item(&mut tether, 3).await.unwrap();

        assert_eq!(*received.lock().unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn sink_reconnects_on_poll_ready_error() {
        let received1 = Arc::new(Mutex::new(Vec::<u32>::new()));
        let received2 = Arc::new(Mutex::new(Vec::new()));

        let sink1 = VecSink::with_ready_error(received1.clone(), other("sink error"));
        let sink2 = VecSink::new(received2.clone());

        let mut tether = Tether::connect(VecSinkConnector(VecDeque::from([sink1, sink2])), Once)
            .await
            .unwrap();

        // poll_ready fails on sink1 → reconnect to sink2 → item goes to sink2
        send_item(&mut tether, 42).await.unwrap();

        assert!(received1.lock().unwrap().is_empty());
        assert_eq!(*received2.lock().unwrap(), vec![42]);
    }

    #[tokio::test]
    async fn sink_close_works() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = VecSink::new(received.clone());

        let mut tether = Tether::connect(
            VecSinkConnector(VecDeque::from([sink])),
            Value(Action::Exhaust),
        )
        .await
        .unwrap();

        poll_fn(|cx| Pin::new(&mut tether).poll_close(cx))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn sink_flush_works() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = VecSink::new(received.clone());

        let mut tether = Tether::connect(
            VecSinkConnector(VecDeque::from([sink])),
            Value(Action::Exhaust),
        )
        .await
        .unwrap();

        send_item(&mut tether, 7).await.unwrap();
        poll_fn(|cx| Pin::new(&mut tether).poll_flush(cx))
            .await
            .unwrap();
        assert_eq!(*received.lock().unwrap(), vec![7]);
    }
}
