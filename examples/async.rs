//! Async request-response example using ReqRespMessenger
//!
//! This example demonstrates rustcomm's built-in async request-response system.
//! Uses futures::executor - no tokio required!

use bincode::{Decode, Encode};
use config::Config;
use futures::executor::block_on;
use futures::join;
use rustcomm::{
    impl_req_resp_message, register_bincode_message, MessageRegistry, Messenger, MessengerEvent,
    ReqRespMessenger,
};
use std::net::SocketAddr;
use std::thread;

// Request message with unique ID for matching responses
#[derive(Encode, Decode, Debug, Default)]
struct Request {
    request_id: u64,
    text: String,
}

// Use the macro to implement Message trait with request-response support
impl_req_resp_message!(Request, request_id);

// Response message with ID to match back to request
#[derive(Encode, Decode, Debug, Default)]
struct Response {
    request_id: u64,
    text: String,
}

// Use the macro to implement Message trait with request-response support
impl_req_resp_message!(Response, request_id);

/// Server echoes back every request as a response
fn run_server(config: &Config, registry: &MessageRegistry) -> SocketAddr {
    let mut messenger = Messenger::new(config, registry).expect("Failed to create messenger");
    let (_listener_id, listener_addr) = messenger.listen("127.0.0.1:0").expect("Failed to listen");

    thread::spawn(move || {
        loop {
            let events = messenger.fetch_events().expect("Failed to fetch events");
            for event in events {
                match event {
                    MessengerEvent::Message { id, msg } => {
                        // Server receives Request, sends back Response
                        if let Some(request) = msg.downcast_ref::<Request>() {
                            println!(
                                "[Server] Received request {}: {}",
                                request.request_id, request.text
                            );

                            let response = Response {
                                request_id: request.request_id,
                                text: format!("Echo: {}", request.text),
                            };
                            messenger.send_to(id, &response);
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    listener_addr
}

fn main() {
    // Create config and registry
    let config = Config::default();
    let mut registry = MessageRegistry::new();
    register_bincode_message!(registry, Request);
    register_bincode_message!(registry, Response);

    // Start server in background thread
    let server_addr = run_server(&config, &registry);
    println!("[Main] Server started at {}", server_addr);

    // Create ReqRespMessenger - creates its own messenger and starts event loop
    let req_resp =
        ReqRespMessenger::new(&config, &registry).expect("Failed to create ReqRespMessenger");

    // Connect to server using ReqRespMessenger's connect() method
    let (server_id, _) = req_resp.connect(server_addr).expect("Failed to connect");
    println!("[Main] Connected to server with id {}", server_id);

    // Now we can make concurrent async requests!
    println!("\n[Main] Sending 3 concurrent requests...\n");

    // Use futures::executor to run async code without tokio
    let results = block_on(async {
        join!(
            req_resp.send_request::<Response>(
                server_id,
                Box::new(Request {
                    text: "Hello".to_string(),
                    ..Default::default()
                })
            ),
            req_resp.send_request::<Response>(
                server_id,
                Box::new(Request {
                    text: "World".to_string(),
                    ..Default::default()
                })
            ),
            req_resp.send_request::<Response>(
                server_id,
                Box::new(Request {
                    text: "Async".to_string(),
                    ..Default::default()
                })
            ),
        )
    });

    println!("\n[Main] All responses received:");
    println!("  Response 1: {}", results.0.unwrap().text);
    println!("  Response 2: {}", results.1.unwrap().text);
    println!("  Response 3: {}", results.2.unwrap().text);
}
