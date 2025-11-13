use super::registry::MessageRegistry;
use crate::error::Error;
use downcast_rs::{impl_downcast, Downcast};
use std::fmt::Debug;
use tracing::{error, trace, warn};

// ============================================================================
// Type Aliases
// ============================================================================

// Result type for message deserialization, containing the message, context, and bytes
// consumed.
type DeserializeResult<C> = Result<Option<(Box<dyn Message>, C, usize)>, Error>;

// ============================================================================
// Constants
// ============================================================================

const MAGIC: &[u8] = b"mpg!";
const MAGIC_SIZE: usize = MAGIC.len();
const VERSION_MAJOR: u8 = 0;
const VERSION_MINOR: u8 = 1;
const VERSION_SIZE: usize = 2; // major + minor
const BODY_SIZE_SIZE: usize = 4;
const HEADER_SIZE: usize = MAGIC_SIZE + VERSION_SIZE + BODY_SIZE_SIZE;
const INITIAL_BODY_CAPACITY: usize = 64;

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
/// This macro provides the [`message_id()`] implementation, using the type name
/// as the unique identifier. The message ID is used by the registry to route
/// messages to the correct deserializer.
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
// Message Serialization and Deserialization
// ============================================================================

/// Serializes a message with context to its wire format.
///
/// Wire format: \[MAGIC\]\[VERSION\]\[body_size\]\[context\]\[msg_id\]\[msg_body\]
/// - MAGIC: 4 bytes ("mpg!") - helps detect protocol mismatches
/// - VERSION: 2 bytes (major, minor) - protocol version
/// - body_size: 4 bytes (u32 LE) - length of everything after the header
/// - context: variable length (serialized context data)
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

    let mut buf = Vec::with_capacity(HEADER_SIZE + INITIAL_BODY_CAPACITY);

    // Write magic bytes for protocol identification
    buf.extend_from_slice(MAGIC);

    // Write version (major, minor)
    buf.push(VERSION_MAJOR);
    buf.push(VERSION_MINOR);

    // Reserve space for body size (will be filled in later)
    let body_size_pos = buf.len();
    buf.extend(&[0u8; 4]);

    // Serialize context first
    ctx.serialize_into(&mut buf);

    // Serialize message ID with length prefix (manual format)
    let msg_id_bytes = msg_id.as_bytes();
    buf.extend(&(msg_id_bytes.len() as u32).to_le_bytes());
    buf.extend(msg_id_bytes);

    // Serialize message-specific body data using the registered serializer
    (codec_pair.serializer)(msg, &mut buf);

    // Go back and fill in the actual body size
    let body_len = (buf.len() - HEADER_SIZE) as u32;
    buf[body_size_pos..body_size_pos + 4].copy_from_slice(&body_len.to_le_bytes());

    buf
}

/// Deserializes a message with context from a buffer.
///
/// This is an internal function. Use `Messenger::fetch_events` instead.
///
/// This function is designed for streaming scenarios where data arrives
/// incrementally. It will return `Ok(None)` when there's insufficient data
/// rather than erroring, allowing the caller to wait for more bytes to arrive.
///
/// Returns:
/// - `Ok(Some((msg, ctx, bytes_read)))` - Successfully deserialized message and context
/// - `Ok(None)` - Not enough data available (normal streaming condition)
/// - `Err(_)` - Invalid data (bad magic bytes, malformed data, unknown message ID)
pub(super) fn deserialize_message<C: Context>(
    buf: &[u8],
    registry: &MessageRegistry,
) -> DeserializeResult<C> {
    // Need at least header bytes to proceed
    if buf.len() < HEADER_SIZE {
        return Ok(None);
    }

    // Verify magic bytes to ensure this is our protocol
    if &buf[0..MAGIC_SIZE] != MAGIC {
        error!(
            expected = ?MAGIC,
            received = ?&buf[0..MAGIC_SIZE],
            "Invalid magic bytes in message header"
        );
        return Err(Error::InvalidMagicBytes);
    }

    // Read and validate version
    let version_major = buf[MAGIC_SIZE];
    let version_minor = buf[MAGIC_SIZE + 1];

    if version_major != VERSION_MAJOR || version_minor != VERSION_MINOR {
        error!(
            expected_major = VERSION_MAJOR,
            expected_minor = VERSION_MINOR,
            received_major = version_major,
            received_minor = version_minor,
            "Protocol version mismatch"
        );
        return Err(Error::VersionMismatch {
            expected_major: VERSION_MAJOR,
            expected_minor: VERSION_MINOR,
            received_major: version_major,
            received_minor: version_minor,
        });
    }

    // Read the body size to know how many total bytes we need
    let body_size_bytes: [u8; 4] = match buf
        .get(MAGIC_SIZE + VERSION_SIZE..MAGIC_SIZE + VERSION_SIZE + BODY_SIZE_SIZE)
        .and_then(|s| s.try_into().ok())
    {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    let body_size = u32::from_le_bytes(body_size_bytes) as usize;

    // Calculate total message size and check if we have enough data
    let msg_size = body_size + HEADER_SIZE;

    if buf.len() < msg_size {
        return Ok(None); // Wait for more data
    }

    // Extract the body (everything after the header)
    let body = &buf[HEADER_SIZE..msg_size];

    // Deserialize context first
    let (ctx, ctx_bytes) = C::deserialize(body)?;

    // The remaining body starts after the context
    let remaining_body = &body[ctx_bytes..];

    // Read message ID length prefix
    let msg_id_len_bytes: [u8; 4] = match remaining_body.get(0..4).and_then(|s| s.try_into().ok()) {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    let msg_id_len = u32::from_le_bytes(msg_id_len_bytes) as usize;

    if remaining_body.len() < 4 + msg_id_len {
        return Ok(None);
    }

    // Extract and validate message ID as UTF-8 string
    let msg_id = std::str::from_utf8(&remaining_body[4..4 + msg_id_len]).map_err(|e| {
        warn!(%e, "Invalid UTF-8 in message ID");
        Error::MalformedData(format!("Invalid UTF-8 in message ID: {}", e))
    })?;

    // The remaining bytes are the message-specific body data
    let msg_data = &remaining_body[4 + msg_id_len..];

    // Look up the codec pair for this message type
    let codec_pair = registry.get(msg_id).ok_or_else(|| {
        warn!(msg_id, "Unknown message ID");
        Error::UnknownMessageId(msg_id.to_string())
    })?;

    trace!(msg_id, len = msg_size, "Deserializing message");

    // Deserialize the message body using the registered deserializer
    // At this point we have complete data as guaranteed by the framing layer
    let msg = (codec_pair.deserializer)(msg_data).map_err(|e| {
        warn!(msg_id, error = %e, "Failed to deserialize message");
        e
    })?;

    Ok(Some((msg, ctx, msg_size)))
}
