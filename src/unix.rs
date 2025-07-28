//! Tether implementations for Unix sockets
use super::*;

use std::path::Path;

use tokio::net::UnixStream;

/// Wrapper for building [`UnixStream`]s
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnixConnector<P>(P);

impl<P> UnixConnector<P> {
    pub fn new(path: P) -> Self {
        Self(path)
    }

    pub fn get_path(&self) -> &P {
        &self.0
    }

    pub fn get_path_mut(&mut self) -> &mut P {
        &mut self.0
    }
}

impl<P, R> Tether<UnixConnector<P>, R>
where
    R: Resolver<UnixConnector<P>>,
    P: AsRef<Path>,
{
    /// Helper function for building a Unix socket connection
    pub async fn connect_unix(path: P, resolver: R) -> Result<Self, std::io::Error> {
        let connector = UnixConnector::new(path);
        Tether::connect(connector, resolver).await
    }
}

impl<P> Connector for UnixConnector<P>
where
    P: AsRef<Path>,
{
    type Output = UnixStream;

    fn connect(&mut self) -> PinFut<Result<Self::Output, std::io::Error>> {
        let path = self.0.as_ref().to_path_buf();
        Box::pin(UnixStream::connect(path))
    }
}
