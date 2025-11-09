use super::registry::MessageRegistry;
use super::{serialize_message, Message};
use crate::transport::TransportInterface;
use crate::Error;
use std::net::SocketAddr;

/// Thread-safe interface for sending messages through a Messenger.
///
/// Allows multiple threads to send messages to the same
/// [`Messenger`](super::Messenger). Obtain an instance by calling
/// [`Messenger::get_messenger_interface()`](super::Messenger::get_messenger_interface).
///
/// Multiple threads can hold cloned instances to send messages concurrently.
#[derive(Clone)]
pub struct MessengerInterface {
    transport_interface: TransportInterface,
    registry: MessageRegistry,
}

impl MessengerInterface {
    pub(super) fn new(transport_interface: TransportInterface, registry: MessageRegistry) -> Self {
        Self {
            transport_interface,
            registry,
        }
    }

    // ============================================================================
    // Connection Management
    // ============================================================================

    /// Opens a new listener on the specified address.
    ///
    /// This is thread-safe and blocks until the listener is created.
    /// The Transport's event loop will process the request and return the
    /// listener ID and actual bound address.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::listen()`] directly for better performance.
    pub fn listen<A: std::net::ToSocketAddrs>(
        &self,
        addr: A,
    ) -> Result<(usize, SocketAddr), Error> {
        self.transport_interface.listen(addr)
    }

    /// Opens a new connection to the specified address.
    ///
    /// This is thread-safe and blocks until the connection is established.
    /// The Transport's event loop will process the request and return the
    /// connection ID and peer's socket address.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::connect()`] directly for better performance.
    pub fn connect<A: std::net::ToSocketAddrs>(
        &self,
        addr: A,
    ) -> Result<(usize, SocketAddr), Error> {
        self.transport_interface.connect(addr)
    }

    /// Gets the local socket addresses of all active listeners.
    ///
    /// This is thread-safe and blocks until the addresses are retrieved.
    /// The Transport's event loop will process the request and return the addresses.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::get_listener_addresses()`] directly for better performance.
    pub fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        self.transport_interface.get_listener_addresses()
    }

    /// Queues a connection to be closed.
    ///
    /// This is thread-safe and non-blocking. The connection will be closed when
    /// the Transport's event loop processes the request.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::close_connection()`] directly for better performance.
    ///
    /// **Note:** This does not trigger a `MessengerEvent::Disconnected` event.
    pub fn close_connection(&self, id: usize) {
        self.transport_interface.close_connection(id);
    }

    /// Queues a listener to be closed.
    ///
    /// This is thread-safe and non-blocking. The listener will be closed when
    /// the Transport's event loop processes the request.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::close_listener()`] directly for better performance.
    pub fn close_listener(&self, id: usize) {
        self.transport_interface.close_listener(id);
    }

    /// Queues all connections and listeners to be closed.
    ///
    /// This is thread-safe and non-blocking. The listener will be closed when
    /// the Transport's event loop processes the request.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::close_all()`] directly for better performance.
    ///
    /// **Note:** This does not trigger `MessengerEvent::Disconnected` events.
    /// However, it will trigger a `MessengerEvent::Inactive` event if no new
    /// connections or listeners are created before calling [`Messenger::fetch_events()`](super::Messenger::fetch_events).
    pub fn close_all(&self) {
        self.transport_interface.close_all();
    }

    // ============================================================================
    // Data Operations
    // ============================================================================

    /// Queues a message to be sent to a specific connection.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::send_to()`] directly for better performance.
    pub fn send_to(&self, id: usize, msg: &dyn Message) {
        let data = serialize_message(msg, &self.registry);
        self.transport_interface.send_to(id, data);
    }

    /// Queues a message to be sent to multiple specific connections.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::send_to_many()`] directly for better performance.
    pub fn send_to_many(&self, ids: Vec<usize>, msg: &dyn Message) {
        let data = serialize_message(msg, &self.registry);
        self.transport_interface.send_to_many(ids, data);
    }

    /// Queues a message to be broadcast to all connected clients.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call [`super::Messenger::broadcast()`]
    /// directly for better performance.
    pub fn broadcast(&self, msg: &dyn Message) {
        let data = serialize_message(msg, &self.registry);
        self.transport_interface.broadcast(data);
    }

    /// Queues a message to be broadcast to all connected clients except one.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::broadcast_except()`] directly for better performance.
    pub fn broadcast_except(&self, msg: &dyn Message, except_id: usize) {
        let data = serialize_message(msg, &self.registry);
        self.transport_interface.broadcast_except(data, except_id);
    }

    /// Queues a message to be broadcast to all connected clients except
    /// multiple specified ones.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call
    /// [`super::Messenger::broadcast_except_many()`] directly for better performance.
    pub fn broadcast_except_many(&self, msg: &dyn Message, except_ids: Vec<usize>) {
        let data = serialize_message(msg, &self.registry);
        self.transport_interface
            .broadcast_except_many(data, except_ids);
    }
}
