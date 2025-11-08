//! High-level message-oriented networking built on the transport layer.
//!
//! The [`Messenger`] handles serialization, deserialization, and dispatch of
//! strongly-typed messages over any Transport.

#[cfg(feature = "bincode")]
pub mod bincode;
mod interface;
mod message;
mod registry;

pub use interface::MessengerInterface;
pub use message::{deserialize_message, serialize_message, Message};
pub use registry::{MessageDeserializer, MessageRegistry, MessageSerializer};

use crate::error::Error;
use crate::transport::Transport;
use config::Config;

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use tracing::{debug, error, instrument, warn};

/// Messenger handler for client-server communication.
///
/// Wraps a transport layer and adds message serialization/deserialization. Not
/// thread-safe - use [`MessengerInterface`] for cross-thread communication.
///
/// The transport type (TCP or TLS) is automatically selected based on
/// configuration.
pub struct Messenger {
    transport: Transport,
    registry: MessageRegistry,
    /// Per-connection receive buffers for handling partial messages
    recv_buffers: HashMap<usize, Vec<u8>>,
}

/// Events produced by [`Messenger::fetch_events()`]
///
/// # Variants
///
/// - `Inactive` - The messenger has no listeners or connections.
/// - `Connected { id }` - Connection established.
/// - `ConnectionFailed { id }` - Connection establishement failed.
/// - `Disconnected { id }` - Connection closed. Clean up state associated with this `id`.
/// - `Message { id, msg }` - Received and deserialized message. Use `downcast_ref::<T>()` to access.
#[derive(Debug)]
pub enum MessengerEvent {
    Inactive,
    Connected { id: usize },
    ConnectionFailed { id: usize },
    Disconnected { id: usize },
    Message { id: usize, msg: Box<dyn Message> },
}

// ============================================================================
// Constructors
// ============================================================================

impl Messenger {
    /// Creates a new Messenger instance based on configuration.
    ///
    /// The transport type (TCP or TLS) is automatically selected based on the
    /// `transport_type` configuration key. Defaults to "tcp" if not specified.
    ///
    /// # Configuration Keys
    ///
    /// - `transport_type`: Either "tcp" or "tls" (defaults to "tcp")
    /// - Plus all keys for the specific transport type
    pub fn new(config: &Config, registry: &MessageRegistry) -> Result<Self, Error> {
        Self::new_named(config, registry, "")
    }

    /// Creates a new named Messenger instance with configuration namespacing.
    ///
    /// Configuration lookup follows this priority:
    /// 1. `{name}.{key}` (e.g., `game_server.transport_type`)
    /// 2. `{key}` (e.g., `transport_type`)
    /// 3. Hard-coded default
    ///
    /// This allows different messenger instances to have different configurations.
    ///
    /// # Configuration Keys
    ///
    /// - `transport_type`: Either "tcp" or "tls"
    /// - Plus all keys for the specific transport type
    ///
    /// # Example
    ///
    /// ```toml
    /// # Global defaults
    /// transport_type = "tcp"
    ///
    /// # Specific to "api_server" instance
    /// [api_server]
    /// transport_type = "tls"
    /// tls_server_cert = "/path/to/cert.pem"
    /// ```
    pub fn new_named(
        config: &Config,
        registry: &MessageRegistry,
        name: &str,
    ) -> Result<Self, Error> {
        let transport = Transport::new_named(config, name)?;
        Self::new_with_transport(transport, config, registry, name)
    }

    /// Creates a new Messenger instance with an explicitly provided transport.
    ///
    /// This is useful for advanced use cases where you need more control over
    /// transport creation. For most cases, use [`new()`](Self::new) or
    /// [`new_named()`](Self::new_named) instead.
    pub fn new_with_transport(
        transport: Transport,
        _config: &Config,
        registry: &MessageRegistry,
        _name: &str,
    ) -> Result<Self, Error> {
        Ok(Self {
            transport,
            registry: registry.clone(),
            recv_buffers: HashMap::new(),
        })
    }
}

