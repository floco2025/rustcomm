//! TLS transport implementation.
//!
//! Provides a non-blocking TLS transport layer using rustls and mio for
//! event-driven I/O. Supports multiple simultaneous secure connections and
//! listeners with a single-threaded event loop.

use super::tls_config::{load_tls_client_config, load_tls_server_config};
use super::*;
use crate::config::{get_namespaced_string, get_namespaced_usize};
use crate::error::Error;
use ::config::Config;

use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token, Waker};
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{
    mpsc::{channel, Receiver, Sender},
    Arc,
};
use tracing::{debug, error, info, instrument, trace, warn};

// Internal constants for connection management
const WAKE_ID: usize = 2;
const CONNECTION_ID_RANGE_START: usize = 1000;

// Internal enum for TLS connection type
enum TlsConnection {
    Server(rustls::ServerConnection),
    Client(rustls::ClientConnection),
}

// Internal connection status
#[derive(Debug, Clone, Copy, PartialEq)]
enum ConnectionStatus {
    NotConnected,
    TcpConnected,
    TlsConnected,
}

// Internal data type for connection management
struct Connection {
    stream: TcpStream,
    tls_conn: TlsConnection,
    connected: ConnectionStatus,
    interest: Interest,
    local_addr: SocketAddr,
    peer_addr: SocketAddr,
    send_buf: Vec<u8>,
}

impl Connection {
    fn read_tls(&mut self) -> Result<usize, std::io::Error> {
        match &mut self.tls_conn {
            TlsConnection::Server(conn) => conn.read_tls(&mut self.stream),
            TlsConnection::Client(conn) => conn.read_tls(&mut self.stream),
        }
    }

    fn write_tls(&mut self) -> Result<usize, std::io::Error> {
        match &mut self.tls_conn {
            TlsConnection::Server(conn) => conn.write_tls(&mut self.stream),
            TlsConnection::Client(conn) => conn.write_tls(&mut self.stream),
        }
    }

    fn process_new_packets(&mut self) -> Result<rustls::IoState, rustls::Error> {
        match &mut self.tls_conn {
            TlsConnection::Server(conn) => conn.process_new_packets(),
            TlsConnection::Client(conn) => conn.process_new_packets(),
        }
    }

    fn wants_write(&self) -> bool {
        match &self.tls_conn {
            TlsConnection::Server(conn) => conn.wants_write(),
            TlsConnection::Client(conn) => conn.wants_write(),
        }
    }

    fn writer(&mut self) -> rustls::Writer<'_> {
        match &mut self.tls_conn {
            TlsConnection::Server(conn) => conn.writer(),
            TlsConnection::Client(conn) => conn.writer(),
        }
    }

    fn reader(&mut self) -> rustls::Reader<'_> {
        match &mut self.tls_conn {
            TlsConnection::Server(conn) => conn.reader(),
            TlsConnection::Client(conn) => conn.reader(),
        }
    }

    fn is_handshaking(&self) -> bool {
        match &self.tls_conn {
            TlsConnection::Server(conn) => conn.is_handshaking(),
            TlsConnection::Client(conn) => conn.is_handshaking(),
        }
    }
}

// Internal data type for read_connection
enum ReadConnectionResult {
    Ok(Vec<u8>),
    Connected(Vec<u8>),
    Disconnected(Vec<u8>),
}

// Internal data type for write_connection
enum WriteConnectionResult {
    Ok,
    Connected,
    Disconnected,
    ConnectionFailed,
}

/// Non-blocking TCP transport handler for client-server communication with TLS support.
///
/// Not thread-safe - use TransportInterface for cross-thread communication.
///
/// Note: This struct is internal. Users should use the `Transport` struct instead.
pub(super) struct TlsTransport {
    connections: HashMap<usize, Connection>,
    listeners: HashMap<usize, TcpListener>,
    next_id: usize,
    poll: Poll,
    poll_capacity: usize,
    waker: Arc<Waker>,
    sender: Sender<SendRequest>,
    receiver: Receiver<SendRequest>,
    spurious_wakeups: usize,
    max_read_size: usize,
    max_spurious_wakeups: u32,
    tls_client_config: Option<Arc<rustls::ClientConfig>>,
    tls_server_config: Option<Arc<rustls::ServerConfig>>,
    tls_server_name: Option<String>,
}

// ============================================================================
// Constructors
// ============================================================================

