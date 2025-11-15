//! Message framing and routing for the messenger layer.
//!
//! This module provides a **simple length-prefixed framing layer** for messages
//! sent over the transport. The framing protocol is intentionally minimal: it
//! only adds a 4-byte length prefix to delimit message boundaries in the byte
//! stream.
//!
//! # Framing Protocol
//!
//! Each frame consists of:
//! - **Length prefix**: 4 bytes (u32 little-endian) indicating payload size
//! - **Payload**: Variable-length data containing context, message ID, and
//!   message body
//!
//! # Message Routing
//!
//! The payload contains:
//! - An optional application-defined context (serialized via [`Context`] trait)
//! - Message ID (string identifying the message type)
//! - Message body (serialized via [`MessageRegistry`] codecs)
//!
//! The [`MessageRegistry`] maps message IDs to serialization/deserialization
//! functions, enabling type-safe message dispatch.

use super::registry::MessageRegistry;
use crate::error::Error;
use downcast_rs::{impl_downcast, Downcast};
use std::fmt::Debug;
use tracing::{trace, warn};

// ============================================================================
// Type Aliases
// ============================================================================

// Result type for message deserialization, containing the message, context, and bytes
// consumed.
type DeserializeResult<C> = Result<Option<(Box<dyn Message>, C, usize)>, Error>;

// ============================================================================
// Constants
// ============================================================================

/// Size of the message length prefix in bytes (u32)
const FRAME_SIZE_BYTES: usize = 4;
/// Initial capacity for message payload buffer
const INITIAL_PAYLOAD_CAPACITY: usize = 64;

// ============================================================================
// Message Trait
// ============================================================================

/// Trait for message types that can be sent over the network.
///
/// All messages must be Send + Debug + Downcast to support multi-threaded
/// message dispatch and downcasting for type-specific handling.
pub trait Message: Send + Debug + Downcast {
    /// Returns the unique identifier for this message type.
    fn message_id(&self) -> &str;
}

impl_downcast!(Message);

// ============================================================================
// Context Trait
// ============================================================================

/// Trait for message context that can be serialized alongside messages.
///
/// Context provides a way to add metadata or wrapper information around
/// messages without modifying the message types themselves.
pub trait Context: Debug {
    /// Serializes the context into the provided buffer.
    fn serialize_into(&self, buf: &mut Vec<u8>);

    /// Deserializes the context from a buffer slice.
    ///
    /// Returns the deserialized context and number of bytes consumed.
    fn deserialize(buf: &[u8]) -> Result<(Self, usize), Error>
    where
        Self: Sized;
}

/// An empty context implementation that performs no serialization.
///
/// This is useful as a default context when no additional metadata is needed.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmptyContext;

impl Context for EmptyContext {
    fn serialize_into(&self, _buf: &mut Vec<u8>) {
        // No-op: nothing to serialize
    }

    fn deserialize(_buf: &[u8]) -> Result<(Self, usize), Error> {
        // No-op: return empty context with 0 bytes consumed
        Ok((EmptyContext, 0))
    }
}

// ============================================================================
// impl_message! Macro
// ============================================================================

/// Implements the [`Message`] trait for a message type.
///
/// This macro provides the [`Message::message_id`] implementation, using the
/// type name as the unique identifier. The message ID is used by the registry
/// to route messages to the correct deserializer.
///
/// # Example
///
/// ```no_run
/// use rustcomm::impl_message;
///
/// #[derive(Debug)]
/// struct ChatMessage { text: String }
/// impl_message!(ChatMessage);
/// ```
#[macro_export]
macro_rules! impl_message {
    ($type:ty) => {
        impl $crate::Message for $type {
            fn message_id(&self) -> &str {
                stringify!($type)
            }
        }
    };
}

// ============================================================================
// Message Framing
// ============================================================================

