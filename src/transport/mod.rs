//! Transport layer for abstracting over different transport implementations.
//!
//! This module provides the [`Transport`] struct which selects between TCP and
//! TLS transports based on configuration.

mod interface;
#[cfg(feature = "quic")]
mod quic;
mod tcp;
#[cfg(feature = "tls")]
mod tls;
#[cfg(any(feature = "tls", feature = "quic"))]
mod tls_config;

use interface::SendRequest;
pub use interface::TransportInterface;
#[cfg(feature = "quic")]
use quic::QuicTransport;
use tcp::TcpTransport;
#[cfg(feature = "tls")]
use tls::TlsTransport;

use crate::error::Error;
use config::Config;
use std::net::{Shutdown, SocketAddr, ToSocketAddrs};

// Internal transport trait for network communication.
//
// This trait abstracts over different transport implementations (TCP, TLS, etc.)
// providing a common interface for connection management, data transmission,
// and event handling.
//
// Note: This trait is internal. Users should use the `Transport` struct instead.
trait TransportImpl: Send {
    // ============================================================================
    // Connection Management
    // ============================================================================

    // We cannot use ToSocketAddrs like with Transport::connect/listen, because
    // this would make this function generic, and thereby not dyn-compatible.
    fn connect_impl(&mut self, addr: SocketAddr) -> Result<(usize, SocketAddr), Error>;
    fn listen_impl(&mut self, addr: SocketAddr) -> Result<(usize, SocketAddr), Error>;

    fn get_listener_addresses(&self) -> Vec<SocketAddr>;

    fn close_connection(&mut self, id: usize);
    fn close_all_connections(&mut self);
    fn shutdown_connection(&mut self, id: usize, how: Shutdown);
    fn shutdown_all_connections(&mut self, how: Shutdown);
    fn close_listener(&mut self, id: usize);
    fn close_all_listeners(&mut self);
    fn close_all(&mut self);

    // ============================================================================
    // Data Operations
    // ============================================================================

    fn send_to(&mut self, id: usize, buf: Vec<u8>);
    fn send_to_many(&mut self, ids: &[usize], buf: Vec<u8>);
    fn broadcast(&mut self, buf: Vec<u8>);
    fn broadcast_except(&mut self, buf: Vec<u8>, except_id: usize);
    fn broadcast_except_many(&mut self, buf: Vec<u8>, except_ids: &[usize]);

    // ============================================================================
    // Event Operations
    // ============================================================================

    fn fetch_events(&mut self) -> Result<Vec<TransportEvent>, Error>;

    // ============================================================================
    // Utilities
    // ============================================================================

    fn get_transport_interface(&self) -> TransportInterface;
}

/// Dynamic transport that wraps TCP, TLS, or QUIC transports based on configuration.
///
/// This is the main transport type users should interact with. The actual transport
/// type (TCP, TLS, or QUIC) is selected based on the `transport_type` configuration key.
///
/// # Configuration Keys
///
/// - `transport_type`: Either "tcp" or "tls" (defaults to "tcp")
/// - Transport-specific keys (e.g., `max_read_size` for TCP, `tls_server_cert` for TLS)
///
/// # Example
///
/// ```toml
/// transport_type = "tcp"
/// max_read_size = 1048576
/// ```
pub struct Transport {
    inner: Box<dyn TransportImpl>,
}

/// Events produced by [`Transport::fetch_events()`].
///
/// These events represent the lifecycle of connections and data transfer at the
/// transport layer. Handle each event to manage connection state and process
/// incoming raw data.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    /// The transport has no listeners or connections.
    Inactive,
    /// Connection established.
    Connected { id: usize },
    /// Connection establishment failed.
    ConnectionFailed { id: usize },
    /// Connection closed. Clean up state associated with this `id`.
    Disconnected { id: usize },
    /// Received raw bytes from connection.
    Data { id: usize, data: Vec<u8> },
}

// ============================================================================
// Constructors
// ============================================================================

impl Transport {
    /// Creates a new Transport based on configuration.
    ///
    /// Reads the `transport_type` configuration key to determine whether to create
    /// a TCP or TLS transport. Defaults to "tcp" if not specified.
    ///
    /// # Configuration Keys
    ///
    /// - `transport_type`: Either "tcp" or "tls"
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `transport_type` is invalid (not "tcp" or "tls")
    /// - The underlying transport creation fails
    pub fn new(config: &Config) -> Result<Self, Error> {
        Self::new_named(config, "")
    }

    /// Creates a new named Transport with configuration namespacing.
    ///
    /// Configuration lookup follows this priority:
    /// 1. `{name}.{key}` (e.g., `game_server.transport_type`)
    /// 2. `{key}` (e.g., `transport_type`)
    /// 3. Hard-coded default ("tcp")
    ///
    /// # Configuration Keys
    ///
    /// - `transport_type`: Either "tcp" or "tls"
    /// - Transport-specific keys (e.g., `max_read_size` for TCP,
    ///   `tls_server_cert` for TLS)
    ///
    /// # Example
    ///
    /// ```toml
    /// # Global default
    /// transport_type = "tcp"
    ///
    /// # Specific to "api_server" instance
    /// [api_server]
    /// transport_type = "tls"
    /// tls_server_cert = "/path/to/cert.pem"
    /// ```
    pub fn new_named(config: &Config, name: &str) -> Result<Self, Error> {
        let transport_type = if name.is_empty() {
            config
                .get_string("transport_type")
                .unwrap_or_else(|_| "tcp".to_string())
        } else {
            config
                .get_string(&format!("{}.transport_type", name))
                .or_else(|_| config.get_string("transport_type"))
                .unwrap_or_else(|_| "tcp".to_string())
        };

        let inner: Box<dyn TransportImpl> = match transport_type.as_str() {
            "tcp" => Box::new(TcpTransport::new_named(config, name)?),
            #[cfg(feature = "tls")]
            "tls" => Box::new(TlsTransport::new_named(config, name)?),
            #[cfg(feature = "quic")]
            "quic" => Box::new(QuicTransport::new_named(config, name)?),
            _ => {
                let mut valid = vec!["tcp".to_string()];
                #[cfg(feature = "tls")]
                {
                    valid.push("tls".to_string());
                }
                #[cfg(feature = "quic")]
                {
                    valid.push("quic".to_string());
                }

                return Err(Error::InvalidTransportType {
                    got: transport_type,
                    valid,
                });
            }
        };

        Ok(Self { inner })
    }