impl TlsTransport {
    /// Creates a new named TlsTransport instance with configuration namespacing.
    pub fn new_named(config: &Config, name: &str) -> Result<Self, Error> {
        let max_read_size =
            get_namespaced_usize(config, name, "max_read_size").unwrap_or(1024 * 1024);

        let poll_capacity =
            get_namespaced_usize(config, name, "poll_capacity").unwrap_or(DEFAULT_POLL_CAPACITY);

        let poll = Poll::new()?;
        let waker = Arc::new(Waker::new(poll.registry(), Token(WAKE_ID))?);
        let (sender, receiver) = channel();

        const MAX_SPURIOUS_WAKEUPS: u32 = 10;

        // Load TLS server config from paths if both cert and key are provided
        let tls_server_config = if let (Ok(cert_path), Ok(key_path)) = (
            get_namespaced_string(config, name, "tls_server_cert"),
            get_namespaced_string(config, name, "tls_server_key"),
        ) {
            Some(Arc::new(load_tls_server_config(&cert_path, &key_path)?))
        } else {
            None
        };

        // Load TLS client config from CA cert path if provided
        let tls_client_config =
            if let Ok(ca_cert_path) = get_namespaced_string(config, name, "tls_ca_cert") {
                Some(Arc::new(load_tls_client_config(&ca_cert_path)?))
            } else {
                None
            };

        // Optional override for the TLS server name/SNI used during connect
        let tls_server_name = match get_namespaced_string(config, name, "tls_server_name") {
            Ok(name) => Some(name),
            Err(config::ConfigError::NotFound(_)) => None,
            Err(err) => return Err(err.into()),
        };

        Ok(Self {
            connections: HashMap::new(),
            listeners: HashMap::new(),
            next_id: CONNECTION_ID_RANGE_START,
            poll,
            poll_capacity,
            waker,
            sender,
            receiver,
            spurious_wakeups: 0,
            max_read_size,
            max_spurious_wakeups: MAX_SPURIOUS_WAKEUPS,
            tls_client_config,
            tls_server_config,
            tls_server_name,
        })
    }
}

// ============================================================================
// Connection Management
// ============================================================================

impl TlsTransport {
    /// Initiates a connection to the specified address.
    #[instrument(skip(self, addr))]
    pub fn connect<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        // Fail if TLS client config wasn't loaded
        if self.tls_client_config.is_none() {
            return Err(Error::TlsClientConfigMissing);
        }

        let peer_addr = addr
            .to_socket_addrs()?
            .next()
            .expect("Address resolution returned empty iterator");
        let mut stream = TcpStream::connect(peer_addr)?;
        stream.set_nodelay(true)?;

        let connection_id = self.next_id;
        let local_addr = stream.local_addr().expect("Failed to get local address");
        info!(id = connection_id, %local_addr, %peer_addr, "Initiating connection");

        // Create TLS client connection with configurable SNI
        let server_name_value = self
            .tls_server_name
            .clone()
            .unwrap_or_else(|| "localhost".to_string());
        let server_name_for_err = server_name_value.clone();
        let server_name = ServerName::try_from(server_name_value)
            .map_err(|_| Error::TlsInvalidServerName(server_name_for_err))?;
        let tls_conn = rustls::ClientConnection::new(
            self.tls_client_config.as_ref().unwrap().clone(),
            server_name,
        )
        .map_err(|e| Error::TlsClientConfigBuild(e.to_string()))?;

        let interest = Interest::WRITABLE;
        self.poll
            .registry()
            .register(&mut stream, Token(connection_id), interest)
            .expect("Failed to register connection");
        self.connections.insert(
            connection_id,
            Connection {
                stream,
                tls_conn: TlsConnection::Client(tls_conn),
                // Connections from connect() are not yet TCP established
                connected: ConnectionStatus::NotConnected,
                interest,
                local_addr,
                peer_addr,
                send_buf: Vec::new(),
            },
        );

        self.advance_connection_id();

