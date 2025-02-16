use super::*;

use tokio::net::{TcpStream, ToSocketAddrs};

pub struct TcpConnector;

impl<I, R> Tether<I, TcpConnector, R>
where
    R: Resolver,
    I: 'static + ToSocketAddrs + Clone + Send + Sync,
{
    pub async fn connect_tcp(initializer: I, resolver: R) -> Result<Self, std::io::Error> {
        let mut connector = TcpConnector;
        let io = connector.connect(initializer.clone()).await?;
        Ok(Tether::new(connector, io, initializer, resolver))
    }
}

impl<T> Io<T> for TcpConnector
where
    T: 'static + ToSocketAddrs + Clone + Send + Sync,
{
    type Output = TcpStream;

    fn connect(&mut self, initializer: T) -> PinFut<Result<Self::Output, std::io::Error>> {
        Box::pin(TcpStream::connect(initializer))
    }
}
