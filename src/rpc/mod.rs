//! RPC (Remote Procedure Call) communication layer on top of Messenger
//!
//! ⚠️ **WORK IN PROGRESS - DO NOT USE** ⚠️
//!
//! This module is under active development and the API will change
//! significantly. Do not use this until it if finished.

use crate::{
    Context, Message, MessageRegistry, Messenger, MessengerEvent, MessengerInterface, RequestError,
};
use futures::channel::oneshot;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use tracing::{debug, error, instrument};

// ============================================================================
// RpcContext
// ============================================================================

/// Context for RPC messages containing request/response tracking information.
#[derive(Debug, Clone, Copy, Default)]
pub struct RpcContext {
    /// The request ID for matching requests with responses.
    pub request_id: u64,
}

impl RpcContext {
    /// Creates a new RpcContext with the given request ID.
    pub fn new(request_id: u64) -> Self {
        Self { request_id }
    }
}

impl Context for RpcContext {
    fn serialize_into(&self, buf: &mut Vec<u8>) {
        buf.extend(&self.request_id.to_le_bytes());
    }

    fn deserialize(buf: &[u8]) -> Result<(Self, usize), crate::Error>
    where
        Self: Sized,
    {
        if buf.len() < 8 {
            return Err(crate::Error::MalformedData(
                "RpcContext requires 8 bytes for request_id".to_string(),
            ));
        }

        let request_id_bytes: [u8; 8] = buf[0..8]
            .try_into()
            .map_err(|_| crate::Error::MalformedData("Invalid request ID".to_string()))?;
        let request_id = u64::from_le_bytes(request_id_bytes);

        Ok((Self { request_id }, 8))
    }
}

// ============================================================================
// Pending Request Tracking
// ============================================================================

/// Pending request information
struct PendingRequest {
    connection_id: usize,
    sender: oneshot::Sender<Box<dyn Message>>,
}

/// RPC messenger that wraps a Messenger for async RPC-style communication
pub struct RpcMessenger {
    interface: MessengerInterface<RpcContext>,
    pending_requests: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    next_request_id: Arc<Mutex<u64>>,
}

impl RpcMessenger {
    /// Create a new RpcMessenger from configuration and message registry
    ///
    /// This creates an internal [`Messenger`] and starts the event loop in a
    /// background thread. The messenger is owned by the event loop and cannot
    /// be accessed directly.
    pub fn new(config: &config::Config, registry: &MessageRegistry) -> Result<Self, crate::Error> {
        let transport = crate::transport::Transport::new(config)?;
        let messenger = Messenger::<RpcContext>::new_named_with_context(transport, config, registry, "")?;

        // Get interface before moving messenger
        let interface = messenger.get_messenger_interface();

        // Shared state for pending requests
        let pending_requests = Arc::new(Mutex::new(HashMap::new()));
        let pending_requests_clone = pending_requests.clone();

        // Spawn event loop thread
        thread::spawn(move || {
            Self::run_event_loop(messenger, pending_requests_clone);
        });

        Ok(Self {
            interface,
            pending_requests,
            next_request_id: Arc::new(Mutex::new(0)),
        })
    }

    /// Create a new RpcMessenger with a named configuration namespace
    ///
    /// This creates an internal [`Messenger`] using the named config and starts
    /// the event loop in a background thread. The messenger is owned by the
    /// event loop and cannot be accessed directly.
    pub fn new_named(
        config: &config::Config,
        registry: &MessageRegistry,
        name: &str,
    ) -> Result<Self, crate::Error> {
        let transport = crate::transport::Transport::new_named(config, name)?;
        let messenger = Messenger::<RpcContext>::new_named_with_context(transport, config, registry, name)?;

        // Get interface before moving messenger
        let interface = messenger.get_messenger_interface();

        // Shared state for pending requests
        let pending_requests = Arc::new(Mutex::new(HashMap::new()));
        let pending_requests_clone = pending_requests.clone();

        // Spawn event loop thread
        thread::spawn(move || {
            Self::run_event_loop(messenger, pending_requests_clone);
        });

        Ok(Self {
            interface,
            pending_requests,
            next_request_id: Arc::new(Mutex::new(0)),
        })
    }

    /// Internal event loop that processes messages and completes pending
    /// requests
    fn run_event_loop(
        mut messenger: Messenger<RpcContext>,
        pending_requests: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    ) {
        loop {
            let events = messenger.fetch_events().expect("Failed to fetch events");

            for event in events {
                match event {
                    MessengerEvent::Inactive => {
                        // No connections or listeners remain, terminate event
                        // loop
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
                                // Connection closed, drop the sender to signal
                                // cancellation This will cause the receiver to
                                // get a Canceled error
                                false
                            } else {
                                true
                            }
                        });
                    }
                    MessengerEvent::Message { msg, id, ctx } => {
                        // All messages are responses that need to be matched to pending requests
                        let mut pending = pending_requests.lock().unwrap();
                        if let Some(pending_req) = pending.remove(&ctx.request_id) {
                            // Security check: ensure response came from the
                            // correct connection
                            if pending_req.connection_id != id {
                                // TODO: Proper error handling - log
                                // security violation and drop message
                                eprintln!(
                                    "Security violation: response for request {} came from connection {} but expected connection {}",
                                    ctx.request_id, id, pending_req.connection_id
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

    /// Send a request and await the response
    ///
    /// Returns the response message cast to the expected type, or an error if:
    /// - The connection closed before receiving a response
    /// - The response was of the wrong type
    /// - The event loop terminated unexpectedly
    ///
    /// The request ID is automatically assigned and included in the RpcContext.
    #[instrument(skip(self, request))]
    pub async fn send_request<Resp: Message>(
        &self,
        peer_id: usize,
        request: Box<dyn Message>,
    ) -> Result<Resp, RequestError> {
        // Generate unique request ID
        let request_id = {
            let mut id = self.next_request_id.lock().unwrap();
            let current = *id;
            *id += 1;
            current
        };

        debug!("Sending request {} to peer {}", request_id, peer_id);

        // Create RpcContext with the request ID
        let ctx = RpcContext::new(request_id);

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

        // Send request with context (non-blocking)
        self.interface.send_to_with_context(peer_id, &*request, &ctx);

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
    // Connection Management
    // ============================================================================

    /// Connect to a remote address.
    ///
    /// This is thread-safe and blocks until the connection is established.
    /// Delegates to the underlying MessengerInterface.
    pub fn connect<A: std::net::ToSocketAddrs>(
        &self,
        addr: A,
    ) -> Result<(usize, std::net::SocketAddr), crate::Error> {
        self.interface.connect(addr)
    }

    /// Listen for incoming connections on the specified address.
    ///
    /// This is thread-safe and blocks until the listener is created. Delegates
    /// to the underlying MessengerInterface.
    pub fn listen<A: std::net::ToSocketAddrs>(
        &self,
        addr: A,
    ) -> Result<(usize, std::net::SocketAddr), crate::Error> {
        self.interface.listen(addr)
    }

    /// Gets the local socket addresses of all active listeners.
    ///
    /// This is thread-safe and blocks until the addresses are retrieved.
    /// Delegates to the underlying MessengerInterface.
    pub fn get_listener_addresses(&self) -> Vec<std::net::SocketAddr> {
        self.interface.get_listener_addresses()
    }

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
