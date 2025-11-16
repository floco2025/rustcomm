//! QUIC transport implementation using mio and quinn-proto.
//!
//! This module provides an event-driven QUIC transport that reuses the same
//! cross-thread request interface as the TCP and TLS implementations. It keeps
//! the integration intentionally small by driving `quinn-proto` directly on top
//! of a non-blocking UDP socket managed by `mio`.

use super::tls_config::{load_tls_client_config, load_tls_server_config};
use super::*;
use crate::config::{get_namespaced_string, get_namespaced_usize};
use crate::error::Error;
use ::config::Config;
use bytes::{Bytes, BytesMut};
use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token, Waker};
use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn_proto::{
    AcceptError, ClientConfig, Connection, ConnectionHandle, DatagramEvent, Dir, Endpoint,
    EndpointConfig, Event, Incoming, ServerConfig, StreamEvent, TransportConfig, VarInt,
    WriteError,
};
use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, instrument, trace, warn};

// Internal constants for connection management
const WAKE_ID: usize = 2;
const CLIENT_SOCKET_TOKEN: usize = 3;
const CONNECTION_ID_RANGE_START: usize = 1000;
const MAX_DATAGRAMS: usize = 16;
const MAX_UDP_PAYLOAD: usize = 65535;

// Internal data type for tracking half shutdown
#[derive(Debug, Default)]
struct StreamHalfShutdown {
    requested: bool,
    applied: bool,
}

impl StreamHalfShutdown {
    fn request(&mut self) {
        self.requested = true;
    }

    fn mark_applied(&mut self) {
        self.applied = true;
    }

    fn pending(&self) -> bool {
        self.requested && !self.applied
    }
}

/// Internal state associated with an active QUIC connection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EndpointRef {
    Client,
    Listener(usize),
}

// Internal data type for a QUIC connection
#[derive(Debug)]
struct QuicConnection {
    handle: ConnectionHandle,
    connection: Connection,
    stream_id: Option<quinn_proto::StreamId>,
    send_buf: VecDeque<u8>,
    connected: bool,
    timeout: Option<Instant>,
    read_shutdown: StreamHalfShutdown,
    write_shutdown: StreamHalfShutdown,
    endpoint: EndpointRef,
}

impl QuicConnection {
    fn new(handle: ConnectionHandle, connection: Connection, endpoint: EndpointRef) -> Self {
        Self {
            handle,
            connection,
            stream_id: None,
            send_buf: VecDeque::new(),
            connected: false,
            timeout: None,
            read_shutdown: StreamHalfShutdown::default(),
            write_shutdown: StreamHalfShutdown::default(),
            endpoint,
        }
    }
}

// Internal data type for a listener state
#[derive(Debug)]
struct ListenerState {
    socket: UdpSocket,
    endpoint: Endpoint,
    recv_buffer: Vec<u8>,
    local_addr: SocketAddr,
    accepting: bool,
}

/// QUIC transport driven by mio.
///
/// Not thread-safe - use TransportInterface for cross-thread communication.
///
/// Note: This struct is internal. Users should use the `Transport` struct instead.
#[derive(Debug)]
pub(super) struct QuicTransport {
    connections: HashMap<usize, QuicConnection>,
    handle_map: HashMap<(EndpointRef, ConnectionHandle), usize>,
    listeners: HashMap<usize, ListenerState>,
    next_id: usize,
    poll: Poll,
    poll_capacity: usize,
    waker: Arc<Waker>,
    sender: Sender<SendRequest>,
    receiver: Receiver<SendRequest>,
    client_socket: Option<UdpSocket>,
    client_endpoint: Option<Endpoint>,
    client_recv_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
    tls_client_config: Option<ClientConfig>,
    tls_server_config: Option<Arc<ServerConfig>>,
    tls_server_name: Option<String>,
}

// ============================================================================
// Constructors
// ============================================================================

