//! High-level message-oriented networking built on the transport layer.
//!
//! The [`Messenger`] handles serialization, deserialization, and dispatch of
//! strongly-typed messages over any Transport.

#[cfg(feature = "bincode")]
pub mod bincode;
mod interface;
mod message;
mod registry;

use crate::error::Error;
use crate::transport::Transport;
pub use interface::MessengerInterface;
use message::{deserialize_message, serialize_message};
pub use message::{Context, EmptyContext, Message};
pub use registry::{MessageDeserializer, MessageRegistry, MessageSerializer};

use ::config::Config;
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
pub struct Messenger<C: Context = EmptyContext> {
    transport: Transport,
    registry: MessageRegistry,
    /// Per-connection receive buffers for handling partial messages
    recv_buffers: HashMap<usize, Vec<u8>>,
    _phantom: std::marker::PhantomData<C>,
}

/// Events produced by [`Messenger::fetch_events()`].
///
/// These events represent the lifecycle of connections and messages in the
/// messenger. Handle each event to manage connection state and process incoming
/// messages.
#[derive(Debug)]
pub enum MessengerEvent<C: Context = EmptyContext> {
    /// The messenger has no listeners or connections.
    Inactive,
    /// Connection established.
    Connected { id: usize },
    /// Connection establishment failed.
    ConnectionFailed { id: usize },
    /// Connection closed. Clean up state associated with this `id`.
    Disconnected { id: usize },
    /// Received and deserialized message. Use `downcast_ref::<T>()` to access.
    Message {
        id: usize,
        msg: Box<dyn Message>,
        ctx: C,
    },
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
    /// This allows different messenger instances to have different
    /// configurations.
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
        Ok(Self {
            transport,
            registry: registry.clone(),
            recv_buffers: HashMap::new(),
            _phantom: std::marker::PhantomData,
        })
    }
}

// ============================================================================
// Public Methods
// ============================================================================

impl<C: Context> Messenger<C> {
    /// Creates a new Messenger instance with a custom context type and transport.
    ///
    /// This allows creating a Messenger with a specific context type (e.g., RpcContext)
    /// rather than the default EmptyContext.
    pub fn new_named_with_context(
        transport: Transport,
        _config: &Config,
        registry: &MessageRegistry,
        _name: &str,
    ) -> Result<Self, Error> {
        Ok(Self {
            transport,
            registry: registry.clone(),
            recv_buffers: HashMap::new(),
            _phantom: std::marker::PhantomData,
        })
    }

    // ============================================================================
    // Connection Management
    // ============================================================================

