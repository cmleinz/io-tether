//! Tether implementations for Files
use std::path::Path;

use super::*;

use tokio::fs::{File, OpenOptions};

/// Wrapper for building [`File`]s
///
/// Convenience functions exist for the same constructors as exist in the standard library, but for
/// more complicated file options this can be built manually
pub struct FileConnector<P> {
    pub path: P,
    pub options: OpenOptions,
}

impl<P> FileConnector<P> {
    /// Construct a FileConnector with the same options as [`std::fs::File::open`]
    pub fn open(path: P) -> Self {
        let mut options = OpenOptions::new();
        options.read(true);

        Self { path, options }
    }

    /// Construct a FileConnector with the same options as [`std::fs::File::create`]
    pub fn create(path: P) -> Self {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);

        Self { path, options }
    }

    /// Construct a FileConnector with the same options as [`std::fs::File::create_new`]
    pub fn create_new(path: P) -> Self {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);

        Self { path, options }
    }
}

impl<P, R> Tether<FileConnector<P>, R>
where
    R: Resolver<FileConnector<P>>,
    P: AsRef<Path>,
{
    /// Helper function for building a Unix socket connection
    pub async fn connect_file(
        mut connector: FileConnector<P>,
        resolver: R,
    ) -> Result<Self, std::io::Error> {
        let io = connector.connect().await?;
        Ok(Tether::new(connector, io, resolver))
    }
}

impl<P> Io for FileConnector<P>
where
    P: AsRef<Path>,
{
    type Output = File;

    fn connect(&mut self) -> PinFut<Result<Self::Output, std::io::Error>> {
        let path = self.path.as_ref().to_path_buf();
        let options = self.options.clone();

        Box::pin(async move { options.open(path).await })
    }
}