        Ok((connection_id, peer_addr))
    }

    /// Starts listening for incoming connections on the specified address.
    #[instrument(skip(self, addr))]
    pub fn listen<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        // Fail if TLS server config wasn't loaded
        if self.tls_server_config.is_none() {
            return Err(Error::TlsServerConfigMissing);
        }

        let requested_addr = addr
            .to_socket_addrs()?
            .next()
            .expect("Address resolution returned empty iterator");
        let mut listener = TcpListener::bind(requested_addr)?;

        let listener_id = self.next_id;
        let local_addr = listener.local_addr().expect("Failed to get local address");
        info!(id = listener_id, %local_addr, "Listening for connections");
        self.poll
            .registry()
            .register(&mut listener, Token(listener_id), Interest::READABLE)
            .expect("Failed to register listener");
        self.listeners.insert(listener_id, listener);

        self.advance_connection_id();

        Ok((listener_id, local_addr))
    }

    /// Gets the local socket addresses of all active listeners.
    pub fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        self.listeners
            .values()
            .map(|listener| {
                listener
                    .local_addr()
                    .expect("Failed to get listener local address")
            })
            .collect()
    }

    /// Closes a connection by its ID.
    #[instrument(skip(self))]
    pub fn close_connection(&mut self, id: usize) {
        match self.connections.remove(&id) {
            Some(mut connection) => {
                self.poll
                    .registry()
                    .deregister(&mut connection.stream)
                    .expect("Failed to deregister connection");
                let local_addr = &connection.local_addr;
                let peer_addr = &connection.peer_addr;
                info!(id, %local_addr, %peer_addr, "Closed connection");
            }
            None => {
                warn!(id, "Connection not found when closing connection");
            }
        }
    }

    /// Closes all connections.
    #[instrument(skip(self))]
    pub fn close_all_connections(&mut self) {
        for (id, mut connection) in self.connections.drain() {
            self.poll
                .registry()
                .deregister(&mut connection.stream)
                .expect("Failed to deregister connection");
            let local_addr = &connection.local_addr;
            let peer_addr = &connection.peer_addr;
            info!(id, %local_addr, %peer_addr, "Closed connection");
        }
    }

    /// Shuts down a connection by its ID.
    #[instrument(skip(self))]
    pub fn shutdown_connection(&mut self, id: usize, how: Shutdown) {
        match self.connections.get(&id) {
            Some(connection) => {
                let local_addr = &connection.local_addr;
                let peer_addr = &connection.peer_addr;
                match connection.stream.shutdown(how) {
                    Ok(_) => {
                        info!(id, how = ?how, %local_addr, %peer_addr, "Shut down connection");
                    }
                    Err(err) => {
                        warn!(id, how = ?how, %local_addr, %peer_addr, ?err, "Error shutting down connection");
                    }
                }
            }
            None => {
                warn!(id, "Connection not found when shutting down connection");
            }
        }
    }

    /// Shuts down all connections.
    #[instrument(skip(self))]
    pub fn shutdown_all_connections(&mut self, how: Shutdown) {
        for (id, connection) in self.connections.iter() {
            let local_addr = &connection.local_addr;
            let peer_addr = &connection.peer_addr;
            match connection.stream.shutdown(how) {
                Ok(_) => {
                    info!(id, how = ?how, %local_addr, %peer_addr, "Shut down connection");
                }
                Err(err) => {
                    warn!(id, how = ?how, %local_addr, %peer_addr, ?err, "Error shutting down connection");
                }
            }
        }
    }

    /// Closes a listener by its ID.
    #[instrument(skip(self))]
    pub fn close_listener(&mut self, id: usize) {
        match self.listeners.remove(&id) {
            Some(mut listener) => {
                self.poll
                    .registry()
                    .deregister(&mut listener)
                    .expect("Failed to deregister listener");
                let local_addr = listener.local_addr().expect("Failed to get local address");
                info!(id, %local_addr, "Closed listener");
            }
            None => {
                warn!(id, "Listener not found when closing listener");
            }
        }
    }

    /// Closes all listeners.
    #[instrument(skip(self))]
    pub fn close_all_listeners(&mut self) {
        for (id, mut listener) in self.listeners.drain() {
            self.poll
                .registry()
                .deregister(&mut listener)
                .expect("Failed to deregister listener");
            let local_addr = listener.local_addr().expect("Failed to get local address");
            info!(id, %local_addr, "Closed listener");
        }
    }

    /// Closes all listeners and connections.
    #[instrument(skip(self))]
    pub fn close_all(&mut self) {
        self.close_all_listeners();
        self.close_all_connections();
    }
}

// ============================================================================
// Data Operations
// ============================================================================

impl TlsTransport {
    /// Sends data to a specific connection.
    #[instrument(skip(self, buf))]
    pub fn send_to(&mut self, to_id: usize, buf: Vec<u8>) {
        debug!(len = buf.len(), "Sending data");
        self.queue_data(to_id, buf);
    }

    /// Sends data to multiple specific connections.
    #[instrument(skip(self, buf, to_ids))]
    pub fn send_to_many(&mut self, to_ids: &[usize], buf: Vec<u8>) {
        debug!(
            count = to_ids.len(),
            len = buf.len(),
            "Sending data to many"
        );
        if let Some((&last_id, rest)) = to_ids.split_last() {
            for &to_id in rest {
                self.queue_data(to_id, buf.clone());
            }
            self.queue_data(last_id, buf);
        }
    }

    /// Broadcasts data to all connected clients.
    #[instrument(skip(self, buf))]
    pub fn broadcast(&mut self, buf: Vec<u8>) {
        debug!(len = buf.len(), "Broadcasting data");
        let to_ids: Vec<_> = self.connections.keys().copied().collect();
        if let Some((last_id, rest)) = to_ids.split_last() {
            for &to_id in rest {
                self.queue_data(to_id, buf.clone());
            }
            self.queue_data(*last_id, buf);
        }
    }