impl QuicTransport {
    /// Creates a new named QuicTransport instance with configuration namespacing.
    pub fn new_named(config: &Config, name: &str) -> Result<Self, Error> {
        let poll_capacity =
            get_namespaced_usize(config, name, "poll_capacity").unwrap_or(DEFAULT_POLL_CAPACITY);

        let poll = Poll::new()?;
        let waker = Arc::new(Waker::new(poll.registry(), Token(WAKE_ID))?);
        let (sender, receiver) = channel();

        // Load TLS server config from paths if both cert and key are provided
        let server_config = if let (Ok(cert_path), Ok(key_path)) = (
            get_namespaced_string(config, name, "tls_server_cert"),
            get_namespaced_string(config, name, "tls_server_key"),
        ) {
            Some(build_quic_server_config(&cert_path, &key_path)?)
        } else {
            None
        };

        // Load TLS client config from CA cert path if provided
        let client_config =
            if let Ok(ca_cert_path) = get_namespaced_string(config, name, "tls_ca_cert") {
                Some(build_quic_client_config(&ca_cert_path)?)
            } else {
                None
            };

        // Optional override for the TLS server name/SNI used during connect
        let tls_server_name = match get_namespaced_string(config, name, "tls_server_name") {
            Ok(name) => Some(name),
            Err(::config::ConfigError::NotFound(_)) => None,
            Err(err) => return Err(err.into()),
        };

        Ok(Self {
            connections: HashMap::new(),
            handle_map: HashMap::new(),
            listeners: HashMap::new(),
            next_id: CONNECTION_ID_RANGE_START,
            poll,
            poll_capacity,
            waker,
            sender,
            receiver,
            client_socket: None,
            client_endpoint: None,
            client_recv_buffer: vec![0u8; MAX_UDP_PAYLOAD],
            send_buffer: Vec::with_capacity(MAX_UDP_PAYLOAD),
            tls_client_config: client_config,
            tls_server_config: server_config,
            tls_server_name,
        })
    }
}

// ============================================================================
// Connection Management
// ============================================================================

impl QuicTransport {
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

        let _ = rustls::crypto::ring::default_provider().install_default();

        let local_addr = self.ensure_client_socket(None)?;
        let endpoint = self
            .client_endpoint
            .as_mut()
            .expect("endpoint must exist after ensure_socket");

        let now = Instant::now();
        let server_name = self.tls_server_name.as_deref().unwrap_or("localhost");

        let (handle, connection) = endpoint.connect(
            now,
            self.tls_client_config.as_ref().unwrap().clone(),
            peer_addr,
            server_name,
        )?;

        let conn_id = self.register_connection(EndpointRef::Client, handle, connection);
        info!(id = conn_id, %local_addr, %peer_addr, "Initiating QUIC connection");
        Ok((conn_id, peer_addr))
    }

    /// Starts listening for incoming QUIC connections on the specified address.
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
        let std_socket = std::net::UdpSocket::bind(requested_addr)?;
        std_socket.set_nonblocking(true)?;
        let mut socket = UdpSocket::from_std(std_socket);

        let listener_id = self.next_id;
        self.poll
            .registry()
            .register(&mut socket, Token(listener_id), Interest::READABLE)
            .expect("Failed to register QUIC listener");
        let local_addr = socket
            .local_addr()
            .expect("Failed to get QUIC listener local address");
        info!(id = listener_id, %local_addr, "Listening for QUIC connections");

        let mut endpoint = Endpoint::new(
            Arc::new(EndpointConfig::default()),
            Some(self.tls_server_config.as_ref().unwrap().clone()),
            true,
            None,
        );
        endpoint.set_server_config(Some(self.tls_server_config.as_ref().unwrap().clone()));

        let state = ListenerState {
            socket,
            endpoint,
            recv_buffer: vec![0u8; MAX_UDP_PAYLOAD],
            local_addr,
            accepting: true,
        };

        self.listeners.insert(listener_id, state);
        self.advance_connection_id();
        Ok((listener_id, local_addr))
    }

    /// Returns the local addresses of all listeners currently accepting connections.
    pub fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        self.listeners
            .values()
            .filter(|listener| listener.accepting)
            .map(|listener| listener.local_addr)
            .collect()
    }

    /// Closes a single connection by ID, issuing a QUIC close frame.
    pub fn close_connection(&mut self, id: usize) {
        if let Some(mut conn) = self.connections.remove(&id) {
            let now = Instant::now();
            conn.connection
                .close(now, VarInt::from_u32(0), Bytes::new());
            let local_ip = conn.connection.local_ip();
            let peer_addr = conn.connection.remote_address();
            info!(id, local_ip = ?local_ip, %peer_addr, "Closed QUIC connection");
            self.handle_map.remove(&(conn.endpoint, conn.handle));
        }
    }

    /// Closes every active connection.
    pub fn close_all_connections(&mut self) {
        let ids: Vec<usize> = self.connections.keys().copied().collect();
        for id in ids {
            self.close_connection(id);
        }
    }

    /// Requests a graceful shutdown (half or full) on the given connection.
    pub fn shutdown_connection(&mut self, id: usize, how: Shutdown) {
        if let Some(conn) = self.connections.get_mut(&id) {
            let (read, write) = match how {
                Shutdown::Read => (true, false),
                Shutdown::Write => (false, true),
                Shutdown::Both => (true, true),
            };

            let local_ip = conn.connection.local_ip();
            let peer_addr = conn.connection.remote_address();
            info!(id, how = ?how, local_ip = ?local_ip, %peer_addr, "Shutting down QUIC connection");

            if read {
                conn.read_shutdown.request();
            }
            if write {
                conn.write_shutdown.request();
            }

            if how == Shutdown::Both {
                trace!(id, "Initiating graceful QUIC full shutdown");
                let now = Instant::now();
                conn.connection
                    .close(now, VarInt::from_u32(0), Bytes::new());
            } else {
                trace!(id, read, write, "Requesting QUIC half-shutdown");
                Self::apply_stream_shutdowns(id, conn);
            }
        } else {
            warn!(id, "QUIC connection not found for shutdown");
        }
    }

    /// Applies the requested shutdown mode to every active connection.
    pub fn shutdown_all_connections(&mut self, how: Shutdown) {
        let ids: Vec<usize> = self.connections.keys().copied().collect();
        for id in ids {
            self.shutdown_connection(id, how);
        }
    }

    /// Stops a specific listener and removes it from the poll registry.
    pub fn close_listener(&mut self, id: usize) {
        if let Some(mut state) = self.listeners.remove(&id) {
            self.poll
                .registry()
                .deregister(&mut state.socket)
                .expect("Failed to deregister QUIC listener");
            info!(listener_id = id, %state.local_addr, "Closed QUIC listener");
        }
    }

    /// Stops all listeners.
    pub fn close_all_listeners(&mut self) {
        let ids: Vec<usize> = self.listeners.keys().copied().collect();
        for id in ids {
            self.close_listener(id);
        }
    }

    /// Closes all listeners and connections.
    pub fn close_all(&mut self) {
        self.close_all_connections();
        self.close_all_listeners();
    }
}

