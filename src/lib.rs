//! RustComm - A lightweight, type-safe messaging library for Rust
//!
//! RustComm provides a simple yet powerful abstraction for building networked applications
//! with support for TCP and TLS transports. It handles connection management, message
//! serialization/deserialization, and provides both single-threaded and thread-safe APIs.
//!
//! See the [README](https://github.com/floco2025/rustcomm) for quick start guide, examples, and configuration options.

// Internal-only modules
pub(crate) mod error;
pub(crate) mod messenger;
pub(crate) mod transport;

// These are the intended public API
pub use error::Error;
pub use messenger::{
    Message, MessageDeserializer, MessageRegistry, MessageSerializer, Messenger, MessengerEvent,
    MessengerInterface,
};
pub use transport::{Transport, TransportEvent, TransportInterface};

// Bincode support (optional feature, enabled by default)
#[cfg(feature = "bincode")]
pub use messenger::bincode;

/// Convenient re-exports of commonly used types.
pub mod prelude {
    pub use crate::error::Error;
    pub use crate::impl_message;
    pub use crate::messenger::{
        Message, MessageDeserializer, MessageRegistry, MessageSerializer, Messenger,
        MessengerEvent, MessengerInterface,
    };
    pub use crate::transport::{Transport, TransportEvent, TransportInterface};

    // Bincode support (optional feature, enabled by default)
    #[cfg(feature = "bincode")]
    pub use crate::register_bincode_message;
}

// Re-export functions that are only needed for testing
// Hidden from documentation to discourage use in production code
#[doc(hidden)]
pub use messenger::{deserialize_message, serialize_message};