    /// Broadcasts data to all connected clients except one.
    #[instrument(skip(self, buf))]
    pub fn broadcast_except(&mut self, buf: Vec<u8>, except_id: usize) {
        let to_ids: Vec<_> = self
            .connections
            .keys()
            .copied()
            .filter(|&id| id != except_id)
            .collect();
        debug!(len = buf.len(), "Broadcasting data with exception");
        if let Some((last_id, rest)) = to_ids.split_last() {
            for &to_id in rest {
                self.queue_data(to_id, buf.clone());
            }
            self.queue_data(*last_id, buf);
        }
    }

    /// Broadcasts data to all connected clients except multiple specified ones.
    #[instrument(skip(self, buf, except_ids))]
    pub fn broadcast_except_many(&mut self, buf: Vec<u8>, except_ids: &[usize]) {
        let to_ids: Vec<_> = self
            .connections
            .keys()
            .copied()
            .filter(|id| !except_ids.contains(id))
            .collect();
        debug!(
            except_count = except_ids.len(),
            len = buf.len(),
            "Broadcasting data with many exceptions"
        );
        if let Some((last_id, rest)) = to_ids.split_last() {
            for &to_id in rest {
                self.queue_data(to_id, buf.clone());
            }
            self.queue_data(*last_id, buf);
        }
    }
}

// ============================================================================
// Event Operations
// ============================================================================

impl TlsTransport {
    /// Blocks until transport events are available and returns them.
    #[instrument(skip(self))]
    pub fn fetch_events(&mut self) -> Result<Vec<TransportEvent>, Error> {
        let mut dispatch_events = Vec::new();

        while dispatch_events.is_empty() {
            // Process queued transport interface events
            self.process_interface_requests();

            // Is there anything to do?
            if self.connections.is_empty() && self.listeners.is_empty() {
                dispatch_events.push(TransportEvent::Inactive);
                return Ok(dispatch_events);
            }

            let mut poll_events = Events::with_capacity(self.poll_capacity);
            self.poll.poll(&mut poll_events, None)?;

            // Track disconnected connections in this poll cycle to avoid processing
            // their remaining events. Only needed to keep the assert!() in the
            // if/else chain below valid. Ccan be removed later once connection
            // cleanup is proven bug-free.
            let mut disconnected_ids = std::collections::HashSet::new();

            for event in poll_events.iter() {
                let Token(id) = event.token();

                if id == WAKE_ID {
                    // Nothing to do
                } else if self.listeners.contains_key(&id) {
                    self.accept_connections(id)?;
                    // In contrast to TCP, we do not add a connected event after
                    // accepting, because the connection is only considered to be
                    // established when the TLS handshake is complete.
                } else if disconnected_ids.contains(&id) {
                    continue;
                } else {
                    assert!(
                        self.connections.contains_key(&id),
                        "Connection {} not found - was it properly removed after disconnect?",
                        id
                    );

                    assert!(event.is_readable() || event.is_writable());
                    // mio reports errors alongside readable/writable bits, so
                    // we intentionally skip event.is_error() here and let the
                    // actual read/write attempt surface specific failures.

                    if event.is_readable() {
                        match self.read_connection(id) {
                            ReadConnectionResult::Ok(data) => {
                                if !data.is_empty() {
                                    dispatch_events.push(TransportEvent::Data { id, data });
                                }
                            }
                            ReadConnectionResult::Connected(data) => {
                                if !data.is_empty() {
                                    dispatch_events.push(TransportEvent::Data { id, data });
                                }
                                dispatch_events.push(TransportEvent::Connected { id });
                            }
                            ReadConnectionResult::Disconnected(data) => {
                                if !data.is_empty() {
                                    dispatch_events.push(TransportEvent::Data { id, data });
                                }
                                dispatch_events.push(TransportEvent::Disconnected { id });
                                disconnected_ids.insert(id);
                                continue; // Skip writable check for disconnected connections
                            }
                        }
                    }

                    if event.is_writable() {
                        match self.write_connection(id) {
                            WriteConnectionResult::Ok => (),
                            WriteConnectionResult::Connected => {
                                dispatch_events.push(TransportEvent::Connected { id })
                            }
                            WriteConnectionResult::Disconnected => {
                                dispatch_events.push(TransportEvent::Disconnected { id });
                                disconnected_ids.insert(id);
                            }
                            WriteConnectionResult::ConnectionFailed => {
                                dispatch_events.push(TransportEvent::ConnectionFailed { id });
                                disconnected_ids.insert(id);
                            }
                        }
                    }
                }
            }
        }

        debug!(count = dispatch_events.len(), "Fetched events");
        Ok(dispatch_events)
    }
}

// ============================================================================
// Utilities
// ============================================================================

impl TlsTransport {
    /// Gets a thread-safe interface for sending data from other threads.
    pub fn get_transport_interface(&self) -> TransportInterface {
        TransportInterface {
            sender: self.sender.clone(),
            waker: self.waker.clone(),
        }
    }
}