// ============================================================================
// Data Operations
// ============================================================================

impl QuicTransport {
    /// Sends data to a specific QUIC connection.
    pub fn send_to(&mut self, id: usize, data: Vec<u8>) {
        debug!(id, len = data.len(), "Sending QUIC data");
        if let Some(conn) = self.connections.get_mut(&id) {
            if conn.write_shutdown.requested {
                warn!(id, "Ignoring send after QUIC write shutdown requested");
                return;
            }
            conn.send_buf.extend(data);
            if conn.connected {
                Self::ensure_stream_open(id, conn);
            }
        } else {
            warn!(id, "QUIC connection not found for send");
        }
    }

    /// Sends data to multiple QUIC connections.
    pub fn send_to_many(&mut self, ids: &[usize], data: Vec<u8>) {
        debug!(
            count = ids.len(),
            len = data.len(),
            "Sending QUIC data to many"
        );
        for &id in ids {
            self.send_to(id, data.clone());
        }
    }

    /// Broadcasts data to all active QUIC connections.
    pub fn broadcast(&mut self, data: Vec<u8>) {
        debug!(len = data.len(), "Broadcasting QUIC data");
        let ids: Vec<_> = self.connections.keys().copied().collect();
        self.send_to_many(&ids, data);
    }

    /// Broadcasts data to all connections except the specified ID.
    pub fn broadcast_except(&mut self, data: Vec<u8>, except_id: usize) {
        debug!(len = data.len(), "Broadcasting QUIC data with exception");
        let ids: Vec<_> = self
            .connections
            .keys()
            .copied()
            .filter(|id| *id != except_id)
            .collect();
        self.send_to_many(&ids, data);
    }

    /// Broadcasts data while excluding multiple connection IDs.
    pub fn broadcast_except_many(&mut self, data: Vec<u8>, except_ids: &[usize]) {
        debug!(
            except_count = except_ids.len(),
            len = data.len(),
            "Broadcasting QUIC data with many exceptions"
        );
        let ids: Vec<_> = self
            .connections
            .keys()
            .copied()
            .filter(|id| !except_ids.contains(id))
            .collect();
        self.send_to_many(&ids, data);
    }
}

// ============================================================================
// Event Operations
// ============================================================================

impl QuicTransport {
    /// Drives the QUIC transport loop until events are available and returns them.
    pub fn fetch_events(&mut self) -> Result<Vec<TransportEvent>, Error> {
        let mut dispatch_events = Vec::new();

        loop {
            self.process_interface_requests();

            if self.connections.is_empty() && self.listeners.is_empty() {
                dispatch_events.push(TransportEvent::Inactive);
                debug!(count = dispatch_events.len(), "Fetched QUIC events");
                return Ok(dispatch_events);
            }

            self.handle_endpoint_udp(EndpointRef::Client)?;
            self.handle_all_listener_udp()?;
            self.drive_connections(&mut dispatch_events)?;

            if !dispatch_events.is_empty() {
                debug!(count = dispatch_events.len(), "Fetched QUIC events");
                return Ok(dispatch_events);
            }

            let timeout = self.next_timeout();
            let mut poll_events = Events::with_capacity(self.poll_capacity);
            self.poll.poll(&mut poll_events, timeout)?;

            for event in poll_events.iter() {
                let Token(id) = event.token();
                if id == WAKE_ID {
                    continue;
                }
                if id == CLIENT_SOCKET_TOKEN {
                    self.handle_endpoint_udp(EndpointRef::Client)?;
                    continue;
                }

                if self.listeners.contains_key(&id) {
                    self.handle_endpoint_udp(EndpointRef::Listener(id))?;
                }
            }
        }
    }
}

