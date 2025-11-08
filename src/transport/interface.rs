use mio::Waker;
use std::sync::{mpsc::Sender, Arc};

// Internal request type for cross-thread communication
#[derive(Debug)]
pub(crate) enum SendRequest {
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
    CloseConnection {
        id: usize,
    },
    CloseListener {
        id: usize,
    },
    CloseAll,
}

/// Thread-safe interface for sending data through a transport.
///
/// Multiple threads can hold cloned instances to send data to the same
/// transport. The owning thread holding the transport must call `fetch_events()`
/// to process sends.
#[derive(Debug, Clone)]
pub struct TransportInterface {
    pub(crate) sender: Sender<SendRequest>,
    pub(crate) waker: Arc<Waker>,
}

impl TransportInterface {
    /// Queues a message to be sent to a specific connection.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call `Transport::send_to()`
    /// directly for better performance.
    pub fn send_to(&self, id: usize, data: Vec<u8>) {
        let send_request = SendRequest::SendTo { id, data };
        let _ = self.sender.send(send_request);
        let _ = self.waker.wake();
    }

    /// Queues a message to be sent to multiple specific connections.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::send_to_many()` directly for better performance.
    pub fn send_to_many(&self, ids: Vec<usize>, data: Vec<u8>) {
        let send_request = SendRequest::SendToMany { ids, data };
        let _ = self.sender.send(send_request);
        let _ = self.waker.wake();
    }

    /// Queues a message to be broadcast to all connected clients.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call `Transport::broadcast()`
    /// directly for better performance.
    pub fn broadcast(&self, data: Vec<u8>) {
        let send_request = SendRequest::Broadcast { data };
        let _ = self.sender.send(send_request);
        let _ = self.waker.wake();
    }

    /// Queues a message to be broadcast to all connected clients except one.
    ///
    /// This is thread-safe and non-blocking. The message will be serialized and
    /// sent when the Transport's event loop processes it.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::broadcast_except()` directly for better performance.
    pub fn broadcast_except(&self, data: Vec<u8>, except_id: usize) {
        let send_request = SendRequest::BroadcastExcept { data, except_id };
        let _ = self.sender.send(send_request);
        let _ = self.waker.wake();
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
        let send_request = SendRequest::BroadcastExceptMany { data, except_ids };
        let _ = self.sender.send(send_request);
        let _ = self.waker.wake();
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
        let send_request = SendRequest::CloseConnection { id };
        let _ = self.sender.send(send_request);
        let _ = self.waker.wake();
    }

    /// Queues a listener to be closed.
    ///
    /// This is thread-safe and non-blocking. The listener will be closed when
    /// the Transport's event loop processes the request.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::close_listener()` directly for better performance.
    pub fn close_listener(&self, id: usize) {
        let send_request = SendRequest::CloseListener { id };
        let _ = self.sender.send(send_request);
        let _ = self.waker.wake();
    }

    /// Queues all connections and listeners to be closed.
    ///
    /// This is thread-safe and non-blocking. The listener will be closed when
    /// the Transport's event loop processes the request.
    ///
    /// **Note:** If thread-safety is not required, call
    /// `Transport::close_all()` directly for better performance.
    ///
    /// **Note:** This does not trigger `TransportEvent::Disconnected` events.
    /// However, it will trigger a `TransportEvent::Inactive` event if no new
    /// connections or listeners are created before calling `fetch_events()`.
    pub fn close_all(&self) {
        let send_request = SendRequest::CloseAll;
        let _ = self.sender.send(send_request);
        let _ = self.waker.wake();
    }
}
