//! Optional bincode serialization helpers.
//!
//! This module provides convenience wrappers for using bincode 2.0 with rustcomm.
//! Rustcomm's core is serialization-agnostic - you can use any serialization
//! approach (bincode, serde-based formats, hand-written, etc.). This module is
//! available when the `bincode` feature is enabled (enabled by default).

use crate::error::Error;
use crate::Message;

// ============================================================================
// Macros
// ============================================================================

/// Registers a message type with a registry using bincode serialization.
///
/// This macro simplifies the registration of message types by automatically
/// generating the appropriate serializer and deserializer closures for bincode.
///
/// # Example
///
/// ```no_run
/// use rustcomm::{MessageRegistry, register_bincode_message, impl_message};
/// use bincode::{Encode, Decode};
///
/// #[derive(Encode, Decode, Debug)]
/// struct MyMessage { data: String }
/// impl_message!(MyMessage);
///
/// let mut registry = MessageRegistry::new();
/// register_bincode_message!(registry, MyMessage);
/// ```
#[macro_export]
macro_rules! register_bincode_message {
    ($registry:expr, $type:ty) => {
        $registry.register(
            stringify!($type),
            $crate::bincode::serialize_message::<$type>,
            $crate::bincode::deserialize_message::<$type>,
        )
    };
}

// Re-export the macro with a shorter name in the bincode module namespace
pub use register_bincode_message as register_message;

// ============================================================================
// Serialization
// ============================================================================

/// Serializes a message trait object into a byte vector using bincode.
///
/// This downcasts the trait object to the concrete type and serializes it.
pub fn serialize_message<T>(msg: &dyn Message, buf: &mut Vec<u8>)
where
    T: Message + bincode::Encode + 'static,
{
    let concrete = msg.downcast_ref::<T>().expect("message type mismatch");
    bincode::encode_into_std_write(concrete, buf, bincode::config::standard())
        .expect("bincode serialization failed");
}

// ============================================================================
// Deserialization
// ============================================================================

/// Deserializes a message from bytes and returns it as a boxed trait object.
///
/// This is a convenience function for use with MessageRegistry that combines
/// deserialization and boxing in one step.
pub fn deserialize_message<T>(data: &[u8]) -> Result<Box<dyn Message>, Error>
where
    T: bincode::Decode<()> + Message + 'static,
{
    bincode::decode_from_slice::<T, _>(data, bincode::config::standard())
        .map(|(msg, _len)| Box::new(msg) as Box<dyn Message>)
        .map_err(|e| Error::MalformedData(format!("bincode: {}", e)))
}
