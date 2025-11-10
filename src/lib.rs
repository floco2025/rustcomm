//! A lightweight, high-performance communication library built on
//! [mio](https://docs.rs/mio) with TCP and TLS transport support.
//!
//! - **Dual-layer architecture:** Low-level [`Transport`] API for raw bytes, or
//!   higher-level [`Messenger`] API for type-safe serialized messages
//! - **Peer-to-peer:** No fixed server/client roles - any peer can listen, connect,
//!   send, and receive
//! - **Serialization-agnostic:** Bring your own serialization (bincode, serde,
//!   hand-written, etc.) - includes optional bincode helpers
//! - **Easily extensible:** Add custom transports beyond TCP/TLS, such as QUIC.
//! - **Flexible threading:** Works in single-threaded event loops or multi-threaded
//!   architectures like thread pools
//!
//! # Quick Start
//!
//! Add rustcomm to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! rustcomm = "0.1"
//! ```
//!
//! ## Option 1: Low-Level Transport (Raw Bytes)
//!
//! Use the transport layer directly for streaming raw bytes:
//!
//! ```no_run
//! use rustcomm::prelude::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Create TCP transport with default config
//! let config = config::Config::default();
//! let mut transport = Transport::new(&config)?;
//!
//! // Start listening
//! let (listener_id, addr) = transport.listen("127.0.0.1:8080")?;
//!
//! // Event loop
//! loop {
//!     for event in transport.fetch_events()? {
//!         match event {
//!             TransportEvent::Connected { id } => {
//!                 println!("Client {} connected", id);
//!             }
//!             TransportEvent::Data { id, data } => {
//!                 // Process raw bytes
//!                 println!("Received {} bytes from {}", data.len(), id);
//!                 // Echo back
//!                 transport.send_to(id, data);
//!             }
//!             TransportEvent::Disconnected { id } => {
//!                 println!("Client {} disconnected", id);
//!             }
//!             _ => {}
//!         }
//!     }
//! }
//! # }
//! ```
//!
//! ## Option 2: High-Level Messenger (Typed Messages)
//!
//! Use the messenger layer for type-safe, serialized messages with any
//! serialization format (bincode, serde, hand-written, etc.). Bincode example:
//!
//! ```no_run
//! use rustcomm::prelude::*;
//!
//! #[derive(bincode::Decode, bincode::Encode, Debug)]
//! struct ChatMessage {
//!     user: String,
//!     text: String,
//! }
//! impl_message!(ChatMessage);
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Create registry and register message
//! let mut registry = MessageRegistry::new();
//! register_bincode_message!(registry, ChatMessage);
//!
//! // Create messenger with default TCP transport
//! let config = config::Config::default();
//! let mut messenger = Messenger::new(&config, &registry)?;
//!
//! // Start listening
//! let (listener_id, addr) = messenger.listen("127.0.0.1:8080")?;
//! println!("Listening on {}", addr);
//!
//! // Main event loop
//! loop {
//!     for event in messenger.fetch_events()? {
//!         match event {
//!             MessengerEvent::Connected { id } => {
//!                 println!("Client {} connected", id);
//!             }
//!             MessengerEvent::Message { id, msg, .. } => {
//!                 if let Some(chat) = msg.downcast_ref::<ChatMessage>() {
//!                     println!("{}: {}", chat.user, chat.text);
//!                 }
//!             }
//!             MessengerEvent::Disconnected { id } => {
//!                 println!("Client {} disconnected", id);
//!             }
//!             _ => {}
//!         }
//!     }
//! }
//! # }
//! ```
//!
//! # Configuration
//!
//! RustComm is configured through the [`config`](https://docs.rs/config/) crate.
//! You can use configuration files (TOML, YAML), environment variables, or build
//! configs programmatically.
//!
//! ## Configuration Keys
//!
//! ### General
//!
//! | Key | Description |
//! |-----|-------------|
//! | `transport_type` | Transport protocol: `"tcp"` or `"tls"` (defaults to `"tcp"`) |
//! | `poll_capacity` | Event polling capacity for mio (default: 256) |
//!
//! ### TCP Configuration
//!
//! | Key | Description |
//! |-----|-------------|
//! | `max_read_size` | Maximum bytes per socket read call |
//!
//! ### TLS Configuration
//!
//! **Required** (when `transport_type = "tls"`):
//!
//! | Key | Description |
//! |-----|-------------|
//! | `tls_server_cert` | Path to server certificate (PEM format) - required for `listen()` |
//! | `tls_server_key` | Path to server private key (PEM format) - required for `listen()` |
//! | `tls_ca_cert` | Path to CA certificate (PEM format) - required for `connect()` |
//!
//! **Optional:**
//!
//! | Key | Description |
//! |-----|-------------|
//! | `max_read_size` | Maximum plaintext bytes per read from rustls |
//!
//! ## Configuration Examples
//!
//! ### Basic TCP (default)
//!
//! ```toml
//! # Minimal - uses all defaults
//! # transport_type defaults to "tcp"
//! ```
//!
//! ### TLS with custom buffer size
//!
//! ```toml
//! transport_type = "tls"
//! tls_server_cert = "/path/to/server.crt"
//! tls_server_key = "/path/to/server.key"
//! tls_ca_cert = "/path/to/ca.crt"
//! max_read_size = 2097152  # 2 MB
//! ```
//!
//! ### Named Instances
//!
//! Use namespacing to configure different messenger instances separately:
//!
//! ```toml
//! # Global defaults
//! transport_type = "tcp"
//!
//! # Game server instance uses larger buffers
//! [game_server]
//! max_read_size = 4194304  # 4 MB
//!
//! # Auth server uses TLS
//! [auth_server]
//! transport_type = "tls"
//! tls_server_cert = "/path/to/auth.crt"
//! tls_server_key = "/path/to/auth.key"
//! tls_ca_cert = "/path/to/ca.crt"
//! ```
//!
//! Use with:
//!
//! ```no_run
//! # use rustcomm::prelude::*;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let config = config::Config::default();
//! # let registry = MessageRegistry::new();
//! let game_messenger = Messenger::new_named(&config, &registry, "game_server")?;
//! let auth_messenger = Messenger::new_named(&config, &registry, "auth_server")?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Loading Configuration
//!
//! ### From a file
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let config = config::Config::builder()
//!     .add_source(config::File::with_name("config.toml"))
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Programmatically
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let config = config::Config::builder()
//!     .set_default("transport_type", "tls")?
//!     .set_default("tls_ca_cert", "/path/to/ca.crt")?
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Environment variables
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let config = config::Config::builder()
//!     .add_source(config::Environment::with_prefix("RUSTCOMM"))
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! Then set: `RUSTCOMM_TRANSPORT_TYPE=tls`, etc.