    // ============================================================================
    // Connection Management
    // ============================================================================

    /// Initiates a connection to the specified address.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// The connection is established asynchronously. You will receive a
    /// [`TransportEvent::Connected`] event when the connection is fully
    /// established, or [`TransportEvent::ConnectionFailed`] if it fails.
    ///
    /// Returns a tuple of (connection_id, socket_addr) where:
    /// - `connection_id`: Use this ID to send data to this connection.
    /// - `socket_addr`: The peer address that was connected to (useful when you
    ///   pass multiple addresses or use DNS names and want to know which
    ///   address was actually used).
    pub fn connect<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        let socket_addr = addr.to_socket_addrs()?.next().ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Could not resolve address",
            ))
        })?;
        self.inner.connect_impl(socket_addr)
    }

    /// Starts listening for incoming connections on the specified address.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Returns a tuple of (listener_id, socket_addr) where:
    /// - `listener_id`: Can be used with [`Self::close_listener`] to stop
    ///   listening. Note: This ID cannot be used for sending data - only for
    ///   closing the listener.
    /// - `socket_addr`: The actual address being listened on (useful when
    ///   binding to port 0 for dynamic allocation).
    ///
    /// Multiple listeners can be added to listen on different addresses/ports.
    pub fn listen<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        let socket_addr = addr.to_socket_addrs()?.next().ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Could not resolve address",
            ))
        })?;
        self.inner.listen_impl(socket_addr)
    }

    /// Gets the local socket addresses of all active listeners.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    pub fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        self.inner.get_listener_addresses()
    }

    /// Closes a connection by its ID.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally.
    ///
    /// **Note:** This does not trigger a [`TransportEvent::Disconnected`]
    /// event.
    pub fn close_connection(&mut self, id: usize) {
        self.inner.close_connection(id)
    }

    /// Closes all connections.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// **Note:** This does not trigger [`TransportEvent::Disconnected`] events.
    pub fn close_all_connections(&mut self) {
        self.inner.close_all_connections()
    }

    /// Shuts down a connection by its ID.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally.
    pub fn shutdown_connection(&mut self, id: usize, how: Shutdown) {
        self.inner.shutdown_connection(id, how)
    }

    /// Shuts down all connections.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    pub fn shutdown_all_connections(&mut self, how: Shutdown) {
        self.inner.shutdown_all_connections(how)
    }

    /// Closes a listener by its ID.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Ignores non-existent listener ids, because the listener might have been
    /// closed already internally.
    pub fn close_listener(&mut self, id: usize) {
        self.inner.close_listener(id)
    }

    /// Closes all listeners.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    pub fn close_all_listeners(&mut self) {
        self.inner.close_all_listeners()
    }

    /// Closes all connections and listeners.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// **Note:** This does not trigger [`TransportEvent::Disconnected`] events.
    /// However, it will trigger a [`TransportEvent::Inactive`] event if no new
    /// connections or listeners are created before calling
    /// [`Self::fetch_events`].
    pub fn close_all(&mut self) {
        self.inner.close_all()
    }

    // ============================================================================
    // Data Operations
    // ============================================================================

    /// Sends data to a specific connection.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// TransportEvents.
    pub fn send_to(&mut self, id: usize, buf: Vec<u8>) {
        self.inner.send_to(id, buf)
    }

    /// Sends data to multiple specific connections.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// TransportEvents.
    pub fn send_to_many(&mut self, ids: &[usize], buf: Vec<u8>) {
        self.inner.send_to_many(ids, buf)
    }

    /// Broadcasts data to all connected clients.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// TransportEvents.
    pub fn broadcast(&mut self, buf: Vec<u8>) {
        self.inner.broadcast(buf)
    }

    /// Broadcasts data to all connected clients except one.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// TransportEvents.
    pub fn broadcast_except(&mut self, buf: Vec<u8>, except_id: usize) {
        self.inner.broadcast_except(buf, except_id)
    }

    /// Broadcasts data to all connected clients except multiple specified ones.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally. Errors are handled asynchronously with
    /// TransportEvents.
    pub fn broadcast_except_many(&mut self, buf: Vec<u8>, except_ids: &[usize]) {
        self.inner.broadcast_except_many(buf, except_ids)
    }

    // ============================================================================
    // Event Operations
    // ============================================================================

    /// Blocks until transport events are available and returns them.
    ///
    /// Returns [`TransportEvent::Inactive`] if the [`Transport`] has no
    /// connections or listeners.
    ///
    /// Only returns unrecoverable errors. Recoverable errors are handled by the
    /// returned events.
    pub fn fetch_events(&mut self) -> Result<Vec<TransportEvent>, Error> {
        self.inner.fetch_events()
    }

    // ============================================================================
    // Utilities
    // ============================================================================

    /// Gets a thread-safe interface for sending data from other threads.
    pub fn get_transport_interface(&self) -> TransportInterface {
        self.inner.get_transport_interface()
    }
}
