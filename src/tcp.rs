use super::*;

use tokio::net::{TcpStream, ToSocketAddrs};

pub struct TcpConnector<A>(A);

impl<A, R> Tether<TcpConnector<A>, R>
where
    R: Resolver,
    A: 'static + ToSocketAddrs + Clone + Send + Sync,
{
    pub async fn connect_tcp(address: A, resolver: R) -> Result<Self, std::io::Error> {
        let mut connector = TcpConnector(address);
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
