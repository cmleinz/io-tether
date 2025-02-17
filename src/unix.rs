use super::*;

use std::path::Path;

use tokio::net::UnixStream;

pub struct UnixConnector<P>(P);

impl<P, R> Tether<UnixConnector<P>, R>
where
    R: Resolver,
    P: AsRef<Path>,
{
    pub async fn connect_unix(path: P, resolver: R) -> Result<Self, std::io::Error> {
        let mut connector = UnixConnector(path);
        let io = connector.connect().await?;
        Ok(Tether::new(connector, io, resolver))
    }
}

impl<P> Io for UnixConnector<P>
where
    P: AsRef<Path>,
{
    type Output = UnixStream;

    fn connect(&mut self) -> PinFut<Result<Self::Output, std::io::Error>> {
        let path = self.0.as_ref().to_path_buf();
        Box::pin(UnixStream::connect(path))
    }
}
