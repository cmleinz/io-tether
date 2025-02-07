# io-tether

<p align="center">
  <img src="https://cdn.akamai.steamstatic.com/apps/dota2/images/dota_react/abilities/wisp_tether.png" />
</p>


Traits for defining I/O objects which automatically reconnect upon failure.

[Crates.io](https://crates.io/crates/io-tether) |
[API Docs](https://docs.rs/io-tether/latest/io_tether/) 

This project is similar in scope to
[stubborn-io](https://github.com/craftytrickster/stubborn-io), but aims to
leverage the recently stabilized `async fn` in traits, to make the
implementation of reconnecting simpler for the end user.

## Usage

To get started, add `io-tether` to your list of dependencies

```toml
io-tether = { version = "0.1.3" }
```

Then in most cases, it is expected that the consumer of this library will want
to implement `TetherResolver` on their own types. This allows them to inject
arbitrary asynchronous code just before the I/O attempts to reconnect.

```rust
use std::time::Duration;
use io_tether::{TetherResolver, Context, State, Tether, PinFut};
use tokio::{net::TcpStream, io::{AsyncReadExt, AsyncWriteExt}, sync::mpsc};

/// Custom resolver
pub struct CallbackResolver {
    channel: mpsc::Sender<()>,
}

impl TetherResolver for CallbackResolver {
    fn disconnected(
        &mut self,
        context: &Context,
        state: &State,
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

