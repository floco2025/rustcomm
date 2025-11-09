//! Request-response communication layer on top of Messenger
//!
//! This module provides a higher-level abstraction for request-response patterns,
//! allowing async communication while the underlying Messenger remains synchronous.

use crate::{Message, Messenger, MessengerEvent, MessengerInterface, RequestError};
use futures::channel::oneshot;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use tracing::{debug, error, instrument};

// ============================================================================
// impl_req_resp_message! Macro
// ============================================================================

/// Implements the [`Message`] trait for a request-response message type.
///
/// This macro provides the full [`Message`] trait implementation including
/// request-response support via [`get_request_id()`](Message::get_request_id)
/// and [`set_request_id()`](Message::set_request_id). The message type must
/// have a field that stores the request ID.
///
/// # Example
///
/// ```no_run
/// use rustcomm::impl_req_resp_message;
///
/// #[derive(Debug)]
/// struct Request {
///     request_id: u64,
///     data: String,
/// }
/// impl_req_resp_message!(Request, request_id);
/// ```
#[macro_export]
macro_rules! impl_req_resp_message {
    ($type:ty, $id_field:ident) => {
        impl $crate::Message for $type {
            fn message_id(&self) -> &str {
                stringify!($type)
            }

            fn get_request_id(&self) -> Option<u64> {
                Some(self.$id_field)
            }

            fn set_request_id(&mut self, id: u64) {
                self.$id_field = id;
            }
        }
    };
}

/// Pending request information
struct PendingRequest {
    connection_id: usize,
    sender: oneshot::Sender<Box<dyn Message>>,
}

/// Request-response client that wraps a Messenger for async RPC-style communication
pub struct RequestResponse {
    interface: MessengerInterface,
    pending_requests: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    next_request_id: Arc<Mutex<u64>>,
}

impl RequestResponse {
    /// Create a new RequestResponse and start the event loop in a background thread
    pub fn new(messenger: Messenger) -> Self {
        // Get interface before moving messenger
        let interface = messenger.get_messenger_interface();

        // Shared state for pending requests
        let pending_requests = Arc::new(Mutex::new(HashMap::new()));
        let pending_requests_clone = pending_requests.clone();

        // Spawn event loop thread
        thread::spawn(move || {
            Self::run_event_loop(messenger, pending_requests_clone);
        });

        Self {
            interface,
            pending_requests,
            next_request_id: Arc::new(Mutex::new(0)),
        }
    }

    /// Internal event loop that processes messages and completes pending requests
    fn run_event_loop(
        mut messenger: Messenger,
        pending_requests: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    ) {
        loop {
            let events = messenger.fetch_events().expect("Failed to fetch events");

            for event in events {
                match event {
                    MessengerEvent::Inactive => {
                        // No connections or listeners remain, terminate event loop
                        break;
                    }
                    MessengerEvent::Connected { .. } => {
                        // Nothing to do - connection established
                    }
                    MessengerEvent::ConnectionFailed { .. } => {
                        // Nothing to do - connection attempt failed
                    }
                    MessengerEvent::Disconnected { id } => {
                        // Cancel all pending requests for this connection
                        let mut pending = pending_requests.lock().unwrap();
                        pending.retain(|_request_id, pending_req| {
                            if pending_req.connection_id == id {
                                // Connection closed, drop the sender to signal cancellation
                                // This will cause the receiver to get a Canceled error
                                false
                            } else {
                                true
                            }
                        });
                    }
                    MessengerEvent::Message { msg, id } => {
                        // Check if this message has a request ID (is a response)
                        if let Some(request_id) = msg.get_request_id() {
                            let mut pending = pending_requests.lock().unwrap();
                            if let Some(pending_req) = pending.remove(&request_id) {
                                // Security check: ensure response came from the correct connection
                                if pending_req.connection_id != id {
                                    // TODO: Proper error handling - log security violation and drop message
                                    eprintln!(
                                        "Security violation: response for request {} came from connection {} but expected connection {}",
                                        request_id, id, pending_req.connection_id
                                    );
                                    continue;
                                }

                                // Send the message to the waiting future
                                let _ = pending_req.sender.send(msg);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Send a request and await the response
    ///
    /// Returns the response message cast to the expected type, or an error if:
    /// - The connection closed before receiving a response
    /// - The response was of the wrong type
    /// - The event loop terminated unexpectedly
    ///
    /// The request message must have
    /// [`get_request_id()`](Message::get_request_id) and
    /// [`set_request_id()`](Message::set_request_id) implemented. The request
    /// ID will be set automatically before sending.
    #[instrument(skip(self, request))]
    pub async fn send_request<Resp: Message>(
        &self,
        peer_id: usize,
        mut request: Box<dyn Message>,
    ) -> Result<Resp, RequestError> {
        // Generate unique request ID
        let request_id = {
            let mut id = self.next_request_id.lock().unwrap();
            let current = *id;
            *id += 1;
            current
        };

        debug!("Sending request {} to peer {}", request_id, peer_id);

        // Set the request ID on the message
        request.set_request_id(request_id);

        // Create oneshot channel for this request
        let (tx, rx) = oneshot::channel();

        // Register pending request BEFORE sending
        self.pending_requests.lock().unwrap().insert(
            request_id,
            PendingRequest {
                connection_id: peer_id,
                sender: tx,
            },
        );

        // Send request (non-blocking)
        self.interface.send_to(peer_id, &*request);

        // Await response - this future completes when event loop receives response
        let response = rx.await.map_err(|_| {
            error!(
                "Event loop terminated or connection closed for request {}",
                request_id
            );
            RequestError::EventLoopTerminated
        })?;

        // Downcast to expected response type
        match response.downcast::<Resp>() {
            Ok(typed_response) => {
                debug!("Successfully received response for request {}", request_id);
                Ok(*typed_response)
            }
            Err(_) => {
                error!(
                    "Received response for request {}, but it was not the expected type {}",
                    request_id,
                    std::any::type_name::<Resp>()
                );
                Err(RequestError::WrongResponseType {
                    expected: std::any::type_name::<Resp>(),
                })
            }
        }
    }

    // ============================================================================
    // Delegate methods to MessengerInterface
    // ============================================================================

    /// Listen for incoming connections on the specified address.
    /// Delegates to the underlying Messenger's transport.
    /// Note: This requires access to the Messenger, which is owned by the event loop.
    /// Consider using the Messenger directly before creating RequestResponse if you need to listen.
    // pub fn listen<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
    //     // Cannot implement - we don't have mutable access to Messenger
    // }

    /// Connect to a remote address.
    /// Note: This requires access to the Messenger, which is owned by the event loop.
    /// Consider using the Messenger directly before creating RequestResponse if you need to connect.
    // pub fn connect<A: ToSocketAddrs>(&mut self, addr: A) -> Result<(usize, SocketAddr), Error> {
    //     // Cannot implement - we don't have mutable access to Messenger
    // }

    /// Close a connection by its ID.
    pub fn close_connection(&self, id: usize) {
        self.interface.close_connection(id);
    }

    /// Close a listener by its ID.
    pub fn close_listener(&self, id: usize) {
        self.interface.close_listener(id);
    }

    /// Close all connections and listeners.
    pub fn close_all(&self) {
        self.interface.close_all();
    }
}
