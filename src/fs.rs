//! Tether implementations for File descriptors
use std::path::{Path, PathBuf};

use super::*;

use tokio::fs::{File, OpenOptions};

/// Used to construct [`File`]s
pub struct FileConnector;

/// A trait to abstract over file creation
pub trait FileInitializer {
    fn options(&self) -> OpenOptions;

    fn path(&self) -> impl AsRef<Path>;
}

#[derive(Debug, Clone)]
struct BasicInitializer {
    pub path: PathBuf,
    pub options: OpenOptions,
}

impl FileInitializer for BasicInitializer {
    fn options(&self) -> OpenOptions {
        self.options.clone()
    }

    fn path(&self) -> impl AsRef<Path> {
        &self.path
    }
}

impl<R> Tether<BasicInitializer, FileConnector, R>
where
    R: Resolver,
{
    /// Initialize a file connection
    pub async fn connect_file(
        path: impl AsRef<Path>,
        options: OpenOptions,
        resolver: R,
    ) -> Result<Self, std::io::Error> {
        let mut connector = FileConnector;
        let initializer = BasicInitializer {
            path: path.as_ref().to_path_buf(),
            options,
        };
        let io = connector.connect(initializer.clone()).await?;
        Ok(Tether::new(connector, io, initializer, resolver))
    }
}

impl<T> Io<T> for FileConnector
where
    T: 'static + FileInitializer + Clone + Send + Sync,
{
    type Output = File;

    fn connect(&mut self, initializer: T) -> PinFut<Result<Self::Output, std::io::Error>> {
        let path = initializer.path().as_ref().to_path_buf();
        Box::pin(async move {
            let opts = initializer.options();
            opts.open(path).await
        })
    }
}
