//! Tether implementations for Unix sockets
use super::*;

use std::path::Path;

use tokio::net::UnixStream;

impl<I, R> Tether<I, UnixConnector, R>
where
    R: Resolver,
    I: 'static + AsRef<Path> + Clone + Send + Sync,
{
    /// Initialize a Unix socket connection
    pub async fn connect_unix(initializer: I, resolver: R) -> Result<Self, std::io::Error> {
        let mut connector = UnixConnector;
        let io = connector.connect(initializer.clone()).await?;
        Ok(Tether::new(connector, io, initializer, resolver))
    }
}

/// Used to construct [`UnixStream`]s
pub struct UnixConnector;

impl<T> Io<T> for UnixConnector
where
    T: 'static + AsRef<Path> + Clone + Send + Sync,
{
    type Output = UnixStream;

    fn connect(&mut self, initializer: T) -> PinFut<Result<Self::Output, std::io::Error>> {
        Box::pin(UnixStream::connect(initializer))
    }
}
