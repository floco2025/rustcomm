//! Transport layer for abstracting over different transport implementations.
//!
//! This module provides the [`Transport`] struct which selects between TCP and
//! TLS transports based on configuration.
//!
//! # Design Decision: Dynamic Dispatch
//!
//! This module uses trait objects (`Box<dyn TransportImpl>`) rather than
//! generic type parameters to enable **runtime transport selection from
//! configuration files**.
//!
//! ## Why Dynamic Dispatch?
//!
//! **Runtime configuration**: Transport type is chosen at runtime based on
//! config files (e.g., `transport_type = "tcp"`). This allows the same
//! compiled binary to use different transports without recompilation.
//!
//! With generics (`Transport<T: TransportImpl>`), transport selection must
//! happen at compile time. Users would need to either:
//! - Hard-code the transport type, or
//! - Implement their own runtime selection with `Box<dyn>` or enums anyway
//!
//! ## Performance
//!
//! The vtable overhead (~1-2ns per call) is negligible compared to network
//! I/O latency (microseconds to milliseconds). We're bottlenecked by the
//! network, not function dispatch.
//!
//! ## Alternatives Considered
//!
//! - **Generic parameters**: `Transport<T: TransportImpl>`
//!   - ✅ More idiomatic Rust, better optimization potential
//!   - ❌ Requires compile-time transport selection
//!   - ❌ Users must implement runtime selection themselves
//!
//! - **Dynamic dispatch**: `Box<dyn TransportImpl>` (current choice)
//!   - ✅ Built-in runtime configuration support
//!   - ✅ Same binary works with any transport
//!   - ⚠️ Small vtable overhead (negligible for network I/O)

mod interface;
mod tcp;
#[cfg(feature = "tls")]
mod tls;

use interface::SendRequest;
pub use interface::TransportInterface;
use tcp::TcpTransport;
#[cfg(feature = "tls")]
use tls::TlsTransport;

use crate::error::Error;
use config::Config;
use std::net::{SocketAddr, ToSocketAddrs};

// Internal transport trait for network communication.
//
// This trait abstracts over different transport implementations (TCP, TLS, etc.)
// providing a common interface for connection management, data transmission,
// and event handling.
//
// Note: This trait is internal. Users should use the `Transport` struct instead.
trait TransportImpl: Send {
    // ========================================================================
    // Connection Management
    // ========================================================================

    // Starts listening for incoming connections on the specified address. We
    // cannot use ToSocketAddrs like with Transport::listen, because this would
    // make this function generic, and thereby not dyn-compatible.
    fn listen_impl(&mut self, addr: SocketAddr) -> Result<(usize, SocketAddr), Error>;

    // Initiates a connection to the specified address. We cannot use
    // ToSocketAddrs like with Transport::connect, because this would make this
    // function generic, and thereby not dyn-compatible.
    fn connect_impl(&mut self, addr: SocketAddr) -> Result<(usize, SocketAddr), Error>;

    // Closes a connection by its ID.
    fn close_connection(&mut self, id: usize);

    // Closes a listener by its ID.
    fn close_listener(&mut self, id: usize);

    // Closes all connections and listeners.
    fn close_all(&mut self);

    // ========================================================================
    // Data Operations
    // ========================================================================

    // Sends data to a specific connection.
    fn send_to(&mut self, id: usize, buf: Vec<u8>);

    // Sends data to multiple specific connections.
    fn send_to_many(&mut self, ids: &[usize], buf: Vec<u8>);

    // Broadcasts data to all connected clients.
    fn broadcast(&mut self, buf: Vec<u8>);

    // Broadcasts data to all connected clients except one.
    fn broadcast_except(&mut self, buf: Vec<u8>, except_id: usize);

    // Broadcasts data to all connected clients except multiple specified ones.
    fn broadcast_except_many(&mut self, buf: Vec<u8>, except_ids: &[usize]);