// ============================================================================
// Utilities
// ============================================================================

impl QuicTransport {
    /// Returns a thread-safe interface for enqueuing transport requests.
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

impl QuicTransport {
    /// Processes queued requests from `TransportInterface` senders.
    fn process_interface_requests(&mut self) {
        let requests: Vec<SendRequest> = self.receiver.try_iter().collect();
        for request in requests {
            match request {
                SendRequest::Connect { addr, response } => {
                    let result = self.connect(addr);
                    let _ = response.send(result);
                }
                SendRequest::Listen { addr, response } => {
                    let result = self.listen(addr);
                    let _ = response.send(result);
                }
                SendRequest::GetListenerAddresses { response } => {
                    let _ = response.send(self.get_listener_addresses());
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
            }
        }
    }

    /// Pumps datagrams for every active listener endpoint.
    fn handle_all_listener_udp(&mut self) -> Result<(), Error> {
        let listener_ids: Vec<usize> = self.listeners.keys().copied().collect();
        for id in listener_ids {
            self.handle_endpoint_udp(EndpointRef::Listener(id))?;
        }
        Ok(())
    }

    /// Handles pending UDP datagrams for a single endpoint.
    fn handle_endpoint_udp(&mut self, source: EndpointRef) -> Result<(), Error> {
        while let Some((source_ref, peer, now, event, response_buf)) =
            self.poll_endpoint_datagram(source)?
        {
            if let Some(event) = event {
                match source_ref {
                    EndpointRef::Client => {
                        trace!(peer = ?peer, "QUIC datagram event (client)");
                    }
                    EndpointRef::Listener(id) => {
                        trace!(peer = ?peer, listener_id = id, "QUIC datagram event (listener)");
                    }
                }

                self.process_datagram_event(source_ref, event, now, response_buf.as_slice())?;
            }
        }
        Ok(())
    }

    /// Polls a UDP socket for datagrams and returns the parsed QUIC event, if any.
    fn poll_endpoint_datagram(
        &mut self,
        source: EndpointRef,
    ) -> Result<
        Option<(
            EndpointRef,
            SocketAddr,
            Instant,
            Option<DatagramEvent>,
            Vec<u8>,
        )>,
        Error,
    > {
        match source {
            EndpointRef::Client => {
                let Some(socket) = self.client_socket.as_mut() else {
                    return Ok(None);
                };
                let Some(endpoint) = self.client_endpoint.as_mut() else {
                    return Ok(None);
                };

                match socket.recv_from(&mut self.client_recv_buffer) {
                    Ok((len, peer)) => {
                        let bytes = BytesMut::from(&self.client_recv_buffer[..len]);
                        let mut response_buf = Vec::with_capacity(MAX_UDP_PAYLOAD);
                        let now = Instant::now();
                        let event =
                            endpoint.handle(now, peer, None, None, bytes, &mut response_buf);
                        Ok(Some((EndpointRef::Client, peer, now, event, response_buf)))
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(None),
                    Err(err) => Err(err.into()),
                }
            }
            EndpointRef::Listener(listener_id) => {
                let state = self
                    .listeners
                    .get_mut(&listener_id)
                    .expect("Listener should exist when polling datagram");

                match state.socket.recv_from(&mut state.recv_buffer) {
                    Ok((len, peer)) => {
                        let bytes = BytesMut::from(&state.recv_buffer[..len]);
                        let mut response_buf = Vec::with_capacity(MAX_UDP_PAYLOAD);
                        let now = Instant::now();
                        let event =
                            state
                                .endpoint
                                .handle(now, peer, None, None, bytes, &mut response_buf);
                        Ok(Some((
                            EndpointRef::Listener(listener_id),
                            peer,
                            now,
                            event,
                            response_buf,
                        )))
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(None),
                    Err(err) => Err(err.into()),
                }
            }
        }
    }

    /// Returns a mutable reference to the QUIC endpoint for the provided handle.
    fn endpoint_mut(&mut self, endpoint: EndpointRef) -> Option<&mut Endpoint> {
        match endpoint {
            EndpointRef::Client => self.client_endpoint.as_mut(),
            EndpointRef::Listener(id) => {
                let state = self
                    .listeners
                    .get_mut(&id)
                    .expect("Listener should exist for endpoint mutation");
                Some(&mut state.endpoint)
            }
        }
    }

    /// Returns the UDP socket associated with an endpoint.
    fn socket_mut(&mut self, endpoint: EndpointRef) -> Option<&mut UdpSocket> {
        match endpoint {
            EndpointRef::Client => self.client_socket.as_mut(),
            EndpointRef::Listener(id) => {
                let state = self
                    .listeners
                    .get_mut(&id)
                    .expect("Listener should exist for socket mutation");
                Some(&mut state.socket)
            }
        }
    }

    /// Applies a `DatagramEvent` emitted by `quinn-proto` to the corresponding connection/listener.
    fn process_datagram_event(
        &mut self,
        source: EndpointRef,
        event: DatagramEvent,
        now: Instant,
        response_buf: &[u8],
    ) -> Result<(), Error> {
        match event {
            DatagramEvent::ConnectionEvent(handle, conn_event) => {
                trace!(handle = ?handle, "QUIC connection event");
                if let Some(&id) = self.handle_map.get(&(source, handle)) {
                    if let Some(conn) = self.connections.get_mut(&id) {
                        conn.connection.handle_event(conn_event);
                    }
                }
            }
            DatagramEvent::NewConnection(incoming) => {
                trace!("QUIC incoming connection");
                self.accept_connection(source, incoming, now)?;
            }
            DatagramEvent::Response(transmit) => {
                trace!(destination = ?transmit.destination, len = transmit.size, "QUIC transmit from connection");
                self.send_transmit(source, &transmit, &response_buf[..transmit.size])?;
            }
        }
        Ok(())
    }

    /// Accepts an incoming QUIC connection and registers it with transport state.
    fn accept_connection(
        &mut self,
        source: EndpointRef,
        incoming: Incoming,
        now: Instant,
    ) -> Result<(), Error> {
        let (endpoint, listener_meta) = match source {
            EndpointRef::Client => {
                warn!("Ignoring unexpected incoming connection on client endpoint");
                return Ok(());
            }
            EndpointRef::Listener(id) => {
                let state = self
                    .listeners
                    .get_mut(&id)
                    .expect("Listener should exist when accepting connection");
                (&mut state.endpoint, Some((id, state.local_addr)))
            }
        };

        let mut buf = Vec::with_capacity(MAX_UDP_PAYLOAD);
        let peer_addr = incoming.remote_address();
        match endpoint.accept(incoming, now, &mut buf, None) {
            Ok((handle, connection)) => {
                let conn_id = self.register_connection(source, handle, connection);
                if let Some((listener_id, local_addr)) = listener_meta {
                    info!(id = conn_id, listener_id, %local_addr, %peer_addr, "Accepting QUIC connection");
                } else {
                    info!(id = conn_id, %peer_addr, "Accepting QUIC connection");
                }
            }
            Err(AcceptError { cause, response }) => {
                if let Some(transmit) = response {
                    self.send_transmit(source, &transmit, &buf[..transmit.size])?;
                }
                warn!(?cause, "Failed to accept incoming QUIC connection");
            }
        }
        Ok(())
    }

    /// Advances all active connections, generating transmit packets and dispatch events.
    fn drive_connections(&mut self, dispatch: &mut Vec<TransportEvent>) -> Result<(), Error> {
        let now = Instant::now();
        let mut old_connections = HashMap::new();
        // Swap the map out so we can mutate connection state without borrow conflicts.
        std::mem::swap(&mut self.connections, &mut old_connections);
        let mut send_buffer = std::mem::take(&mut self.send_buffer);

        for (id, mut conn) in old_connections.into_iter() {
            self.pump_endpoint_events(&mut conn)?;

            while let Some(transmit) =
                conn.connection
                    .poll_transmit(now, MAX_DATAGRAMS, &mut send_buffer)
            {
                let payload = &send_buffer[..transmit.size];
                self.send_transmit(conn.endpoint, &transmit, payload)?;
                send_buffer.clear();
            }

            let mut alive = true;

            while let Some(event) = conn.connection.poll() {
                match event {
                    Event::Connected => {
                        if !conn.connected {
                            conn.connected = true;
                            dispatch.push(TransportEvent::Connected { id });
                            let local_ip = conn.connection.local_ip();
                            let peer_addr = conn.connection.remote_address();
                            info!(id, local_ip = ?local_ip, %peer_addr, "QUIC connection established");
                        }
                        Self::ensure_stream_open(id, &mut conn);
                    }
                    Event::ConnectionLost { reason } => {
                        alive = false;
                        self.handle_map.remove(&(conn.endpoint, conn.handle));
                        let local_ip = conn.connection.local_ip();
                        let peer_addr = conn.connection.remote_address();
                        info!(id, local_ip = ?local_ip, %peer_addr, ?reason, "QUIC connection lost");
                        if conn.connected {
                            dispatch.push(TransportEvent::Disconnected { id });
                        } else {
                            dispatch.push(TransportEvent::ConnectionFailed { id });
                        }
                        break;
                    }
                    Event::Stream(stream_event) => {
                        trace!(id, ?stream_event, "QUIC stream event");
                        Self::handle_stream_event(id, &mut conn, stream_event, dispatch);
                    }
                    Event::DatagramReceived => {}
                    _ => {}
                }
            }

            if !alive {
                continue;
            }

            if let Some(deadline) = conn.connection.poll_timeout() {
                conn.timeout = Some(deadline);
            }

            if let Some(timeout) = conn.timeout {
                if timeout <= Instant::now() {
                    conn.connection.handle_timeout(now);
                    conn.timeout = conn.connection.poll_timeout();
                }
            }

            self.connections.insert(id, conn);
        }

        self.send_buffer = send_buffer;

        Ok(())
    }

    /// Delivers pending endpoint events from `quinn-proto` back into the connection.
    fn pump_endpoint_events(&mut self, conn: &mut QuicConnection) -> Result<(), Error> {
        let Some(endpoint) = self.endpoint_mut(conn.endpoint) else {
            return Ok(());
        };
        while let Some(event) = conn.connection.poll_endpoint_events() {
            if let Some(response) = endpoint.handle_event(conn.handle, event) {
                conn.connection.handle_event(response);
            }
        }
        Ok(())
    }
}

// ============================================================================
// Internal Connection I/O
// ============================================================================

impl QuicTransport {
    /// Handles `quinn-proto` stream events and converts them into transport actions.
    fn handle_stream_event(
        id: usize,
        conn: &mut QuicConnection,
        stream_event: StreamEvent,
        dispatch: &mut Vec<TransportEvent>,
    ) {
        match stream_event {
            StreamEvent::Opened { dir } => {
                if let Some(accepted) = conn.connection.streams().accept(dir) {
                    trace!(id, ?dir, stream_id = ?accepted, "QUIC accepted incoming stream");
                    if dir == Dir::Bi {
                        if conn.stream_id.is_none() {
                            conn.stream_id = Some(accepted);
                            Self::ensure_stream_open(id, conn);
                        }
                        Self::drain_recv_stream(id, conn, accepted, dispatch);
                    }
                }
            }
            StreamEvent::Readable { id: stream_id } => {
                if conn.stream_id.is_none() {
                    conn.stream_id = Some(stream_id);
                    Self::apply_stream_shutdowns(id, conn);
                }
                Self::drain_recv_stream(id, conn, stream_id, dispatch);
            }
            StreamEvent::Writable { id: stream_id } => {
                if conn.stream_id.is_none() {
                    conn.stream_id = Some(stream_id);
                    Self::apply_stream_shutdowns(id, conn);
                }
                Self::flush_send_buffer(id, conn);
            }
            StreamEvent::Finished { .. } => {
                dispatch.push(TransportEvent::Disconnected { id });
            }
            StreamEvent::Stopped { .. } => {
                dispatch.push(TransportEvent::Disconnected { id });
            }
            StreamEvent::Available { .. } => {}
        }
    }

    /// Ensures a bidirectional stream exists and kicks off buffered sends/shutdowns.
    fn ensure_stream_open(id: usize, conn: &mut QuicConnection) {
        if conn.stream_id.is_none() {
            if let Some(stream_id) = conn.connection.streams().open(Dir::Bi) {
                conn.stream_id = Some(stream_id);
            }
        }
        Self::flush_send_buffer(id, conn);
        Self::apply_stream_shutdowns(id, conn);
    }

    /// Applies pending half-shutdown requests to the active stream.
    fn apply_stream_shutdowns(id: usize, conn: &mut QuicConnection) {
        if conn.stream_id.is_none() {
            return;
        }
        if conn.write_shutdown.pending() {
            Self::finish_send_stream(id, conn);
        }
        if conn.read_shutdown.pending() {
            Self::stop_recv_stream(id, conn);
        }
    }

    /// Finishes the send side of the stream once all buffered bytes are flushed.
    fn finish_send_stream(id: usize, conn: &mut QuicConnection) {
        let Some(stream_id) = conn.stream_id else {
            return;
        };
        Self::flush_send_buffer(id, conn);
        match conn.connection.send_stream(stream_id).finish() {
            Ok(()) => {
                conn.write_shutdown.mark_applied();
                trace!(id, stream_id = ?stream_id, "QUIC send stream finished");
            }
            Err(err) => {
                warn!(id, ?err, "Failed to finish QUIC send stream");
            }
        }
    }

    /// Issues a STOP_SENDING frame for the receive side when read shutdown is requested.
    fn stop_recv_stream(id: usize, conn: &mut QuicConnection) {
        let Some(stream_id) = conn.stream_id else {
            return;
        };
        let err_code = VarInt::from_u32(0);
        match conn.connection.recv_stream(stream_id).stop(err_code) {
            Ok(()) => {
                conn.read_shutdown.mark_applied();
                trace!(id, stream_id = ?stream_id, "QUIC recv stream stopped");
            }
            Err(err) => {
                warn!(id, ?err, "Failed to stop QUIC recv stream");
            }
        }
    }

    /// Reads all available bytes from the stream and emits transport data events.
    fn drain_recv_stream(
        id: usize,
        conn: &mut QuicConnection,
        stream_id: quinn_proto::StreamId,
        dispatch: &mut Vec<TransportEvent>,
    ) {
        if conn.read_shutdown.requested {
            trace!(id, "Dropping incoming data after read shutdown request");
            return;
        }
        let mut buf = Vec::new();
        match conn.connection.recv_stream(stream_id).read(false) {
            Ok(mut chunks) => {
                while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
                    buf.extend_from_slice(&chunk.bytes);
                }
                let _ = chunks.finalize();
                if !buf.is_empty() {
                    debug!(id, bytes = buf.len(), "QUIC received data");
                    dispatch.push(TransportEvent::Data { id, data: buf });
                }
            }
            Err(err) => {
                error!(id, ?err, "Error reading QUIC stream");
            }
        }
    }

    /// Writes buffered application data into the QUIC send stream.
    fn flush_send_buffer(id: usize, conn: &mut QuicConnection) {
        let Some(stream_id) = conn.stream_id else {
            return;
        };

        while !conn.send_buf.is_empty() {
            let write_result = {
                let buf = conn.send_buf.make_contiguous();
                if buf.is_empty() {
                    break;
                }

                match conn.connection.send_stream(stream_id).write(buf) {
                    Ok(0) => return,
                    Ok(written) => {
                        trace!(id, written, "QUIC wrote bytes");
                        Ok(written)
                    }
                    Err(WriteError::Blocked) => Err(WriteError::Blocked),
                    Err(err) => Err(err),
                }
            };

            match write_result {
                Ok(written) => {
                    conn.send_buf.drain(..written);
                }
                Err(WriteError::Blocked) => break,
                Err(err) => {
                    error!(?err, "Error writing to QUIC stream");
                    break;
                }
            }
        }
    }

    /// Sends a `quinn-proto` transmit over the correct UDP socket, handling segmentation if needed.
    fn send_transmit(
        &mut self,
        endpoint: EndpointRef,
        transmit: &quinn_proto::Transmit,
        payload: &[u8],
    ) -> Result<(), Error> {
        let socket = self
            .socket_mut(endpoint)
            .expect("QUIC socket unavailable for transmit");
        if let Some(segment) = transmit.segment_size {
            let mut offset = 0;
            while offset < payload.len() {
                let end = (offset + segment).min(payload.len());
                socket.send_to(&payload[offset..end], transmit.destination)?;
                offset = end;
            }
        } else {
            socket.send_to(payload, transmit.destination)?;
        }
        Ok(())
    }

    /// Returns the nearest connection timeout to use for poll deadlines.
    fn next_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut deadline: Option<Duration> = None;
        for conn in self.connections.values() {
            if let Some(timeout) = conn.timeout {
                if timeout <= now {
                    return Some(Duration::from_millis(0));
                }
                let remaining = timeout.duration_since(now);
                deadline = Some(match deadline {
                    Some(current) => current.min(remaining),
                    None => remaining,
                });
            }
        }
        deadline
    }
}

// ============================================================================
// Internal Helpers
// ============================================================================

impl QuicTransport {
    /// Lazily creates the shared client UDP socket/endpoint and returns its local address.
    fn ensure_client_socket(
        &mut self,
        requested_addr: Option<SocketAddr>,
    ) -> Result<SocketAddr, Error> {
        if let Some(socket) = self.client_socket.as_mut() {
            let addr = socket
                .local_addr()
                .expect("Failed to get QUIC client socket local address");
            return Ok(normalize_addr(addr));
        }

        let bind_addr = requested_addr.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
        let std_socket = std::net::UdpSocket::bind(bind_addr)?;
        std_socket.set_nonblocking(true)?;
        let mut socket = UdpSocket::from_std(std_socket);
        self.poll
            .registry()
            .register(&mut socket, Token(CLIENT_SOCKET_TOKEN), Interest::READABLE)
            .expect("Failed to register QUIC client socket");
        let local_addr = socket
            .local_addr()
            .expect("Failed to get QUIC client socket local address");
        self.client_socket = Some(socket);

        if self.client_endpoint.is_none() {
            let endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None, false, None);
            self.client_endpoint = Some(endpoint);
        }

