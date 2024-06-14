# io-tether

Traits for defining I/O objects which automatically reconnect upon failure.

This project is similar in scope to [stubborn-io](https://github.com/craftytrickster/stubborn-io),
but aims to leverage the recently stabilized `async fn` in traits, to make the
implementation of reconnecting simpler for the end user.

## Usage

In most cases, it is expected that the consumer of this library will want to
implement their own `TetherResolver` types. This allows them to inject arbitrary
asynchronous code just before the I/O attempts to reconnect.

```rust
use tokio::{net::TcpStream, io::AsyncReadExt, sync::mpsc};

/// Custom resolver
pub struct CallbackResolver {
	channel: mpsc::Sender<String>,
}

impl TetherResolver for CallbackResolver {
	type Error = std::io::Error;

    async fn disconnected(
        &mut self,
        context: &Context,
        state: &State<Self::Error>,
    ) -> bool {
		match state {
			State::Eof => false,
			State::Err(error) => {
				let error = error.to_string();
				self.channel.send(error).await.unwrap();
				true	
			}
		}
	}
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	let mut buf = Vec::new();
	let (channel, rx) = mpsc::channel(10);

	let resolver = CallbackResolver {
		channel,
	};
	let mut tether = Tether::<TcpStream>::connect("localhost:8080", resolver)
		.await?;

	tether.read_buf(&mut buf).await?;
	Ok(())
}

```

## State

This project is still very much a work in progress, expect fairly common 
breaking changes in the short term. 