    /// Starts listening for incoming connections on the specified address.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Delegates to [`Transport::listen()`]. See that method for full
    /// documentation.
    #[instrument(skip(self, addr))]
    pub fn listen<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        self.transport.listen(addr)
    }

    /// Initiates a connection to the specified address.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Delegates to [`Transport::connect()`]. See that method for full
    /// documentation.
    #[instrument(skip(self, addr))]
    pub fn connect<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        self.transport.connect(addr)
    }

    /// Gets the local socket addresses of all active listeners.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Delegates to [`Transport::get_listener_addresses()`]. See that method
    /// for full documentation.
    pub fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        self.transport.get_listener_addresses()
    }

    /// Closes a connection by its ID.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Delegates to [`Transport::close_connection()`]. Additionally cleans up
    /// the message receive buffer for this connection.
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
    /// Delegates to [`Transport::close_listener()`]. See that method for full
    /// documentation.
    #[instrument(skip(self))]
    pub fn close_listener(&mut self, id: usize) {
        self.transport.close_listener(id);
    }

    /// Closes all connections.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Delegates to [`Transport::close_all_connections()`]. Additionally cleans
    /// up all connection message receive buffers.
    #[instrument(skip(self))]
    pub fn close_all_connections(&mut self) {
        // Clean up all connection receive buffers
        self.recv_buffers.clear();
        self.transport.close_all_connections()
    }

    /// Shuts down a connection by its ID.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Delegates to [`Transport::shutdown_connection()`]. See that method for
    /// full documentation.
    #[instrument(skip(self))]
    pub fn shutdown_connection(&mut self, id: usize, how: std::net::Shutdown) {
        self.transport.shutdown_connection(id, how);
    }

    /// Shuts down all connections.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Delegates to [`Transport::shutdown_all_connections()`]. See that method
    /// for full documentation.
    #[instrument(skip(self))]
    pub fn shutdown_all_connections(&mut self, how: std::net::Shutdown) {
        self.transport.shutdown_all_connections(how);
    }

    /// Closes all listeners.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Delegates to [`Transport::close_all_listeners()`]. See that method for
    /// full documentation.
    #[instrument(skip(self))]
    pub fn close_all_listeners(&mut self) {
        self.transport.close_all_listeners()
    }

    /// Closes all connections and listeners.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Delegates to [`Transport::close_all()`]. Additionally cleans up all
    /// message receive buffers.
    #[instrument(skip(self))]
    pub fn close_all(&mut self) {
        // Clean up all receive buffers
        self.recv_buffers.clear();
        self.transport.close_all()
    }

    // ============================================================================
    // Data Operations
    // ============================================================================

    /// Sends a message to a specific connection.
    ///
    /// Calls [`Self::send_to_with_context`] with an empty context.
    #[instrument(skip(self, msg), fields(msg_id = msg.message_id()))]
    pub fn send_to(&mut self, to_id: usize, msg: &dyn Message)
    where
        C: Default,
    {
        self.send_to_with_context(to_id, msg, &C::default());
    }

    /// Sends a message with context to a specific connection.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg, ctx), fields(msg_id = msg.message_id()))]
    pub fn send_to_with_context(&mut self, to_id: usize, msg: &dyn Message, ctx: &C) {
        let data = serialize_message(msg, ctx, &self.registry);
        debug!(len = data.len(), "Sending message");
        self.transport.send_to(to_id, data);
    }

    /// Sends a message to multiple specific connections.
    ///
    /// Calls [`Self::send_to_many_with_context`] with an empty context.
    #[instrument(skip(self, msg), fields(msg_id = msg.message_id()))]
    pub fn send_to_many(&mut self, to_ids: &[usize], msg: &dyn Message)
    where
        C: Default,
    {
        self.send_to_many_with_context(to_ids, msg, &C::default());
    }

    /// Sends a message with context to multiple specific connections.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg, to_ids, ctx), fields(msg_id = msg.message_id()))]
    pub fn send_to_many_with_context(&mut self, to_ids: &[usize], msg: &dyn Message, ctx: &C) {
        let data = serialize_message(msg, ctx, &self.registry);
        debug!(
            count = to_ids.len(),
            len = data.len(),
            "Sending message to many"
        );
        self.transport.send_to_many(to_ids, data);
    }

    /// Broadcasts a message to all connected clients.
    ///
    /// Calls [`Self::broadcast_with_context`] with an empty context.
    #[instrument(skip(self, msg), fields(msg_id = msg.message_id()))]
    pub fn broadcast(&mut self, msg: &dyn Message)
    where
        C: Default,
    {
        self.broadcast_with_context(msg, &C::default());
    }

    /// Broadcasts a message with context to all connected clients.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg, ctx), fields(msg_id = msg.message_id()))]
    pub fn broadcast_with_context(&mut self, msg: &dyn Message, ctx: &C) {
        let data = serialize_message(msg, ctx, &self.registry);
        debug!(len = data.len(), "Broadcasting message");
        self.transport.broadcast(data);
    }

    /// Broadcasts a message to all connected clients except one.
    ///
    /// Calls [`Self::broadcast_except_with_context`] with an empty context.
    #[instrument(skip(self, msg), fields(msg_id = msg.message_id()))]
    pub fn broadcast_except(&mut self, msg: &dyn Message, except_id: usize)
    where
        C: Default,
    {
        self.broadcast_except_with_context(msg, except_id, &C::default());
    }

    /// Broadcasts a message with context to all connected clients except one.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg, ctx), fields(msg_id = msg.message_id()))]
    pub fn broadcast_except_with_context(&mut self, msg: &dyn Message, except_id: usize, ctx: &C) {
        let data = serialize_message(msg, ctx, &self.registry);
        debug!(len = data.len(), "Broadcasting message with exception");
        self.transport.broadcast_except(data, except_id);
    }

    /// Broadcasts a message to all connected clients except multiple specified
    /// ones.
    ///
    /// Calls [`Self::broadcast_except_many_with_context`] with an empty
    /// context.
    #[instrument(skip(self, msg), fields(msg_id = msg.message_id()))]
    pub fn broadcast_except_many(&mut self, msg: &dyn Message, except_ids: &[usize])
    where
        C: Default,
    {
        self.broadcast_except_many_with_context(msg, except_ids, &C::default());
    }

    /// Broadcasts a message with context to all connected clients except
    /// multiple specified ones.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`MessengerInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// MessengerEvents.
    #[instrument(skip(self, msg, except_ids, ctx), fields(msg_id = msg.message_id()))]
    pub fn broadcast_except_many_with_context(
        &mut self,
        msg: &dyn Message,
        except_ids: &[usize],
        ctx: &C,
    ) {
        let data = serialize_message(msg, ctx, &self.registry);
        debug!(
            except_count = except_ids.len(),
            len = data.len(),
            "Broadcasting message with many exceptions"
        );
        self.transport.broadcast_except_many(data, except_ids);
    }

    // ============================================================================
    // Event Operations
    // ============================================================================

    /// Blocks until messenger events are available and returns them.
    ///
    /// Returns [`MessengerEvent::Inactive`] if the [`Transport`] has no
    /// connections or listeners.
    ///
    /// Only returns unrecoverable errors. Recoverable errors are handled by the
    /// returned events.
    ///
    /// **Note:** This method delegates to [`Transport::fetch_events()`] and
    /// adds message deserialization on top of the raw transport events.
    #[instrument(skip(self))]
    pub fn fetch_events(&mut self) -> Result<Vec<MessengerEvent<C>>, Error> {
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
                            match deserialize_message::<C>(&recv_buf[recv_pos..], &self.registry) {
                                Ok(Some((msg, ctx, bytes_read))) => {
                                    debug!(
                                        id,
                                        msg_id = msg.message_id(),
                                        len = bytes_read,
                                        "Received message"
                                    );
                                    dispatch_events.push(MessengerEvent::Message { id, msg, ctx });
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

    // ============================================================================
    // Utilities
    // ============================================================================

    /// Gets a MessengerInterface for sending messages from other threads.
    pub fn get_messenger_interface(&self) -> MessengerInterface<C> {
        let transport_interface = self.transport.get_transport_interface();
        MessengerInterface::new(transport_interface, self.registry.clone())
    }
}