/// Serializes a message with context into a length-prefixed frame.
///
/// This is a simple framing layer that prepends a 4-byte length prefix (u32 LE)
/// to the payload. The payload contains the context and message data as
/// serialized by the registry's codec.
///
/// Frame format: \[length\]\[context\]\[msg_id\]\[msg_body\]
/// - length: 4 bytes (u32 LE) - length of the payload (everything after length
///   prefix)
/// - context: optional variable length (serialized context data)
/// - msg_id: variable length string (includes length prefix)
/// - msg_body: variable length data (format depends on registered serializer)
pub(super) fn serialize_message<C: Context>(
    msg: &dyn Message,
    ctx: &C,
    registry: &MessageRegistry,
) -> Vec<u8> {
    let msg_id = msg.message_id();
    trace!(msg_id, "Serializing message");

    // Get the serializer for this message type
    let codec_pair = registry.get(msg_id).expect("Message not registered");

    let mut buf = Vec::with_capacity(FRAME_SIZE_BYTES + INITIAL_PAYLOAD_CAPACITY);

    // Reserve space for length prefix (will be filled in later)
    let length_pos = buf.len();
    buf.extend(&[0u8; FRAME_SIZE_BYTES]);

    // Serialize context first
    ctx.serialize_into(&mut buf);

    // Serialize message ID with length prefix
    let msg_id_bytes = msg_id.as_bytes();
    buf.extend(&(msg_id_bytes.len() as u32).to_le_bytes());
    buf.extend(msg_id_bytes);

    // Serialize message-specific body data using the registered serializer
    (codec_pair.serializer)(msg, &mut buf);

    // Go back and fill in the actual payload length
    let payload_len = (buf.len() - FRAME_SIZE_BYTES) as u32;
    buf[length_pos..length_pos + FRAME_SIZE_BYTES].copy_from_slice(&payload_len.to_le_bytes());

    buf
}

/// Deserializes a length-prefixed message frame with context from a buffer.
///
/// This function is designed for streaming scenarios where data arrives
/// incrementally. It will return `Ok(None)` when there's insufficient data
/// rather than erroring, allowing the caller to wait for more bytes to arrive.
///
/// Returns:
/// - `Ok(Some((msg, ctx, bytes_read)))` - Successfully deserialized message and
///   context
/// - `Ok(None)` - Not enough data available (normal streaming condition)
/// - `Err(_)` - Invalid data (malformed data, unknown message ID)
pub(super) fn deserialize_message<C: Context>(
    buf: &[u8],
    registry: &MessageRegistry,
) -> DeserializeResult<C> {
    // Need at least the length prefix to proceed
    if buf.len() < FRAME_SIZE_BYTES {
        return Ok(None);
    }

    // Read the payload length
    let length_bytes: [u8; FRAME_SIZE_BYTES] = buf[0..FRAME_SIZE_BYTES]
        .try_into()
        .expect("slice is exactly FRAME_SIZE_BYTES");
    let payload_len = u32::from_le_bytes(length_bytes) as usize;

    // Calculate total frame size and check if we have enough data
    let frame_size = payload_len + FRAME_SIZE_BYTES;

    if buf.len() < frame_size {
        return Ok(None); // Wait for more data
    }

    // Extract the payload (everything after the length prefix)
    let payload = &buf[FRAME_SIZE_BYTES..frame_size];

    // Deserialize context first
    let (ctx, ctx_bytes) = C::deserialize(payload)?;

    // The remaining payload starts after the context
    let remaining_payload = &payload[ctx_bytes..];

    // Read message ID length prefix
    let msg_id_len_bytes: [u8; 4] = match remaining_payload.get(0..4).and_then(|s| s.try_into().ok()) {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    let msg_id_len = u32::from_le_bytes(msg_id_len_bytes) as usize;

    if remaining_payload.len() < 4 + msg_id_len {
        return Ok(None);
    }

    // Extract and validate message ID as UTF-8 string
    let msg_id = std::str::from_utf8(&remaining_payload[4..4 + msg_id_len]).map_err(|e| {
        warn!(%e, "Invalid UTF-8 in message ID");
        Error::MalformedData(format!("Invalid UTF-8 in message ID: {}", e))
    })?;

    // The remaining bytes are the message-specific body data
    let msg_data = &remaining_payload[4 + msg_id_len..];

    // Look up the codec pair for this message type
    let codec_pair = registry.get(msg_id).ok_or_else(|| {
        warn!(msg_id, "Unknown message ID");
        Error::UnknownMessageId(msg_id.to_string())
    })?;

    trace!(msg_id, len = frame_size, "Deserializing message");

    // Deserialize the message body using the registered deserializer
    let msg = (codec_pair.deserializer)(msg_data).map_err(|e| {
        warn!(msg_id, error = %e, "Failed to deserialize message");
        e
    })?;

    Ok(Some((msg, ctx, frame_size)))
}