        Ok(normalize_addr(local_addr))
    }

    /// Inserts a new connection into the tracking maps and returns its ID.
    fn register_connection(
        &mut self,
        source: EndpointRef,
        handle: ConnectionHandle,
        connection: Connection,
    ) -> usize {
        let conn_id = self.next_id;
        self.connections
            .insert(conn_id, QuicConnection::new(handle, connection, source));
        self.handle_map.insert((source, handle), conn_id);
        self.advance_connection_id();
        conn_id
    }

    /// Advances `next_id`, skipping IDs that collide with active resources.
    fn advance_connection_id(&mut self) {
        loop {
            self.next_id = self
                .next_id
                .checked_add(1)
                .unwrap_or(CONNECTION_ID_RANGE_START);
            if !self.connections.contains_key(&self.next_id)
                && !self.listeners.contains_key(&self.next_id)
            {
                break;
            }
        }
    }
}

// ============================================================================
// Transport Trait Implementation
// ============================================================================
//
// Like the TLS transport, this impl simply forwards into the inherent methods
// above so that all business logic lives in one place.

impl TransportImpl for QuicTransport {
    fn connect_impl(&mut self, addr: SocketAddr) -> Result<(usize, SocketAddr), Error> {
        QuicTransport::connect(self, addr)
    }