// ============================================================================
// Internal Event Processing
// ============================================================================

impl TlsTransport {
    fn process_interface_requests(&mut self) {
        let send_requests: Vec<SendRequest> = self.receiver.try_iter().collect();

        for request in send_requests {
            match request {
                SendRequest::Connect { addr, response } => {
                    let result = self.connect(addr);
                    if let Err(e) = response.send(result) {
                        error!("Failed to send connect response: {:?}", e);
                    }
                }
                SendRequest::Listen { addr, response } => {
                    let result = self.listen(addr);
                    if let Err(e) = response.send(result) {
                        error!("Failed to send listen response: {:?}", e);
                    }
                }
                SendRequest::GetListenerAddresses { response } => {
                    let addresses = self.get_listener_addresses();
                    if let Err(e) = response.send(addresses) {
                        error!("Failed to send listener addresses response: {:?}", e);
                    }
                }
                SendRequest::CloseConnection { id } => self.close_connection(id),
                SendRequest::CloseAllConnections => self.close_all_connections(),
                SendRequest::ShutdownConnection { id, how } => self.shutdown_connection(id, how),
                SendRequest::ShutdownAllConnections { how } => self.shutdown_all_connections(how),
                SendRequest::CloseListener { id } => self.close_listener(id),
                SendRequest::CloseAllListeners => self.close_all_listeners(),
                SendRequest::CloseAll => self.close_all(),
                SendRequest::SendTo { id, data } => self.send_to(id, data),
                SendRequest::SendToMany { ids, data } => self.send_to_many(&ids, data),
                SendRequest::Broadcast { data } => self.broadcast(data),
                SendRequest::BroadcastExcept { data, except_id } => {
                    self.broadcast_except(data, except_id)
                }
                SendRequest::BroadcastExceptMany { data, except_ids } => {
                    self.broadcast_except_many(data, &except_ids)
                }
                SendRequest::SupportsMultiStream { response } => {
                    let _ = response.send(self.supports_multi_stream());
                }
                SendRequest::OpenStream {
                    connection_id,
                    response,
                } => {
                    let result = self.open_stream(connection_id);
                    let _ = response.send(result);
                }
                SendRequest::CloseStream { stream_id } => self.close_stream(stream_id),
            }
        }
    }
}

// ============================================================================
// Internal Connection I/O
// ============================================================================

impl TlsTransport {
    #[instrument(skip(self))]
    fn accept_connections(&mut self, id: usize) -> Result<Vec<usize>, Error> {
        let mut new_conn_ids = Vec::new();

        let listener = self
            .listeners
            .get_mut(&id)
            .expect("Listener should exist for accept event");

        // We first collect all accepted connections and then add them, to avoid
        // borrow checker issues.
        let mut new_streams = Vec::new();
        loop {
            match listener.accept() {
                Ok((stream, ..)) => {
                    stream.set_nodelay(true)?;
                    new_streams.push(stream);
                }
                Err(err) => match err.kind() {
                    ErrorKind::WouldBlock => {
                        // Further accepting would block, so we are done
                        break;
                    }
                    ErrorKind::Interrupted => continue,
                    ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset => {
                        let local_addr =
                            listener.local_addr().expect("Failed to get local address");
                        warn!(?err, %local_addr, "Transient accept error");
                        continue;
                    }
                    _ => {
                        let local_addr =
                            listener.local_addr().expect("Failed to get local address");
                        error!(?err, %local_addr, "Error accepting connection");
                        self.poll
                            .registry()
                            .deregister(listener)
                            .expect("Failed to deregister listener");
                        self.listeners.remove(&id);
                        return Err(err.into());
                    }
                },
            }
        }

        let accepted_count = new_streams.len();

        // Now register and store all the accepted connections
        for mut stream in new_streams {
            let local_addr = stream.local_addr().expect("Failed to get local address");
            let peer_addr = stream.peer_addr().expect("Failed to get peer address");
            info!(id = self.next_id, %local_addr, %peer_addr, "Accepting connection");

            // Create TLS server connection
            let tls_conn =
                rustls::ServerConnection::new(self.tls_server_config.as_ref().unwrap().clone())
                    .map_err(|e| Error::TlsServerConfigBuild(e.to_string()))?;

            let interest = Interest::READABLE;
            self.poll
                .registry()
                .register(&mut stream, Token(self.next_id), interest)
                .expect("Failed to register connection");
            self.connections.insert(
                self.next_id,
                Connection {
                    stream,
                    tls_conn: TlsConnection::Server(tls_conn),
                    // Connections from accept() are TCP established
                    connected: ConnectionStatus::TcpConnected,
                    interest,
                    local_addr,
                    peer_addr,
                    send_buf: Vec::new(),
                },
            );

            new_conn_ids.push(self.next_id);
            self.advance_connection_id();
        }

        // If no connections were accepted, we have a spurious wakeup
        self.track_spurious_wakeup(accepted_count == 0);

        Ok(new_conn_ids)
    }

