# io-tether

<p align="center">
  <img src="https://cdn.akamai.steamstatic.com/apps/dota2/images/dota_react/abilities/wisp_tether.png" />
</p>

[![Crates.io](https://img.shields.io/crates/v/io-tether.svg)](https://crates.io/crates/io-tether)
[![Documentation](https://docs.rs/io-tether/badge.svg)](https://docs.rs/io-tether/)

Traits for defining I/O objects which automatically reconnect upon failure.

This project is similar in scope to
[stubborn-io](https://github.com/craftytrickster/stubborn-io), but provides
a trait-based solution aimed at allowing the user to define async functions
for various errors.

## Usage

To get started, add `io-tether` to your list of dependencies

```toml
io-tether = { version = "0.2.0" }
```

Then in most cases, it is expected that the consumer of this library will want
to implement `Resolver` on their own types. This allows them to inject
arbitrary asynchronous code just before the I/O attempts to reconnect.

```rust
use std::time::Duration;
use io_tether::{Resolver, Context, Reason, Tether, PinFut};
use tokio::{net::TcpStream, io::{AsyncReadExt, AsyncWriteExt}, sync::mpsc};

/// Custom resolver
pub struct CallbackResolver {
    channel: mpsc::Sender<()>,
}

impl Resolver for CallbackResolver {
    fn disconnected(
        &mut self,
        context: &Context,
        state: &Reason,
    ) -> PinFut<bool> {

        let sender = self.channel.clone();
	    Box::pin(async move {
	        tokio::time::sleep(Duration::from_millis(500)).await;
	        sender.send(()).await.unwrap();
	        true
		})
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (channel, mut rx) = mpsc::channel(10);

    let listener = tokio::net::TcpListener::bind("localhost:8080").await?;
    tokio::spawn(async move {
        loop {
            let (mut stream, _addr) = listener.accept().await.unwrap();
            stream.write_all(b"foobar").await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });

    let resolver = CallbackResolver {
        channel,
    };

	let handle = tokio::spawn(async move {
        let mut tether = Tether::<_, TcpStream, _>::connect(String::from("localhost:8080"), resolver)
            .await
			.unwrap();

		let mut buf = [0; 12];
        tether.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"foobarfoobar");
	});
    
	assert!(rx.recv().await.is_some());
	handle.await?;

    Ok(())
}
```

## Stability

This project is still very much a work in progress. Expect fairly common 
breaking changes in the short term. 

