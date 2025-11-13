//! Async RPC example using RpcMessenger
//!
//! This example demonstrates rustcomm's built-in async RPC system. Uses
//! futures::executor - no tokio required!
//! 
//! WORK IN PROGRESS. This is very basic now, but it will change significantly.

use bincode::{Decode, Encode};
use config::Config;
use futures::executor::block_on;
use futures::join;
use rustcomm::{
    impl_message, register_bincode_message, MessageRegistry, Messenger, MessengerEvent,
    RpcContext, RpcMessenger,
};
use std::net::SocketAddr;
use std::thread;

// Request and response messages
#[derive(Encode, Decode, Debug, Default)]
struct CalculateRequest {
    a: i32,
    b: i32,
}
impl_message!(CalculateRequest);

#[derive(Encode, Decode, Debug, Default)]
struct CalculateResponse {
    result: i32,
}
impl_message!(CalculateResponse);

#[derive(Encode, Decode, Debug, Default)]
struct GreetRequest {
    name: String,
}
impl_message!(GreetRequest);

#[derive(Encode, Decode, Debug, Default)]
struct GreetResponse {
    message: String,
}
impl_message!(GreetResponse);

/// Server handles requests and sends responses
fn run_server(config: &Config, registry: &MessageRegistry) -> SocketAddr {
    let transport = rustcomm::transport::Transport::new(config).expect("Failed to create transport");
    let mut messenger = Messenger::<RpcContext>::new_named_with_context(transport, config, registry, "")
        .expect("Failed to create messenger");
    let (_listener_id, listener_addr) = messenger.listen("127.0.0.1:0").expect("Failed to listen");

    thread::spawn(move || {
        loop {
            let events = messenger.fetch_events().expect("Failed to fetch events");
            for event in events {
                match event {
                    MessengerEvent::Message { id, msg, ctx } => {
                        match (ctx.service.as_str(), ctx.method.as_str()) {
                            ("MathService", "Add") => {
                                if let Some(req) = msg.downcast_ref::<CalculateRequest>() {
                                    let resp = CalculateResponse { result: req.a + req.b };
                                    messenger.send_to_with_context(id, &resp, &ctx);
                                }
                            }
                            ("MathService", "Multiply") => {
                                if let Some(req) = msg.downcast_ref::<CalculateRequest>() {
                                    let resp = CalculateResponse { result: req.a * req.b };
                                    messenger.send_to_with_context(id, &resp, &ctx);
                                }
                            }
                            ("GreetService", "SayHello") => {
                                if let Some(req) = msg.downcast_ref::<GreetRequest>() {
                                    let resp = GreetResponse { message: format!("Hello, {}!", req.name) };
                                    messenger.send_to_with_context(id, &resp, &ctx);
                                }
                            }
                            ("GreetService", "SayGoodbye") => {
                                if let Some(req) = msg.downcast_ref::<GreetRequest>() {
                                    let resp = GreetResponse { message: format!("Goodbye, {}!", req.name) };
                                    messenger.send_to_with_context(id, &resp, &ctx);
                                }
                            }
                            _ => {}
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
    register_bincode_message!(registry, CalculateRequest);
    register_bincode_message!(registry, CalculateResponse);
    register_bincode_message!(registry, GreetRequest);
    register_bincode_message!(registry, GreetResponse);

    // Start server in background thread
    let server_addr = run_server(&config, &registry);
    println!("[Main] Server started at {}", server_addr);

    // Create RpcMessenger - creates its own messenger and starts event loop
    let rpc = RpcMessenger::new(&config, &registry).expect("Failed to create RpcMessenger");

    // Connect to server
    let (server_id, _) = rpc.connect(server_addr).expect("Failed to connect");
    println!("[Main] Connected to server\n");

    // Make concurrent async requests to different services
    let results = block_on(async {
        join!(
            rpc.send_request::<CalculateResponse>(
                server_id,
                "MathService",
                "Add",
                Box::new(CalculateRequest { a: 5, b: 3 })
            ),
            rpc.send_request::<CalculateResponse>(
                server_id,
                "MathService",
                "Multiply",
                Box::new(CalculateRequest { a: 4, b: 7 })
            ),
            rpc.send_request::<GreetResponse>(
                server_id,
                "GreetService",
                "SayHello",
                Box::new(GreetRequest { name: "Alice".to_string() })
            ),
            rpc.send_request::<GreetResponse>(
                server_id,
                "GreetService",
                "SayGoodbye",
                Box::new(GreetRequest { name: "Bob".to_string() })
            ),
        )
    });

    println!("[Main] Results:");
    println!("  MathService.Add: {}", results.0.unwrap().result);
    println!("  MathService.Multiply: {}", results.1.unwrap().result);
    println!("  GreetService.SayHello: {}", results.2.unwrap().message);
    println!("  GreetService.SayGoodbye: {}", results.3.unwrap().message);
}