    #[instrument(skip(self))]
    fn read_connection(&mut self, id: usize) -> ReadConnectionResult {
        let conn = self
            .connections
            .get_mut(&id)
            .expect("Connection should exist for readable event");
        // We shouldn't be here if the TCP connection process it not complete
        assert_ne!(conn.connected, ConnectionStatus::NotConnected);

        let local_addr = conn.local_addr;
        let peer_addr = conn.peer_addr;
        let mut recv_buf = Vec::new();
        let mut recv_pos: usize = 0;
        let mut disconnect = false;
        let old_interest = conn.interest;
        let mut spurious = true;

        while !disconnect {
            // Read TLS data from socket
            match conn.read_tls() {
                Ok(0) => {
                    info!(%local_addr, %peer_addr, "Connection closed");
                    disconnect = true;
                    break;
                }
                Ok(sz) => {
                    trace!(len = sz, %local_addr, %peer_addr, "Read encrypted data from socket");
                    spurious = false;
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    // Further reading would block, so we are done
                    break;
                }
                Err(err) => {
                    if err.kind() == ErrorKind::BrokenPipe {
                        warn!(%local_addr, %peer_addr, "Broken pipe");
                    } else if err.kind() == ErrorKind::ConnectionReset {
                        warn!(%local_addr, %peer_addr, "Connection reset");
                    } else {
                        error!(%local_addr, %peer_addr, ?err, "Error reading from socket");
                    }
                    disconnect = true;
                    break;
                }
            }

            // Process TLS packets (decrypt)
            if let Err(err) = conn.process_new_packets() {
                error!(%local_addr, %peer_addr, ?err, "TLS error processing packets");
                disconnect = true;
                break;
            }

            // Read available decrypted application data
            recv_buf.resize(recv_pos + self.max_read_size, 0);
            let recv_buf_slice = &mut recv_buf[recv_pos..];

            match conn.reader().read(recv_buf_slice) {
                Ok(0) => {
                    // No plaintext data available
                }
                Ok(sz) => {
                    trace!(len = sz, %local_addr, %peer_addr, "Read plaintext from TLS");
                    recv_pos += sz;
                    spurious = false;
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    // No plaintext data available
                }
                Err(err) => {
                    error!(%local_addr, %peer_addr, ?err, "Error reading plaintext from TLS");
                    disconnect = true;
                    break;
                }
            }
        }

        // Remove the data we sent
        recv_buf.truncate(recv_pos);
        if !recv_buf.is_empty() {
            debug!(len = recv_buf.len(), %local_addr, %peer_addr, "Received data");
        }

        if disconnect {
            let connected = conn.connected;
            self.poll
                .registry()
                .deregister(&mut conn.stream)
                .expect("Failed to deregister connection");
            self.connections.remove(&id);
            if connected == ConnectionStatus::TcpConnected {
                // A disconnect while we are only TcpConnected means that the
                // TLS handshake failed. In this case we do not want a
                // Disconnected event, because there never was a Connected event
                // to match.
                warn!(%local_addr, %peer_addr, "TLS handshake failed");
                assert!(recv_buf.is_empty());
                ReadConnectionResult::Ok(recv_buf)
            } else {
                ReadConnectionResult::Disconnected(recv_buf)
            }
        } else {
            let result;
            conn.interest = Interest::READABLE;
            if conn.connected == ConnectionStatus::TcpConnected {
                if conn.is_handshaking() {
                    // We are still handshaking
                    assert!(recv_buf.is_empty());
                    result = ReadConnectionResult::Ok(recv_buf);
                    if conn.wants_write() {
                        conn.interest |= Interest::WRITABLE;
                    }
                } else {
                    // We completed the handshake, so we must return Connected
                    conn.connected = ConnectionStatus::TlsConnected;
                    info!(%local_addr, %peer_addr, "TLS handshake completed");
                    result = ReadConnectionResult::Connected(recv_buf);
                    if conn.wants_write() || !conn.send_buf.is_empty() {
                        conn.interest |= Interest::WRITABLE;
                    }
                }
            } else {
                // We are fully connected
                result = ReadConnectionResult::Ok(recv_buf);
                if conn.wants_write() || !conn.send_buf.is_empty() {
                    conn.interest |= Interest::WRITABLE;
                }
            }

            // Update our registration, if necessary
            if old_interest != conn.interest {
                self.poll
                    .registry()
                    .reregister(&mut conn.stream, Token(id), conn.interest)
                    .expect("Failed to reregister connection");
            }

            self.track_spurious_wakeup(spurious);

            result
        }
    }

