//! Optional bincode serialization helpers.
//!
//! This module provides convenience wrappers for using [bincode
//! 2.0](https://docs.rs/bincode) with rustcomm. Rustcomm's core is
//! serialization-agnostic - you can use any serialization approach (bincode,
//! serde-based formats, hand-written, etc.). This module is available when the
//! `bincode` feature is enabled (enabled by default).

use crate::error::Error;
use crate::Message;

// ============================================================================
// Macros
// ============================================================================

/// Registers a message type with a registry using bincode serialization.
///
/// This macro simplifies the registration of message types by providing
/// the appropriate serializer and deserializer function pointers for bincode.
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

// ============================================================================
// Serialization
// ============================================================================

/// Serializes a [`Message`] into a byte vector using bincode.
///
/// Helper function for serializing messages with bincode. Used with
/// [`MessageRegistry`] via the [`register_bincode_message!`] macro.
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

/// Deserializes a message from bytes and returns it as a boxed [`Message`].
///
/// Helper function for deserializing bincode-encoded messages. Used with
/// [`MessageRegistry`] via the [`register_bincode_message!`] macro.
pub fn deserialize_message<T>(data: &[u8]) -> Result<Box<dyn Message>, Error>
where
    T: bincode::Decode<()> + Message + 'static,
{
    bincode::decode_from_slice::<T, _>(data, bincode::config::standard())
        .map(|(msg, _len)| Box::new(msg) as Box<dyn Message>)
        .map_err(|e| Error::MalformedData(format!("bincode: {}", e)))
}
