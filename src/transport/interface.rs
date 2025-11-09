use crate::Error;
use mio::Waker;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{
    mpsc::{channel, Sender},
    Arc,
};

// Internal request type for cross-thread communication
#[derive(Debug)]
pub(crate) enum SendRequest {
    // Connection Management
    
    Connect {
        addr: SocketAddr,
        response: Sender<Result<(usize, SocketAddr), Error>>,
    },
    Listen {
        addr: SocketAddr,
        response: Sender<Result<(usize, SocketAddr), Error>>,
    },
    GetListenerAddresses {
        response: Sender<Vec<SocketAddr>>,
    },
    CloseConnection {
        id: usize,
    },
    CloseListener {
        id: usize,
    },
    CloseAll,

    // Data Operations

    SendTo {
        id: usize,
        data: Vec<u8>,
    },
    SendToMany {
        ids: Vec<usize>,
        data: Vec<u8>,
    },
    Broadcast {
        data: Vec<u8>,
    },
    BroadcastExcept {
        data: Vec<u8>,
        except_id: usize,
    },
    BroadcastExceptMany {
        data: Vec<u8>,
        except_ids: Vec<usize>,
    },
}

/// Thread-safe interface for sending data through a Transport.
///
/// Allows multiple threads to send data to the same
/// [`Transport`](super::Transport). Obtain an instance by calling
/// [`Transport::get_transport_interface()`](super::Transport::get_transport_interface).
///
/// Multiple threads can hold cloned instances to send data concurrently.
#[derive(Debug, Clone)]
pub struct TransportInterface {
    pub(crate) sender: Sender<SendRequest>,
    pub(crate) waker: Arc<Waker>,
}

impl TransportInterface {
    // ============================================================================
    // Connection Management
    // ============================================================================

    /// Initiates a connection to the specified address.
    ///
    /// This is thread-safe and blocks until the connection is initiated or an
    /// error occurs. The Transport's event loop will process the request and
    /// return the connection ID and peer address.
    ///
    /// **Note:** If thread-safety is not required, call `Transport::connect()`
    /// directly for better performance.
    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> Result<(usize, SocketAddr), Error> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .expect("Address resolution returned empty iterator");

        let (tx, rx) = channel();
        self.sender
            .send(SendRequest::Connect { addr, response: tx })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
        rx.recv()
            .expect("Failed to receive response from event loop")
    }

    /// Starts listening for incoming connections on the specified address.
    ///
    /// This is thread-safe and blocks until the listener is created or an error
    /// occurs. The Transport's event loop will process the request and return
    /// the listener ID and bound address.
    ///
    /// **Note:** If thread-safety is not required, call `Transport::listen()`
    /// directly for better performance.
    pub fn listen<A: ToSocketAddrs>(&self, addr: A) -> Result<(usize, SocketAddr), Error> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .expect("Address resolution returned empty iterator");

        let (tx, rx) = channel();
        self.sender
            .send(SendRequest::Listen { addr, response: tx })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
        rx.recv()
            .expect("Failed to receive response from event loop")
    }

    /// Gets the local socket addresses of all active listeners.
    ///
    /// This is thread-safe and blocks until the addresses are retrieved.
    /// The Transport's event loop will process the request and return the addresses.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::get_listener_addresses()` directly for better performance.
    pub fn get_listener_addresses(&self) -> Vec<SocketAddr> {
        let (tx, rx) = channel();
        self.sender
            .send(SendRequest::GetListenerAddresses { response: tx })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
        rx.recv()
            .expect("Failed to receive response from event loop")
    }

    /// Queues a connection to be closed.
    ///
    /// This is thread-safe and non-blocking. The connection will be closed when
    /// the Transport's event loop processes the request.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::close_connection()` directly for better performance.
    ///
    /// **Note:** This does not trigger a `TransportEvent::Disconnected` event.
    pub fn close_connection(&self, id: usize) {
        self.sender
            .send(SendRequest::CloseConnection { id })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
    }

    /// Queues a listener to be closed.
    ///
    /// This is thread-safe and non-blocking. The listener will be closed when
    /// the Transport's event loop processes the request.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::close_listener()` directly for better performance.
    pub fn close_listener(&self, id: usize) {
        self.sender
            .send(SendRequest::CloseListener { id })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
    }

    /// Queues all connections and listeners to be closed.
    ///
    /// This is thread-safe and non-blocking. All connections and listeners will
    /// be closed when the Transport's event loop processes the request.
    ///
    /// **Note:** If thread-safety is not required, call `Transport::close_all()`
    /// directly for better performance.
    ///
    /// **Note:** This does not trigger `TransportEvent::Disconnected` events.
    /// However, it will trigger a `TransportEvent::Inactive` event if no new
    /// connections or listeners are created before calling
    /// `Transport::fetch_events()`.
    pub fn close_all(&self) {
        self.sender
            .send(SendRequest::CloseAll)
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
    }

    // ============================================================================
    // Data Operations
    // ============================================================================

    /// Queues a message to be sent to a specific connection.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call `Transport::send_to()`
    /// directly for better performance.
    pub fn send_to(&self, id: usize, data: Vec<u8>) {
        self.sender
            .send(SendRequest::SendTo { id, data })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
    }

    /// Queues a message to be sent to multiple specific connections.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::send_to_many()` directly for better performance.
    pub fn send_to_many(&self, ids: Vec<usize>, data: Vec<u8>) {
        self.sender
            .send(SendRequest::SendToMany { ids, data })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
    }

    /// Queues a message to be broadcast to all connected clients.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call `Transport::broadcast()`
    /// directly for better performance.
    pub fn broadcast(&self, data: Vec<u8>) {
        self.sender
            .send(SendRequest::Broadcast { data })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
    }

    /// Queues a message to be broadcast to all connected clients except one.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::broadcast_except()` directly for better performance.
    pub fn broadcast_except(&self, data: Vec<u8>, except_id: usize) {
        self.sender
            .send(SendRequest::BroadcastExcept { data, except_id })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
    }

    /// Queues a message to be broadcast to all connected clients except
    /// multiple specified ones.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::broadcast_except_many()` directly for better performance.
    pub fn broadcast_except_many(&self, data: Vec<u8>, except_ids: Vec<usize>) {
        self.sender
            .send(SendRequest::BroadcastExceptMany { data, except_ids })
            .expect("Failed to send request to event loop");
        self.waker.wake().expect("Failed to wake event loop");
    }
}