    #[instrument(skip(self))]
    fn write_connection(&mut self, id: usize) -> WriteConnectionResult {
        let conn = self
            .connections
            .get_mut(&id)
            .expect("Connection should exist for writable event");
        let local_addr = conn.local_addr;
        let peer_addr = conn.peer_addr;
        let old_interest = conn.interest;
        let mut spurious = true;

        // Check if the TCP connection process is not yet complete
        if conn.connected == ConnectionStatus::NotConnected {
            let result = conn.stream.take_error().expect("Failed to take error");
            match result {
                None => {
                    info!(%local_addr, %peer_addr, "TCP connection established");
                    conn.connected = ConnectionStatus::TcpConnected;
                    spurious = false;
                }
                Some(err) if err.kind() == ErrorKind::ConnectionRefused => {
                    info!(%local_addr, %peer_addr, "TCP connection refused");
                    self.poll
                        .registry()
                        .deregister(&mut conn.stream)
                        .expect("Failed to deregister connection");
                    self.connections.remove(&id);
                    return WriteConnectionResult::ConnectionFailed;
                }
                Some(err) => {
                    error!(%local_addr, %peer_addr, ?err, "TCP connection establishment failed");
                    self.poll
                        .registry()
                        .deregister(&mut conn.stream)
                        .expect("Failed to deregister connection");
                    self.connections.remove(&id);
                    return WriteConnectionResult::ConnectionFailed;
                }
            }
        }

        // We shouldn't be here if the TCP connection process it not complete
        assert_ne!(conn.connected, ConnectionStatus::NotConnected);

        // Cannot use conn.send_buf in the loop because of the borrow checker
        let mut send_buf = std::mem::take(&mut conn.send_buf);
        let mut send_pos = 0;
        loop {
            // If we have plaintext data to send we can write the data to TSL
            if send_pos < send_buf.len() && conn.connected == ConnectionStatus::TlsConnected {
                let write_result = conn.writer().write(&send_buf[send_pos..]);
                match write_result {
                    Ok(0) => {
                        // TLS buffer is full
                    }
                    Ok(sz) => {
                        send_pos += sz;
                        spurious = false;
                        trace!(len = sz, remaining = send_buf.len() - send_pos, %local_addr, %peer_addr, "Wrote plaintext to TLS");
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {
                        // TLS buffer is full
                    }
                    Err(err) => {
                        error!(%local_addr, %peer_addr, ?err, "Error writing plaintext to TLS");
                        let connected = conn.connected;
                        self.poll
                            .registry()
                            .deregister(&mut conn.stream)
                            .expect("Failed to deregister connection");
                        self.connections.remove(&id);
                        if connected == ConnectionStatus::TcpConnected {
                            return WriteConnectionResult::ConnectionFailed;
                        } else {
                            return WriteConnectionResult::Disconnected;
                        }
                    }
                }
            }

            // Write encrypted TLS data to the socket
            match conn.write_tls() {
                Ok(0) => {
                    // Expected. No more encrypted data to write.
                    break;
                }
                Ok(sz) => {
                    trace!(len = sz, %local_addr, %peer_addr, "Wrote encrypted data to socket");
                    spurious = false;
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    // Further writing would block, so we are done
                    break;
                }
                Err(err) if err.kind() == ErrorKind::BrokenPipe => {
                    // Further writing would block, so we are done
                    break;
                }
                Err(err) => {
                    if err.kind() == ErrorKind::BrokenPipe {
                        warn!(%local_addr, %peer_addr, "Broken pipe");
                    } else if err.kind() == ErrorKind::ConnectionReset {
                        warn!(%local_addr, %peer_addr, "Connection reset");
                    } else {
                        error!(%local_addr, %peer_addr, ?err, "Error writing encrypted data to socket");
                    }
                    let connected = conn.connected;
                    self.poll
                        .registry()
                        .deregister(&mut conn.stream)
                        .expect("Failed to deregister connection");
                    self.connections.remove(&id);
                    if connected == ConnectionStatus::TcpConnected {
                        return WriteConnectionResult::ConnectionFailed;
                    } else {
                        return WriteConnectionResult::Disconnected;
                    }
                }
            }
        }

        // Move the send buffer back to conn.send_buf
        conn.send_buf = std::mem::take(&mut send_buf);

        // Remove the data we wrote
        conn.send_buf.drain(..send_pos);

        let result;
        conn.interest = Interest::READABLE;
        if conn.connected == ConnectionStatus::TcpConnected {
            if conn.is_handshaking() {
                // We are still handshaking
                result = WriteConnectionResult::Ok;
                if conn.wants_write() {
                    conn.interest |= Interest::WRITABLE;
                }
            } else {
                // We completed the handshake, so we must return Connected
                conn.connected = ConnectionStatus::TlsConnected;
                info!(%local_addr, %peer_addr, "TLS handshake completed");
                result = WriteConnectionResult::Connected;
                if conn.wants_write() || !conn.send_buf.is_empty() {
                    conn.interest |= Interest::WRITABLE;
                }
            }
        } else {
            // We are fully connected
            result = WriteConnectionResult::Ok;
            if conn.wants_write() || !conn.send_buf.is_empty() {
                conn.interest |= Interest::WRITABLE;
            }
        }

        // Update our registration, if necessary
        if old_interest != conn.interest {
            self.poll
                .registry()
                .reregister(&mut conn.stream, Token(id), conn.interest)
                .expect("Failed to reregister connection");
        }

        self.track_spurious_wakeup(spurious);

        result
    }
}

// ============================================================================
// Internal Helpers
// ============================================================================

impl TlsTransport {
    // Queues data to a connection and enables writability.
    fn queue_data(&mut self, id: usize, buf: Vec<u8>) {
        let Some(conn) = self.connections.get_mut(&id) else {
            warn!(id, "Connection not found when queuing data");
            return;
        };

        // TODO: enforce a per-connection send buffer limit to avoid unbounded
        // memory usage.

        // If send_buf is empty, as it will be in most cases, send_buf will
        // consume buf to avoid extra memory allocations. Perhaps that's
        // unnecessary if extend() is smart, but I'm not sure how it is
        // implemented, so I'll leave this in here.
        if conn.send_buf.is_empty() {
            conn.send_buf = buf;
        } else {
            conn.send_buf.extend(buf);
        }

        // We need to be WRITABLE to send
        let old_interest = conn.interest;
        conn.interest |= Interest::WRITABLE;

        // Update our registration, if necessary
        if old_interest != conn.interest {
            self.poll
                .registry()
                .reregister(&mut conn.stream, Token(id), conn.interest)
                .expect("Failed to reregister connection");
        }
    }