    fn listen_impl(&mut self, addr: SocketAddr) -> Result<(usize, SocketAddr), Error> {
        QuicTransport::listen(self, addr)
    }

    fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        QuicTransport::get_listener_addresses(self)
    }

    fn close_connection(&mut self, id: usize) {
        QuicTransport::close_connection(self, id)
    }

    fn close_all_connections(&mut self) {
        QuicTransport::close_all_connections(self)
    }

    fn shutdown_connection(&mut self, id: usize, how: Shutdown) {
        QuicTransport::shutdown_connection(self, id, how)
    }

    fn shutdown_all_connections(&mut self, how: Shutdown) {
        QuicTransport::shutdown_all_connections(self, how)
    }

    fn close_listener(&mut self, id: usize) {
        QuicTransport::close_listener(self, id)
    }

    fn close_all_listeners(&mut self) {
        QuicTransport::close_all_listeners(self)
    }

    fn close_all(&mut self) {
        QuicTransport::close_all(self)
    }

    fn send_to(&mut self, id: usize, buf: Vec<u8>) {
        QuicTransport::send_to(self, id, buf)
    }

    fn send_to_many(&mut self, ids: &[usize], buf: Vec<u8>) {
        QuicTransport::send_to_many(self, ids, buf)
    }

    fn broadcast(&mut self, buf: Vec<u8>) {
        QuicTransport::broadcast(self, buf)
    }

