//! QUIC transport implementation using mio and quinn-proto.
//!
//! This module provides an event-driven QUIC transport that reuses the same
//! cross-thread request interface as the TCP and TLS implementations. It keeps
//! the integration intentionally small by driving `quinn-proto` directly on top
//! of a non-blocking UDP socket managed by `mio`.

use super::*;
use crate::error::Error;
use bytes::{Bytes, BytesMut};
use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token, Waker};
use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn_proto::{
    AcceptError, ClientConfig, Connection, ConnectionHandle, DatagramEvent, Dir, Endpoint,
    EndpointConfig, Event, Incoming, ServerConfig, StreamEvent, TransportConfig, VarInt, WriteError,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::collections::{HashMap, VecDeque};
use std::io::{Error as IoError, ErrorKind};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, instrument, warn};

const WAKE_ID: usize = 2;
const SOCKET_TOKEN: usize = 3;
const CONNECTION_ID_RANGE_START: usize = 1000;
const MAX_DATAGRAMS: usize = 16;
const MAX_UDP_PAYLOAD: usize = 65535;

/// State associated with an active QUIC connection.
#[derive(Debug)]
struct QuicConnection {
    handle: ConnectionHandle,
    connection: Connection,
    stream_id: Option<quinn_proto::StreamId>,
    send_buf: VecDeque<u8>,
    connected: bool,
    timeout: Option<Instant>,
    read_shutdown_requested: bool,
    write_shutdown_requested: bool,
    read_shutdown_applied: bool,
    write_shutdown_applied: bool,
}

impl QuicConnection {
    fn new(handle: ConnectionHandle, connection: Connection) -> Self {
        Self {
            handle,
            connection,
            stream_id: None,
            send_buf: VecDeque::new(),
            connected: false,
            timeout: None,
            read_shutdown_requested: false,
            write_shutdown_requested: false,
            read_shutdown_applied: false,
            write_shutdown_applied: false,
        }
    }
}

/// QUIC transport driven by mio.
#[derive(Debug)]
pub(super) struct QuicTransport {
    poll: Poll,
    waker: Arc<Waker>,
    sender: Sender<SendRequest>,
    receiver: Receiver<SendRequest>,
    socket: Option<UdpSocket>,
    endpoint: Option<Endpoint>,
    connections: HashMap<usize, QuicConnection>,
    handle_map: HashMap<ConnectionHandle, usize>,
    listeners: HashMap<usize, SocketAddr>,
    next_id: usize,
    recv_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
    client_config: ClientConfig,
    server_config: Option<Arc<ServerConfig>>,
    poll_capacity: usize,
}

impl QuicTransport {
    pub fn new_named(config: &config::Config, name: &str) -> Result<Self, Error> {
        let poll_capacity: usize = if name.is_empty() {
            config.get("quic_poll_capacity").unwrap_or(256)
        } else {
            config
                .get(&format!("{}.quic_poll_capacity", name))
                .or_else(|_| config.get("quic_poll_capacity"))
                .unwrap_or(256)
        };

        let poll = Poll::new()?;
        let waker = Arc::new(Waker::new(poll.registry(), Token(WAKE_ID))?);
        let (sender, receiver) = channel();

        let client_config = ClientConfig::new(Arc::new(build_insecure_client_crypto()));

        Ok(Self {
            poll,
            waker,
            sender,
            receiver,
            socket: None,
            endpoint: None,
            connections: HashMap::new(),
            handle_map: HashMap::new(),
            listeners: HashMap::new(),
            next_id: CONNECTION_ID_RANGE_START,
            recv_buffer: vec![0u8; MAX_UDP_PAYLOAD],
            send_buffer: Vec::with_capacity(MAX_UDP_PAYLOAD),
            client_config,
            server_config: None,
            poll_capacity,
        })
    }
}