    // ========================================================================
    // Event Loop
    // ========================================================================

    // Blocks until transport events are available and returns them.
    //
    // Only returns unrecoverable errors. Recoverable errors are handled by
    // the returned events.
    fn fetch_events(&mut self) -> Result<Vec<TransportEvent>, Error>;

    // ========================================================================
    // Utilities
    // ========================================================================

    // Gets a thread-safe interface for sending data from other threads.
    fn get_transport_interface(&self) -> TransportInterface;

    // Gets the local socket addresses of all active listeners.
    fn get_listener_addresses(&self) -> Vec<SocketAddr>;
}

/// Dynamic transport that wraps either TCP or TLS transport based on configuration.
///
/// This is the main transport type users should interact with. The actual transport
/// type (TCP or TLS) is selected based on the `transport_type` configuration key.
///
/// # Configuration Keys
///
/// - `transport_type`: Either "tcp" or "tls" (defaults to "tcp")
/// - Plus all keys for the specific transport type (see `TcpTransport` and `TlsTransport`)
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

/// Events produced by [`Transport::fetch_events()`]
///
/// # Variants
///
/// - `Inactive` - The transport has no listeners or connections.
/// - `Connected { id }` - Connection established.
/// - `ConnectionFailed { id }` - Connection establishment failed.
/// - `Disconnected { id }` - Connection closed. Clean up state associated with this `id`.
/// - `Data { id, data }` - Received raw bytes from connection.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    Inactive,
    Connected { id: usize },
    ConnectionFailed { id: usize },
    Disconnected { id: usize },
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
    /// - Plus all keys for the specific transport type
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
            _ => {
                #[cfg(feature = "tls")]
                let valid = vec!["tcp".to_string(), "tls".to_string()];
                #[cfg(not(feature = "tls"))]
                let valid = vec!["tcp".to_string()];
                
                return Err(Error::InvalidTransportType {
                    got: transport_type,
                    valid,
                })
            }
        };

        Ok(Self { inner })
    }

    // ========================================================================
    // Connection Management
    // ========================================================================

    /// Starts listening for incoming connections on the specified address.
    ///
    /// Returns a tuple of (listener_id, socket_addr) where:
    /// - `listener_id`: Can be used with
    ///   [`close_listener()`](Self::close_listener) to stop listening. Note:
    ///   This ID cannot be used for sending data - only for closing the
    ///   listener.
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

    /// Initiates a connection to the specified address.
    ///
    /// The connection is established asynchronously. You will receive a
    /// `TransportEvent::Connected` event when the connection is fully
    /// established, or `TransportEvent::ConnectionFailed` if it fails.
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

    /// Closes a connection by its ID.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// Ignores non-existent connection ids, because the connection might have
    /// been closed already internally.
    ///
    /// **Note:** This does not trigger a `TransportEvent::Disconnected` event.
    pub fn close_connection(&mut self, id: usize) {
        self.inner.close_connection(id)
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

    /// Closes all connections and listeners.
    ///
    /// **Not thread-safe.** For multi-threaded use, call this method on
    /// [`TransportInterface`] instead.
    ///
    /// **Note:** This does not trigger `TransportEvent::Disconnected` events.
    /// However, it will trigger a `TransportEvent::Inactive` event if no new
    /// connections or listeners are created before calling
    /// [`fetch_events()`](Self::fetch_events).
    pub fn close_all(&mut self) {
        self.inner.close_all()
    }

    // ========================================================================
    // Data Operations
    // ========================================================================

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

    // ========================================================================
    // Event Operations
    // ========================================================================

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

    // ========================================================================
    // Utilities
    // ========================================================================

    /// Gets a thread-safe interface for sending data from other threads.
    pub fn get_transport_interface(&self) -> TransportInterface {
        self.inner.get_transport_interface()
    }

    /// Gets the local socket addresses of all active listeners.
    pub fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        self.inner.get_listener_addresses()
    }
}