    fn advance_connection_id(&mut self) {
        loop {
            self.next_id = self
                .next_id
                .checked_add(1)
                .unwrap_or(CONNECTION_ID_RANGE_START);
            if !self.listeners.contains_key(&self.next_id)
                && !self.connections.contains_key(&self.next_id)
            {
                break;
            }
        }
    }

    fn track_spurious_wakeup(&mut self, is_spurious: bool) {
        if is_spurious {
            warn!("Spurious wakeup");
            self.spurious_wakeups += 1;
            assert!(
                self.spurious_wakeups <= self.max_spurious_wakeups as usize,
                "Too many spurious wakeups. Something is wrong."
            );
        } else {
            self.spurious_wakeups = 0;
        }
    }
}

// ============================================================================
// Transport Trait Implementation
// ============================================================================
//
// Note: This trait implementation delegates to inherent methods rather than
// containing the implementation directly. This design keeps all implementation
// logic together in the inherent impl blocks above, making the code more
// maintainable and easier to navigate. The trait impl serves as a thin adapter
// layer for dynamic dispatch.

impl TransportImpl for TlsTransport {
    fn connect_impl(&mut self, addr: SocketAddr) -> Result<(usize, SocketAddr), Error> {
        self.connect(addr)
    }

    fn listen_impl(&mut self, addr: SocketAddr) -> Result<(usize, SocketAddr), Error> {
        self.listen(addr)
    }

    fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        self.get_listener_addresses()
    }

    fn close_connection(&mut self, id: usize) {
        self.close_connection(id)
    }

    fn close_all_connections(&mut self) {
        self.close_all_connections()
    }

    fn shutdown_connection(&mut self, id: usize, how: Shutdown) {
        self.shutdown_connection(id, how)
    }

    fn shutdown_all_connections(&mut self, how: Shutdown) {
        self.shutdown_all_connections(how)
    }

    fn close_listener(&mut self, id: usize) {
        self.close_listener(id)
    }

    fn close_all_listeners(&mut self) {
        self.close_all_listeners()
    }

    fn close_all(&mut self) {
        self.close_all()
    }

    fn send_to(&mut self, id: usize, buf: Vec<u8>) {
        self.send_to(id, buf)
    }

    fn send_to_many(&mut self, ids: &[usize], buf: Vec<u8>) {
        self.send_to_many(ids, buf)
    }

    fn broadcast(&mut self, buf: Vec<u8>) {
        self.broadcast(buf)
    }

    fn broadcast_except(&mut self, buf: Vec<u8>, except_id: usize) {
        self.broadcast_except(buf, except_id)
    }

    fn broadcast_except_many(&mut self, buf: Vec<u8>, except_ids: &[usize]) {
        self.broadcast_except_many(buf, except_ids)
    }

    fn fetch_events(&mut self) -> Result<Vec<TransportEvent>, Error> {
        self.fetch_events()
    }

    fn get_transport_interface(&self) -> TransportInterface {
        self.get_transport_interface()
    }
}
// ============================================================================
