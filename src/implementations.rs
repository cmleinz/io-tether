use std::{future::Future, pin::Pin, task::Poll};

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
        let mut this = self.project();

        loop {
            match this.idk.futs {
                FutState::Connected => {
                    let result = {
                        let depth = buf.filled().len();
                        let inner = Pin::new(&mut this.inner);
                        let result = ready!(inner.poll_read(cx, buf));
                        let read_bytes = buf.filled().len().saturating_sub(depth);
                        result.map(|_| read_bytes)
                    };

                    match result {
                        Ok(0) => {
                            unsafe { this.idk.to_disconnected(State::Eof) };
                        }
                        Ok(_) => return Poll::Ready(Ok(())),
                        Err(error) => {
                            unsafe { this.idk.to_disconnected(State::Err(error)) };
                        }
                    }
                }
                FutState::Disconnected(ref mut fut) => {
                    let retry = ready!(fut.as_mut().poll(cx));

                    if retry {
                        let init = this.initializer.clone();
                        let reconnect_fut = Box::pin(T::reconnect(init));
                        this.idk.futs = FutState::Reconnecting(reconnect_fut);
                    } else {
                        let err = std::io::Error::from(&this.idk.state);
                        return Poll::Ready(Err(err));
                    }
                }
                FutState::Reconnecting(ref mut fut) => {
                    let result = ready!(fut.as_mut().poll(cx));

                    match result {
                        Ok(new_thing) => {
                            *this.inner = new_thing;
                            unsafe { this.idk.to_reconnected() };
                        }
                        Err(error) => {
                            this.idk.context.reconnection_attempts += 1;
                            unsafe { this.idk.to_disconnected(State::Err(error)) };
                        }
                    }
                }
                FutState::Reconnected(ref mut fut) => {
                    ready!(fut.as_mut().poll(cx));
                    this.idk.futs = FutState::Connected;
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
