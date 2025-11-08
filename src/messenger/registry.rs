use super::Message;
use crate::error::Error;
use std::collections::HashMap;
use tracing::debug;

/// Type alias for amessage serialization functions.
///
/// A `MessageSerializer` is a function that takes a message reference and a buffer,
/// and serializes the message into the buffer.
///
/// # Function Signature
///
/// ```text
/// fn(&dyn Message, &mut Vec<u8>)
/// ```
pub type MessageSerializer = fn(&dyn Message, &mut Vec<u8>);

/// Type alias for message deserialization functions.
///
/// A `MessageDeserializer` is a function that takes raw bytes and attempts
/// to deserialize a message of a specific type, returning it as a boxed trait object.
///
/// # Function Signature
///
/// ```ignore
/// fn(&[u8]) -> Result<Box<dyn Message>, Error>
/// ```
///
/// # Parameters
///
/// - `&[u8]` - The raw bytes to deserialize from (guaranteed to be complete by framing layer)
///
/// # Returns
///
/// - `Ok(Box<dyn Message>)` - Message successfully deserialized
/// - `Err(Error)` - Malformed data or deserialization error
pub type MessageDeserializer = fn(&[u8]) -> Result<Box<dyn Message>, Error>;

/// Codec pair containing serializer and deserializer functions.
#[derive(Clone, Copy)]
pub(crate) struct CodecPair {
    pub(crate) serializer: MessageSerializer,
    pub(crate) deserializer: MessageDeserializer,
}

/// Message registry for serializing and deserializing messages by ID.
///
/// The registry maps message type identifiers to their serialization and
/// deserialization functions. It is completely agnostic about the serialization
/// format - you can use any approach you want (bincode, JSON, MessagePack,
/// custom binary formats, etc.).
///
/// # Usage
///
/// ```no_run
/// use rustcomm::{MessageRegistry, Message, Error, impl_message};
///
/// #[derive(Debug)]
/// struct MyMessage {
///     data: String,
/// }
/// impl_message!(MyMessage);
///
/// // Define custom serialization functions
/// fn serialize(msg: &dyn Message, buf: &mut Vec<u8>) {
///     let m = msg.downcast_ref::<MyMessage>().unwrap();
///     // ... your serialization logic ...
/// }
///
/// fn deserialize(data: &[u8]) -> Result<Box<dyn Message>, Error> {
///     // ... your deserialization logic ...
///     # Ok(Box::new(MyMessage { data: String::new() }))
/// }
///
/// let mut registry = MessageRegistry::new();
/// registry.register(stringify!(MyMessage), serialize, deserialize);
/// ```
#[derive(Clone, Default)]
pub struct MessageRegistry {
    codecs: HashMap<String, CodecPair>,
}

impl MessageRegistry {
    /// Creates a new empty message registry.
    pub fn new() -> Self {
        Self {
            codecs: HashMap::new(),
        }
    }

    /// Registers a message serializer/deserializer pair for the given message ID.
    pub fn register(
        &mut self,
        msg_id: &str,
        serializer: MessageSerializer,
        deserializer: MessageDeserializer,
    ) -> &mut Self {
        debug!(msg_id, "Registering message codec pair");
        self.codecs.insert(
            msg_id.to_string(),
            CodecPair {
                serializer,
                deserializer,
            },
        );
        self
    }

    // Gets the codec pair for a message ID, if registered.
    pub(crate) fn get(&self, msg_id: &str) -> Option<&CodecPair> {
        self.codecs.get(msg_id)
    }
}