impl QuicTransport {
    fn ensure_socket(&mut self, addr: Option<SocketAddr>) -> Result<SocketAddr, Error> {
        if let Some(socket) = &self.socket {
            let local_addr = socket.local_addr()?;
            if let Some(expected) = addr {
                let normalized_local = normalize_addr(local_addr);
                let normalized_expected = normalize_addr(expected);

                let addr_mismatch = if normalized_expected.port() == 0 {
                    normalized_local.ip() != normalized_expected.ip()
                } else {
                    normalized_local != normalized_expected
                };

                if addr_mismatch {
                    return Err(Error::Io(IoError::new(
                        ErrorKind::AddrInUse,
                        "QUIC transport already bound to a different address",
                    )));
                }
            }
            return Ok(local_addr);
        }

        let bind_addr = addr.unwrap_or_else(|| "0.0.0.0:0".parse().unwrap());
        let std_socket = std::net::UdpSocket::bind(bind_addr)?;
        std_socket.set_nonblocking(true)?;
        let mut socket = UdpSocket::from_std(std_socket);
        self.poll
            .registry()
            .register(&mut socket, Token(SOCKET_TOKEN), Interest::READABLE)?;
        let local_addr = socket.local_addr()?;
        self.socket = Some(socket);

        let endpoint = Endpoint::new(
            Arc::new(EndpointConfig::default()),
            self.server_config.clone(),
            true,
            None,
        );
        self.endpoint = Some(endpoint);

        Ok(local_addr)
    }

    fn update_server_config(&mut self) -> Result<(), Error> {
        if self.server_config.is_none() {
            let config = build_server_config()?;
            self.server_config = Some(config);
        }
        if let (Some(endpoint), Some(server_cfg)) =
            (self.endpoint.as_mut(), self.server_config.clone())
        {
            endpoint.set_server_config(Some(server_cfg));
        }
        Ok(())
    }

