//! Tether implementations for TCP sockets
use super::*;

use tokio::net::{TcpStream, ToSocketAddrs};

/// Wrapper for building [`TcpStream`]s
pub struct TcpConnector<A>(A);

impl<A> TcpConnector<A> {
    pub fn new(address: A) -> Self {
        Self(address)
    }

    pub fn get_addr(&self) -> &A {
        &self.0
    }

    pub fn get_addr_mut(&mut self) -> &mut A {
        &mut self.0
    }
}

impl<A, R> Tether<TcpConnector<A>, R>
where
    R: Resolver,
    A: 'static + ToSocketAddrs + Clone + Send + Sync,
{
    /// Helper function for building a TCP connection
    pub async fn connect_tcp(address: A, resolver: R) -> Result<Self, std::io::Error> {
        let mut connector = TcpConnector::new(address);
        let io = connector.connect().await?;
        Ok(Tether::new(connector, io, resolver))
    }
}

impl<A> Io for TcpConnector<A>
where
    A: 'static + ToSocketAddrs + Clone + Send + Sync,
{
    type Output = TcpStream;

    fn connect(&mut self) -> PinFut<Result<Self::Output, std::io::Error>> {
        let address = self.0.clone();
        Box::pin(TcpStream::connect(address))
    }
}
