//! Async request-response example using RequestResponse
//!
//! This example demonstrates rustcomm's built-in async request-response system.
//! Uses futures::executor - no tokio required!
//!
//! ## Key Concepts
//!
//! - RequestResponse wraps Messenger and provides async send_request()
//! - Event loop runs in background thread automatically
//! - Requests get unique IDs to match responses
//! - Messages implement get_request_id() and set_request_id()

use bincode::{Decode, Encode};
use config::Config;
use futures::executor::block_on;
use futures::join;
use rustcomm::{
    impl_req_resp_message, register_bincode_message, MessageRegistry, Messenger, MessengerEvent,
    RequestResponse,
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

    // Create client messenger and connect to server
    let mut messenger = Messenger::new(&config, &registry).expect("Failed to create messenger");
    let (server_id, _) = messenger.connect(server_addr).expect("Failed to connect");
    println!("[Main] Connected to server with id {}", server_id);

    // Create RequestResponse wrapper (starts event loop automatically)
    let req_resp = RequestResponse::new(messenger);

    // Now we can make concurrent async requests!
    println!("\n[Main] Sending 3 concurrent requests...\n");

    // Use futures::executor to run async code without tokio
    let (r1, r2, r3) = block_on(async {
        join!(
            req_resp.send_request(
                server_id,
                Box::new(Request {
                    text: "Hello".to_string(),
                    ..Default::default()
                })
            ),
            req_resp.send_request(
                server_id,
                Box::new(Request {
                    text: "World".to_string(),
                    ..Default::default()
                })
            ),
            req_resp.send_request(
                server_id,
                Box::new(Request {
                    text: "Async".to_string(),
                    ..Default::default()
                })
            ),
        )
    });

    println!("\n[Main] All responses received:");
    println!("  Response 1: {}", r1.downcast_ref::<Response>().unwrap().text);
    println!("  Response 2: {}", r2.downcast_ref::<Response>().unwrap().text);
    println!("  Response 3: {}", r3.downcast_ref::<Response>().unwrap().text);
}