    #[instrument(skip(self, addr))]
    pub fn connect<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        let peer_addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, "Could not resolve address"))?;

        let _ = rustls::crypto::ring::default_provider().install_default();

        let _local = self.ensure_socket(None)?;
        let endpoint = self
            .endpoint
            .as_mut()
            .expect("endpoint must exist after ensure_socket");

        let now = Instant::now();
        let (handle, connection) = endpoint
            .connect(now, self.client_config.clone(), peer_addr, "localhost")
            .map_err(|err| Error::Io(IoError::new(ErrorKind::Other, err.to_string())))?;

        let conn_id = self.next_id;
        let quic_conn = QuicConnection::new(handle, connection);
        self.connections.insert(conn_id, quic_conn);
        self.handle_map.insert(handle, conn_id);
        self.advance_connection_id();

        Ok((conn_id, peer_addr))
    }

    #[instrument(skip(self, addr))]
    pub fn listen<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
        let requested = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, "Could not resolve address"))?;

        let local_addr = self.ensure_socket(Some(requested))?;
        self.update_server_config()?;

        let listener_id = self.next_id;
        self.listeners.insert(listener_id, local_addr);
        self.advance_connection_id();
        Ok((listener_id, local_addr))
    }

    pub fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        self.listeners.values().copied().collect()
    }

    pub fn close_connection(&mut self, id: usize) {
        if let Some(mut conn) = self.connections.remove(&id) {
            let now = Instant::now();
            conn.connection
                .close(now, VarInt::from_u32(0), Bytes::new());
            self.handle_map.remove(&conn.handle);
        }
    }

    pub fn close_all_connections(&mut self) {
        let ids: Vec<usize> = self.connections.keys().copied().collect();
        for id in ids {
            self.close_connection(id);
        }
    }

    pub fn shutdown_connection(&mut self, id: usize, how: Shutdown) {
        if let Some(conn) = self.connections.get_mut(&id) {
            match how {
                Shutdown::Read => {
                    debug!(id, "Requesting QUIC read-side shutdown");
                    conn.read_shutdown_requested = true;
                    Self::apply_stream_shutdowns(id, conn);
                }
                Shutdown::Write => {
                    debug!(id, "Requesting QUIC write-side shutdown");
                    conn.write_shutdown_requested = true;
                    Self::apply_stream_shutdowns(id, conn);
                }
                Shutdown::Both => {
                    debug!(id, "Initiating graceful QUIC full shutdown");
                    conn.read_shutdown_requested = true;
                    conn.write_shutdown_requested = true;
                    let now = Instant::now();
                    conn.connection.close(now, VarInt::from_u32(0), Bytes::new());
                }
            }
        } else {
            warn!(id, "QUIC connection not found for shutdown");
        }
    }

    pub fn shutdown_all_connections(&mut self, how: Shutdown) {
        let ids: Vec<usize> = self.connections.keys().copied().collect();
        for id in ids {
            self.shutdown_connection(id, how);
        }
    }

    pub fn close_listener(&mut self, id: usize) {
        self.listeners.remove(&id);
    }

    pub fn close_all_listeners(&mut self) {
        self.listeners.clear();
    }

    pub fn close_all(&mut self) {
        self.close_all_connections();
        self.close_all_listeners();
    }

    pub fn send_to(&mut self, id: usize, data: Vec<u8>) {
        if let Some(conn) = self.connections.get_mut(&id) {
            if conn.write_shutdown_requested {
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

    pub fn send_to_many(&mut self, ids: &[usize], data: Vec<u8>) {
        for &id in ids {
            self.send_to(id, data.clone());
        }
    }

    pub fn broadcast(&mut self, data: Vec<u8>) {
        let ids: Vec<_> = self.connections.keys().copied().collect();
        self.send_to_many(&ids, data);
    }

    pub fn broadcast_except(&mut self, data: Vec<u8>, except_id: usize) {
        let ids: Vec<_> = self
            .connections
            .keys()
            .copied()
            .filter(|id| *id != except_id)
            .collect();
        self.send_to_many(&ids, data);
    }

    pub fn broadcast_except_many(&mut self, data: Vec<u8>, except_ids: &[usize]) {
        let ids: Vec<_> = self
            .connections
            .keys()
            .copied()
            .filter(|id| !except_ids.contains(id))
            .collect();
        self.send_to_many(&ids, data);
    }

    pub fn fetch_events(&mut self) -> Result<Vec<TransportEvent>, Error> {
        let mut dispatch_events = Vec::new();

        loop {
            self.process_interface_requests();

            if self.connections.is_empty() && self.listeners.is_empty() {
                dispatch_events.push(TransportEvent::Inactive);
                return Ok(dispatch_events);
            }

            self.handle_udp()?;
            self.drive_connections(&mut dispatch_events)?;

            if !dispatch_events.is_empty() {
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
                if id == SOCKET_TOKEN {
                    self.handle_udp()?;
                }
            }
        }
    }

    pub fn get_transport_interface(&self) -> TransportInterface {
        TransportInterface {
            sender: self.sender.clone(),
            waker: self.waker.clone(),
        }
    }

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

    fn handle_udp(&mut self) -> Result<(), Error> {
        if self.socket.is_none() || self.endpoint.is_none() {
            return Ok(());
        }

        let mut response_buf = Vec::with_capacity(MAX_UDP_PAYLOAD);

        loop {
            let result = {
                let socket = self
                    .socket
                    .as_mut()
                    .expect("socket must exist while handling UDP");
                socket.recv_from(&mut self.recv_buffer)
            };

            match result {
                Ok((len, peer)) => {
                    let bytes = BytesMut::from(&self.recv_buffer[..len]);
                    let now = Instant::now();
                    let event = {
                        let endpoint = self
                            .endpoint
                            .as_mut()
                            .expect("endpoint must exist while handling UDP");
                        endpoint.handle(now, peer, None, None, bytes, &mut response_buf)
                    };

                    if let Some(event) = event {
                        debug!(peer = ?peer, "QUIC datagram event");
                        self.process_datagram_event(event, now, &response_buf)?;
                    }
                    response_buf.clear();
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => return Err(err.into()),
            }
        }

        Ok(())
    }

    fn process_datagram_event(
        &mut self,
        event: DatagramEvent,
        now: Instant,
        response_buf: &[u8],
    ) -> Result<(), Error> {
        match event {
            DatagramEvent::ConnectionEvent(handle, conn_event) => {
                debug!(handle = ?handle, "QUIC connection event");
                if let Some(&id) = self.handle_map.get(&handle) {
                    if let Some(conn) = self.connections.get_mut(&id) {
                        conn.connection.handle_event(conn_event);
                    }
                }
            }
            DatagramEvent::NewConnection(incoming) => {
                debug!("QUIC incoming connection");
                self.accept_connection(incoming, now)?;
            }
            DatagramEvent::Response(transmit) => {
                let payload = response_buf[..transmit.size].to_vec();
                debug!(destination = ?transmit.destination, len = transmit.size, "QUIC transmit from connection");
                self.send_transmit(&transmit, &payload)?;
            }
        }
        Ok(())
    }
    fn accept_connection(&mut self, incoming: Incoming, now: Instant) -> Result<(), Error> {
        let endpoint = self
            .endpoint
            .as_mut()
            .expect("endpoint must exist before accepting");

        let mut buf = Vec::with_capacity(MAX_UDP_PAYLOAD);
        match endpoint.accept(incoming, now, &mut buf, None) {
            Ok((handle, connection)) => {
                let conn_id = self.next_id;
                self.connections
                    .insert(conn_id, QuicConnection::new(handle, connection));
                self.handle_map.insert(handle, conn_id);
                self.advance_connection_id();
            }
            Err(AcceptError { cause, response }) => {
                if let Some(transmit) = response {
                    self.send_transmit(&transmit, &buf[..transmit.size])?;
                }
                warn!(?cause, "Failed to accept incoming QUIC connection");
            }
        }
        Ok(())
    }

    fn drive_connections(&mut self, dispatch: &mut Vec<TransportEvent>) -> Result<(), Error> {
        let now = Instant::now();
        let mut old_connections = HashMap::new();
        std::mem::swap(&mut self.connections, &mut old_connections);

        for (id, mut conn) in old_connections.into_iter() {
            self.pump_endpoint_events(&mut conn)?;

            while let Some(transmit) =
                conn.connection
                    .poll_transmit(now, MAX_DATAGRAMS, &mut self.send_buffer)
            {
                let payload = self.send_buffer[..transmit.size].to_vec();
                self.send_transmit(&transmit, &payload)?;
                self.send_buffer.clear();
            }

            let mut alive = true;

            while let Some(event) = conn.connection.poll() {
                match event {
                    Event::Connected => {
                        conn.connected = true;
                        Self::ensure_stream_open(id, &mut conn);
                        debug!(id, "QUIC connection established");
                        dispatch.push(TransportEvent::Connected { id });
                    }
                    Event::ConnectionLost { .. } => {
                        alive = false;
                        self.handle_map.remove(&conn.handle);
                        debug!(id, was_connected = conn.connected, "QUIC connection lost");
                        if conn.connected {
                            dispatch.push(TransportEvent::Disconnected { id });
                        } else {
                            dispatch.push(TransportEvent::ConnectionFailed { id });
                        }
                        break;
                    }
                    Event::Stream(stream_event) => {
                        debug!(id, ?stream_event, "QUIC stream event");
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

        Ok(())
    }

    fn pump_endpoint_events(&mut self, conn: &mut QuicConnection) -> Result<(), Error> {
        let Some(endpoint) = self.endpoint.as_mut() else {
            return Ok(());
        };
        while let Some(event) = conn.connection.poll_endpoint_events() {
            if let Some(response) = endpoint.handle_event(conn.handle, event) {
                conn.connection.handle_event(response);
            }
        }
        Ok(())
    }

    fn handle_stream_event(
        id: usize,
        conn: &mut QuicConnection,
        stream_event: StreamEvent,
        dispatch: &mut Vec<TransportEvent>,
    ) {
        match stream_event {
            StreamEvent::Opened { dir } => {
                if let Some(accepted) = conn.connection.streams().accept(dir) {
                    debug!(id, ?dir, stream_id = ?accepted, "QUIC accepted incoming stream");
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

    fn ensure_stream_open(id: usize, conn: &mut QuicConnection) {
        if conn.stream_id.is_none() {
            if let Some(stream_id) = conn.connection.streams().open(Dir::Bi) {
                conn.stream_id = Some(stream_id);
            }
        }
        Self::flush_send_buffer(id, conn);
        Self::apply_stream_shutdowns(id, conn);
    }

    fn apply_stream_shutdowns(id: usize, conn: &mut QuicConnection) {
        if conn.stream_id.is_none() {
            return;
        }
        if conn.write_shutdown_requested && !conn.write_shutdown_applied {
            Self::finish_send_stream(id, conn);
        }
        if conn.read_shutdown_requested && !conn.read_shutdown_applied {
            Self::stop_recv_stream(id, conn);
        }
    }

    fn finish_send_stream(id: usize, conn: &mut QuicConnection) {
        let Some(stream_id) = conn.stream_id else {
            return;
        };
        Self::flush_send_buffer(id, conn);
        match conn.connection.send_stream(stream_id).finish() {
            Ok(()) => {
                conn.write_shutdown_applied = true;
                debug!(id, stream_id = ?stream_id, "QUIC send stream finished");
            }
            Err(err) => {
                warn!(id, ?err, "Failed to finish QUIC send stream");
            }
        }
    }

    fn stop_recv_stream(id: usize, conn: &mut QuicConnection) {
        let Some(stream_id) = conn.stream_id else {
            return;
        };
        let err_code = VarInt::from_u32(0);
        match conn.connection.recv_stream(stream_id).stop(err_code) {
            Ok(()) => {
                conn.read_shutdown_applied = true;
                debug!(id, stream_id = ?stream_id, "QUIC recv stream stopped");
            }
            Err(err) => {
                warn!(id, ?err, "Failed to stop QUIC recv stream");
            }
        }
    }

    fn drain_recv_stream(
        id: usize,
        conn: &mut QuicConnection,
        stream_id: quinn_proto::StreamId,
        dispatch: &mut Vec<TransportEvent>,
    ) {
        if conn.read_shutdown_requested {
            debug!(id, "Dropping incoming data after read shutdown request");
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

    fn flush_send_buffer(id: usize, conn: &mut QuicConnection) {
        let Some(stream_id) = conn.stream_id else {
            return;
        };

        while !conn.send_buf.is_empty() {
            let (front, back) = conn.send_buf.as_slices();
            let chunk = if !front.is_empty() { front } else { back };
            if chunk.is_empty() {
                break;
            }

            match conn.connection.send_stream(stream_id).write(chunk) {
                Ok(0) => break,
                Ok(written) => {
                    debug!(id, written, "QUIC wrote bytes");
                    for _ in 0..written {
                        conn.send_buf.pop_front();
                    }
                }
                Err(WriteError::Blocked) => break,
                Err(err) => {
                    error!(?err, "Error writing to QUIC stream");
                    break;
                }
            }
        }
    }

    fn send_transmit(
        &mut self,
        transmit: &quinn_proto::Transmit,
        payload: &[u8],
    ) -> Result<(), Error> {
        let Some(socket) = self.socket.as_mut() else {
            return Ok(());
        };
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

fn build_server_config() -> Result<Arc<ServerConfig>, Error> {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(|e| Error::TlsServerConfigBuild(e.to_string()))?;
    let cert = CertificateDer::from(certified.cert.der().clone());
    let key = PrivateKeyDer::Pkcs8(certified.key_pair.serialize_der().into());

    let mut rustls_server = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .map_err(|e| Error::TlsServerConfigBuild(e.to_string()))?;
    rustls_server.max_early_data_size = u32::MAX;
    rustls_server.alpn_protocols = vec![b"rustcomm".to_vec()];

    let quic_server = QuicServerConfig::try_from(rustls_server)
        .map_err(|e| Error::TlsServerConfigBuild(e.to_string()))?;

    let mut server = ServerConfig::with_crypto(Arc::new(quic_server));
    server.transport = Arc::new(TransportConfig::default());
    Ok(Arc::new(server))
}

fn build_insecure_client_crypto() -> QuicClientConfig {
    #[derive(Debug)]
    struct InsecureVerifier;

    impl ServerCertVerifier for InsecureVerifier {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer,
            _intermediates: &[CertificateDer],
            _server_name: &ServerName,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::RSA_PSS_SHA256,
                SignatureScheme::RSA_PKCS1_SHA256,
            ]
        }
    }

    let verifier = Arc::new(InsecureVerifier);

    let mut rustls_client = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(verifier)
    .with_no_client_auth();
    rustls_client.enable_early_data = true;
    rustls_client.alpn_protocols = vec![b"rustcomm".to_vec()];

    QuicClientConfig::try_from(rustls_client).expect("failed to build insecure QUIC client config")
}
