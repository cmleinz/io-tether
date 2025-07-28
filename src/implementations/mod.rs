use crate::Reason;

use super::{Connector, Resolver, Tether};

#[cfg(feature = "sink")]
mod sink;
#[cfg(feature = "stream")]
mod stream;
mod tokio;

/// I want to avoid implementing From<Reason> for Result<T, std::io::Error>, because it's not a
/// generally applicable transformation. In the specific case of AsyncRead and AsyncWrite, we can
/// map to those, but there's no guarantee that Ok(0) always implies Eof for some arbitrary
/// Result<usize, std::io::Error>
trait IoInto<T>: Sized {
    fn io_into(self) -> T;
}

impl IoInto<Result<usize, std::io::Error>> for Reason {
    fn io_into(self) -> Result<usize, std::io::Error> {
        match self {
            Reason::Eof => Ok(0),
            Reason::Err(error) => Err(error),
        }
    }
}

impl IoInto<Result<(), std::io::Error>> for Reason {
    fn io_into(self) -> Result<(), std::io::Error> {
        match self {
            Reason::Eof => Ok(()),
            Reason::Err(error) => Err(error),
        }
    }
}

pub(crate) mod connected {
    macro_rules! connected {
        ($me:expr, $poll_method:ident, $cx:expr, $default:expr, $($args:expr),*) => {
            loop {
                match $me.state {
                    State::Connected => {
                        let cont = ready!($me.inner.$poll_method(&mut $me.state, $cx, $($args),*));

                        if let ControlFlow::Break(val) = cont {
                            return Poll::Ready(val);
                        }
                    }
                    State::Disconnected(ref mut fut) => {
                        let retry = ready!(fut.as_mut().poll($cx));

                        match retry {
                            crate::Action::AttemptReconnect => {
                                $me.inner.set_reconnecting(&mut $me.state);
                                continue;
                            }
                            crate::Action::Exhaust => {
                                let opt_reason = $me.inner.context.reason.take();
                                let (reason, source) = opt_reason.expect("Can only enter Disconnected state with Reason");
                                $me.inner.set_disconnected(&mut $me.state, reason.clone_private(), source);

                                let reason = match ($me.inner.config.error_propagation_on_no_retry, source) {
                                    (crate::config::ErrorPropagation::IoOperations, Source::Io) => reason.io_into(),
                                    (crate::config::ErrorPropagation::All, _) => reason.io_into(),
                                    _ => $default,
                                };

                                return Poll::Ready(reason);
                            }
                            crate::Action::Ignore => {
                                $me.inner.set_connected(&mut $me.state);
                                continue;
                            }
                        }

                    }
                    State::Reconnecting(ref mut fut) => {
                        let result = ready!(fut.as_mut().poll($cx));
                        $me.inner.context.increment_attempts();

                        match result {
                            Ok(new_io) => {
                                $me.inner.set_reconnected(&mut $me.state, new_io);
                            }
                            Err(error) => {
                                $me.inner.set_disconnected(&mut $me.state, Reason::Err(error), Source::Reconnect);
                            },
                        }
                    }
                    State::Reconnected(ref mut fut) => {
                        ready!(fut.as_mut().poll($cx));
                        $me.inner.set_connected(&mut $me.state);
                    }
                }
            }
        };
    }

    pub(crate) use connected;
}