impl Messenger {
    /// Starts listening for incoming connections on the specified address.
    ///
    /// Returns a tuple of (listener_id, socket_addr) where:
    /// - `listener_id`: Can be used with
    ///   [`close_listener()`](Self::close_listener) to stop listening. Note:
    ///   This ID cannot be used for sending messages - only for closing the
    ///   listener.
    /// - `socket_addr`: The actual address being listened on (useful when
    ///   binding to port 0 for dynamic allocation).
    ///
    /// Multiple listeners can be added to listen on different addresses/ports.
    #[instrument(skip(self, addr))]
    pub fn listen<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        self.transport.listen(addr)
    }

    /// Initiates a connection to the specified address.
    ///
    /// The connection is established asynchronously. You will receive a
    /// `MessengerEvent::Connected` event when the connection is fully
    /// established, or `MessengerEvent::ConnectionFailed` if it fails.
    ///
    /// Returns a tuple of (connection_id, socket_addr) where:
    /// - `connection_id`: Use this ID to send messages to this connection.
    /// - `socket_addr`: The peer address that was connected to (useful when you
    ///   pass multiple addresses or use DNS names and want to know which address
    ///   was actually used).
    #[instrument(skip(self, addr))]
    pub fn connect<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        self.transport.connect(addr)
    }
}

// ============================================================================
// Public Methods
// ============================================================================

impl Messenger {
    /// Gets a MessengerInterface for sending messages from other threads.
    pub fn get_messenger_interface(&self) -> MessengerInterface {
        let transport_interface = self.transport.get_transport_interface();
        MessengerInterface::new(transport_interface, self.registry.clone())
    }