// Internal-only modules
pub(crate) mod error;
pub(crate) mod messenger;
pub mod transport;

#[cfg(feature = "rpc")]
pub mod rpc;

// These are the intended public API
pub use error::Error;
pub use messenger::{
    Context, Message, MessageDeserializer, MessageRegistry, MessageSerializer, Messenger,
    MessengerEvent, MessengerInterface,
};
pub use transport::{Transport, TransportEvent, TransportInterface};

// Request-response support (optional feature)
#[cfg(feature = "rpc")]
pub use error::RequestError;
#[cfg(feature = "rpc")]
pub use rpc::{RpcContext, RpcMessenger};

// Bincode support (optional feature, enabled by default)
#[cfg(feature = "bincode")]
pub use messenger::bincode;

/// Convenient re-exports of commonly used types.
pub mod prelude {
    pub use crate::error::Error;
    pub use crate::impl_message;
    pub use crate::messenger::{
        Context, Message, MessageDeserializer, MessageRegistry, MessageSerializer, Messenger,
        MessengerEvent, MessengerInterface,
    };
    pub use crate::transport::{Transport, TransportEvent, TransportInterface};

    // RPC support (optional feature)
    #[cfg(feature = "rpc")]
    pub use crate::error::RequestError;
    #[cfg(feature = "rpc")]
    pub use crate::rpc::{RpcContext, RpcMessenger};

    // Bincode support (optional feature, enabled by default)
    #[cfg(feature = "bincode")]
    pub use crate::register_bincode_message;
}
