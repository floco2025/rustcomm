# RustComm

A lightweight, high-performance communication library built on
[mio](https://docs.rs/mio) with TCP and TLS transport support.

- **Dual-layer architecture:** Low-level `Transport` API for raw bytes, or
  higher-level `Messenger` API for type-safe serialized messages
- **Peer-to-peer:** No fixed server/client roles - any peer can listen, connect,
  send, and receive
- **Serialization-agnostic:** Bring your own serialization (bincode, serde,
  hand-written, etc.) - includes optional bincode helpers
- **Easily extensible:** Add custom transports beyond TCP/TLS, such as QUIC.
- **Flexible threading:** Works in single-threaded event loops or multi-threaded
  architectures like thread pools

## Quick Start

Add rustcomm to your `Cargo.toml`:

```toml
[dependencies]
rustcomm = "0.1"
```

### Option 1: Low-Level Transport (Raw Bytes)

Use the transport layer directly for streaming raw bytes:

```rust
use rustcomm::prelude::*;

// Create TCP transport
let mut transport = Transport::new_tcp()?;

// Start listening
let (listener_id, addr) = transport.listen("127.0.0.1:8080")?;

// Event loop
loop {
    for event in transport.fetch_events()? {
        match event {
            TransportEvent::Connected { id } => {
                println!("Client {} connected", id);
            }
            TransportEvent::Data { id, data } => {
                // Process raw bytes
                println!("Received {} bytes from {}", data.len(), id);
                // Echo back
                transport.send_to(id, data);
            }
            TransportEvent::Disconnected { id } => {
                println!("Client {} disconnected", id);
            }
            _ => {}
        }
    }
}
```

### Option 2: High-Level Messenger (Typed Messages)

Use the messenger layer for type-safe, serialized messages with any
serialization format (bincode, serde, hand-written, etc.). Bincode example:

```rust
use rustcomm::prelude::*;
pub use bincode::{Decode, Encode};
use config::Config;

#[derive(Decode, Encode, Debug)]
struct ChatMessage {
    user: String,
    text: String,
}
impl_message!(ChatMessage);

// Create registry and register message
let mut registry = MessageRegistry::new();
register_bincode_message!(registry, ChatMessage);

// Create messenger with default TCP transport
let config = Config::default();
let mut messenger = Messenger::new(&config, &registry)?;

// Start listening
let (listener_id, addr) = messenger.listen("127.0.0.1:8080")?;
println!("Listening on {}", addr);

// Main event loop
loop {
    for event in messenger.fetch_events()? {
        match event {
            MessengerEvent::Connected { id } => {
                println!("Client {} connected", id);
            }
            MessengerEvent::Message { id, msg } => {
                if let Some(chat) = msg.downcast_ref::<ChatMessage>() {
                    println!("{}: {}", chat.user, chat.text);
                }
            }
            MessengerEvent::Disconnected { id } => {
                println!("Client {} disconnected", id);
            }
            _ => {}
        }
    }
}
```

## Configuration

RustComm is configured through the [`config::Config`](https://docs.rs/config/)
crate. You can use configuration files (TOML, YAML), environment variables, or
build configs programmatically.

### Configuration Keys

#### General

| Key | Description |
|-----|-------------|
| `transport_type` | Transport protocol: `"tcp"` or `"tls"` (defaults to `"tcp"`) |
| `poll_capacity` | Event polling capacity for mio (default: 256) |

#### TCP Configuration

| Key | Description |
|-----|-------------|
| `max_read_size` | Maximum bytes per socket read call |

#### TLS Configuration

**Required** (when `transport_type = "tls"`):

| Key | Description |
|-----|-------------|
| `tls_server_cert` | Path to server certificate (PEM format) - required for `listen()` |
| `tls_server_key` | Path to server private key (PEM format) - required for `listen()` |
| `tls_ca_cert` | Path to CA certificate (PEM format) - required for `connect()` |

**Optional:**

| Key | Description |
|-----|-------------|
| `max_read_size` | Maximum plaintext bytes per read from rustls |

### Configuration Examples

#### Basic TCP (default)

```toml
# Minimal - uses all defaults
# transport_type defaults to "tcp"
```

#### TLS with custom buffer size

```toml
transport_type = "tls"
tls_server_cert = "/path/to/server.crt"
tls_server_key = "/path/to/server.key"
tls_ca_cert = "/path/to/ca.crt"
max_read_size = 2097152  # 2 MB
```

#### Named Instances

Use namespacing to configure different messenger instances separately:

```toml
# Global defaults
transport_type = "tcp"

# Game server instance uses larger buffers
[game_server]
max_read_size = 4194304  # 4 MB

# Auth server uses TLS
[auth_server]
transport_type = "tls"
tls_server_cert = "/path/to/auth.crt"
tls_server_key = "/path/to/auth.key"
tls_ca_cert = "/path/to/ca.crt"
```

Use with:

```rust
let game_messenger = Messenger::new_named(&config, &registry, "game_server")?;
let auth_messenger = Messenger::new_named(&config, &registry, "auth_server")?;
```

### Loading Configuration

#### From a file

```rust
use config::Config;

let config = Config::builder()
    .add_source(config::File::with_name("config.toml"))
    .build()?;
```

#### Programmatically

```rust
use config::Config;

let config = Config::builder()
    .set_default("transport_type", "tls")?
    .set_default("tls_ca_cert", "/path/to/ca.crt")?
    .build()?;
```

#### Environment variables

```rust
use config::Config;

let config = Config::builder()
    .add_source(config::Environment::with_prefix("RUSTCOMM"))
    .build()?;
```

Then set: `RUSTCOMM_TRANSPORT_TYPE=tls`, etc.

## Examples

The repository includes several examples:

- **minimal** - Simplest messenger-based client-server
- **minimal_transport** - Simplest transport-based (raw bytes) client-server
- **p2p_mesh** - Peer-to-peer mesh network demonstration
- **thread_pool** - Multi-threaded message processing with worker pool
- **chat** - Full-featured chat application with multiple server variants
  - `chat_client` - Chat client
  - `chat_server_single` - Single-threaded chat server
  - `chat_server_multi` - Multi-threaded chat server

Run an example:

```bash
cargo run --example minimal
cargo run --example chat_server_single
cargo run --example chat_client
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

## Contribution

Any contribution intentionally submitted for inclusion in the work by you shall
be dual licensed as MIT OR Apache-2.0, without any additional terms or
conditions.