    /// Gets the local socket addresses, if any.
    pub fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        self.transport.get_listener_addresses()
    }

    /// Blocks until messenger events are available and returns them.
    ///
    /// Returns `MessengerEvent::Inactive` if the transport has no connections
    /// or listeners.
    ///
    /// Only returns unrecoverable errors. Recoverable errors are handled by the
    /// returned events.
    #[instrument(skip(self))]
    pub fn fetch_events(&mut self) -> Result<Vec<MessengerEvent>, Error> {
        let mut dispatch_events = Vec::new();

        while dispatch_events.is_empty() {
            let transport_events = self.transport.fetch_events()?;

            for event in transport_events {
                match event {
                    crate::transport::TransportEvent::Inactive => {
                        // Clean up all receive buffers
                        self.recv_buffers.clear();
                        dispatch_events.push(MessengerEvent::Inactive);
                        break;
                    }
                    crate::transport::TransportEvent::Connected { id } => {
                        dispatch_events.push(MessengerEvent::Connected { id });
                    }
                    crate::transport::TransportEvent::ConnectionFailed { id } => {
                        // Clean up receive buffer for this connection
                        self.recv_buffers.remove(&id);
                        dispatch_events.push(MessengerEvent::ConnectionFailed { id });
                    }
                    crate::transport::TransportEvent::Disconnected { id } => {
                        // Clean up receive buffer for this connection
                        self.recv_buffers.remove(&id);
                        dispatch_events.push(MessengerEvent::Disconnected { id });
                    }
                    crate::transport::TransportEvent::Data { id, data } => {
                        // Get or create receive buffer for this connection, taking ownership
                        // of data if the buffer doesn't exist or is empty (avoids copy)
                        let recv_buf = self.recv_buffers.entry(id).or_insert_with(Vec::new);
                        if recv_buf.is_empty() {
                            *recv_buf = data;
                        } else {
                            recv_buf.extend_from_slice(&data);
                        }

                        // Deserialize all complete messages from the buffer
                        let mut recv_pos = 0;
                        let mut had_error = false;

                        while recv_pos < recv_buf.len() {
                            match deserialize_message(&recv_buf[recv_pos..], &self.registry) {
                                Ok(Some((msg, bytes_read))) => {
                                    debug!(
                                        id,
                                        msg_id = msg.message_id(),
                                        len = bytes_read,
                                        "Received message"
                                    );
                                    dispatch_events.push(MessengerEvent::Message { id, msg });
                                    recv_pos += bytes_read;
                                }
                                Ok(None) => {
                                    // Not enough data for a complete message, keep remaining data
                                    break;
                                }
                                Err(err) => {
                                    error!(id, ?err, "Error deserializing message");
                                    had_error = true;
                                    break;
                                }
                            }
                        }

                        // Remove processed data from buffer
                        if recv_pos > 0 {
                            recv_buf.drain(..recv_pos);
                        }

                        // Handle error after we're done with recv_buf
                        if had_error {
                            self.recv_buffers.remove(&id);
                            self.transport.close_connection(id);
                        }
                    }
                }
            }
        }

        debug!(count = dispatch_events.len(), "Fetched events");
        Ok(dispatch_events)
    }

    /// Sends a message to a specific connection.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg), fields(msg_id = msg.message_id()))]
    pub fn send_to(&mut self, to_id: usize, msg: &dyn Message) {
        let data = serialize_message(msg, &self.registry);
        debug!(len = data.len(), "Sending message");
        self.transport.send_to(to_id, data);
    }

    /// Sends a message to multiple specific connections.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg, to_ids), fields(msg_id = msg.message_id()))]
    pub fn send_to_many(&mut self, to_ids: &[usize], msg: &dyn Message) {
        let data = serialize_message(msg, &self.registry);
        debug!(
            count = to_ids.len(),
            len = data.len(),
            "Sending message to many"
        );
        self.transport.send_to_many(to_ids, data);
    }

    /// Broadcasts a message to all connected clients.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg), fields(msg_id = msg.message_id()))]
    pub fn broadcast(&mut self, msg: &dyn Message) {
        let data = serialize_message(msg, &self.registry);
        debug!(len = data.len(), "Broadcasting message");
        self.transport.broadcast(data);
    }

    /// Broadcasts a message to all connected clients except one.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg), fields(msg_id = msg.message_id()))]
    pub fn broadcast_except(&mut self, msg: &dyn Message, except_id: usize) {
        let data = serialize_message(msg, &self.registry);
        debug!(len = data.len(), "Broadcasting message with exception");
        self.transport.broadcast_except(data, except_id);
    }

    /// Broadcasts a message to all connected clients except multiple specified
    /// ones.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg, except_ids), fields(msg_id = msg.message_id()))]
    pub fn broadcast_except_many(&mut self, msg: &dyn Message, except_ids: &[usize]) {
        let data = serialize_message(msg, &self.registry);
        debug!(
            except_count = except_ids.len(),
            len = data.len(),
            "Broadcasting message with many exceptions"
        );
        self.transport.broadcast_except_many(data, except_ids);
    }

    /// Closes a connection by its ID.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally.
    ///
    /// **Note:** This does not trigger a `MessengerEvent::Disconnected` event.
    #[instrument(skip(self))]
    pub fn close_connection(&mut self, id: usize) {
        // Clean up receive buffer for this connection
        self.recv_buffers.remove(&id);
        self.transport.close_connection(id);
    }

    /// Closes a listener by its ID.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent listener ids, because the listener might have been
    /// closed already internally.
    #[instrument(skip(self))]
    pub fn close_listener(&mut self, id: usize) {
        self.transport.close_listener(id);
    }

    /// Closes all connections and listeners.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// **Note:** This does not trigger `MessengerEvent::Disconnected` events.
    /// However, it will trigger a `MessengerEvent::Inactive` event if no new
    /// connections or listeners are created before calling [`fetch_events()`].
    #[instrument(skip(self))]
    pub fn close_all(&mut self) {
        // Clean up all receive buffers
        self.recv_buffers.clear();
        self.transport.close_all()
    }
}
