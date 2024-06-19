use std::{pin::Pin, task::Poll};

use tokio::io::AsyncRead;

use crate::{FutState, State};

use super::{ready::ready, Tether, TetherIo, TetherResolver};

impl<I, T, R> AsyncRead for Tether<I, T, R>
where
    T: AsyncRead + TetherIo<I>,
    I: Unpin + Clone,
    R: 'static + TetherResolver,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut me = self.as_mut();

        loop {
            match me.futs {
                FutState::Connected => {
                    let result = {
                        let depth = buf.filled().len();
                        let inner_pin = std::pin::pin!(&mut me.inner);
                        let result = ready!(inner_pin.poll_read(cx, buf));
                        let read_bytes = buf.filled().len().saturating_sub(depth);
                        result.map(|_| read_bytes)
                    };

                    match result {
                        Ok(0) => {
                            *me.state.borrow_mut() = State::Eof;
                            let fut = me.disconnected_fut();
                            let fut = fut.into_pin();
                            me.futs = FutState::Disconnected(fut);
                        }
                        Ok(_) => return Poll::Ready(Ok(())),
                        Err(error) => {
                            *me.state.borrow_mut() = State::Err(error);
                            let fut = me.disconnected_fut();
                            let fut = fut.into_pin();
                            me.futs = FutState::Disconnected(fut);
                        }
                    }
                }
                FutState::Disconnected(ref mut fut) => {
                    let retry = ready!(fut.as_mut().poll(cx));

                    if retry {
                        let init = me.initializer.clone();
                        let reconnect_fut = Box::pin(T::reconnect(init));
                        me.futs = FutState::Reconnecting(reconnect_fut);
                    } else {
                        let err = &*me.state.borrow();
                        let err = err.into();
                        return Poll::Ready(Err(err));
                    }
                }
                FutState::Reconnecting(ref mut fut) => {
                    let result = ready!(fut.as_mut().poll(cx));
                    me.context.borrow_mut().reconnection_attempts += 1;

                    match result {
                        Ok(new_thing) => {
                            me.inner = new_thing;
                            let fut = me.reconnected_fut();
                            let fut = fut.into_pin();
                            me.futs = FutState::Reconnected(fut);
                        }
                        Err(error) => *me.state.borrow_mut() = State::Err(error),
                    }
                }
                FutState::Reconnected(ref mut fut) => {
                    ready!(fut.as_mut().poll(cx));
                    me.futs = FutState::Connected;
                }
            }
        }
    }
}

#[cfg(feature = "net")]
mod net {
    use super::*;
    mod tcp {
        use super::*;

        use tokio::net::{TcpStream, ToSocketAddrs};

        impl<I, R> Tether<I, TcpStream, R>
        where
            R: TetherResolver,
            I: 'static + ToSocketAddrs + Clone + Send + Sync,
        {
            pub async fn connect_tcp(initializer: I, resolver: R) -> Result<Self, std::io::Error> {
                Self::connect(initializer, resolver).await
            }
        }

        impl<T> TetherIo<T> for TcpStream
        where
            T: 'static + ToSocketAddrs + Clone + Send + Sync,
        {
            async fn connect(initializer: T) -> Result<Self, std::io::Error> {
                let addr = initializer.clone();
                TcpStream::connect(addr).await
            }
        }
    }

    #[cfg(target_family = "unix")]
    mod unix {
        use super::*;

        use std::path::Path;

        use tokio::net::UnixStream;

        impl<I, R> Tether<I, UnixStream, R>
        where
            R: TetherResolver,
            I: 'static + AsRef<Path> + Clone + Send + Sync,
        {
            pub async fn connect_unix(initializer: I, resolver: R) -> Result<Self, std::io::Error> {
                Self::connect(initializer, resolver).await
            }
        }

        impl<T> TetherIo<T> for UnixStream
        where
            T: 'static + AsRef<Path> + Clone + Send + Sync,
        {
            async fn connect(initializer: T) -> Result<Self, std::io::Error> {
                UnixStream::connect(initializer).await
            }
        }
    }
}