    fn broadcast_except(&mut self, buf: Vec<u8>, except_id: usize) {
        QuicTransport::broadcast_except(self, buf, except_id)
    }

    fn broadcast_except_many(&mut self, buf: Vec<u8>, except_ids: &[usize]) {
        QuicTransport::broadcast_except_many(self, buf, except_ids)
    }

    fn fetch_events(&mut self) -> Result<Vec<TransportEvent>, Error> {
        QuicTransport::fetch_events(self)
    }

    fn get_transport_interface(&self) -> TransportInterface {
        QuicTransport::get_transport_interface(self)
    }
}

fn normalize_addr(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V4(v4) => SocketAddr::new((*v4.ip()).into(), v4.port()),
        SocketAddr::V6(v6) => SocketAddr::new((*v6.ip()).into(), v6.port()),
    }
}

fn build_quic_server_config(cert_path: &str, key_path: &str) -> Result<Arc<ServerConfig>, Error> {
    let mut rustls_server = load_tls_server_config(cert_path, key_path)?;
    rustls_server.max_early_data_size = u32::MAX;
    rustls_server.alpn_protocols = vec![b"rustcomm".to_vec()];

    let quic_server = QuicServerConfig::try_from(rustls_server)
        .map_err(|e| Error::TlsServerConfigBuild(e.to_string()))?;

    let mut server = ServerConfig::with_crypto(Arc::new(quic_server));
    server.transport = Arc::new(TransportConfig::default());
    Ok(Arc::new(server))
}

fn build_quic_client_config(ca_cert_path: &str) -> Result<ClientConfig, Error> {
    let mut rustls_client = load_tls_client_config(ca_cert_path)?;
    rustls_client.enable_early_data = true;
    rustls_client.alpn_protocols = vec![b"rustcomm".to_vec()];

    let quic_client = QuicClientConfig::try_from(rustls_client)
        .map_err(|e| Error::TlsClientConfigBuild(e.to_string()))?;

    Ok(ClientConfig::new(Arc::new(quic_client)))
}
