//! Minimal example - The simplest possible rustcomm program
//!
//! This example demonstrates the absolute minimum code needed for client-server
//! communication with rustcomm.
//!
//! ## What it shows
//!
//! - Single message type (`TextMessage`)
//! - Server echoes back raw bytes
//! - Client that sends one message and receives the echo
//!
//! ## Architecture
//!
//! - Creates thread for the server = Client uses main thread
//! - Client blocks until response is received
//!
//! Compare with `minimal_transport` example to see the difference between
//! [`rustcomm::Transport`] (raw bytes) and [`rustcomm::Messenger`] (typed
//! messages).
//!
//! # Usage
//!
//! ```bash
//! cargo run --example minimal
//! ```

use bincode::{Decode, Encode};
use config::Config;
use rustcomm::{
    impl_message, register_bincode_message, MessageRegistry, Messenger, MessengerEvent,
};
use std::net::SocketAddr;
use std::thread;

#[derive(Encode, Decode, Debug)]
struct TextMessage {
    text: String,
}
impl_message!(TextMessage);

/// Server echoes back every message it receives
fn run_server(config: &Config, registry: &MessageRegistry) -> SocketAddr {
    let mut messenger = Messenger::new(config, registry).expect("Failed to create messenger");
    let (_listener_id, listener_addr) = messenger.listen("127.0.0.1:0").expect("Failed to listen");

    thread::spawn(move || {
        loop {
            let events = messenger.fetch_events().expect("Failed to fetch events");
            for event in events {
                match event {
                    MessengerEvent::Message { id, msg, .. } => {
                        // Echo back the message
                        messenger.send_to(id, &*msg);
                    }
                    _ => {}
                }
            }
        }
    });

    listener_addr
}

fn main() {
    // Create config
    let config = Config::default();

    // Create message registry
    let mut registry = MessageRegistry::new();
    register_bincode_message!(registry, TextMessage);

    // Start the server in a thread
    let server_addr = run_server(&config, &registry);

    // Run the client in this thread
    let mut messenger = Messenger::new(&config, &registry).expect("Failed to create messenger");
    let (server_id, _peer_addr) = messenger
        .connect(server_addr)
        .expect("Failed to connect to server");

    // Send message
    let msg = TextMessage {
        text: "Hello, World!".to_string(),
    };
    println!("Sending: {:?}", msg);
    messenger.send_to(server_id, &msg);

    // Receive echo
    loop {
        let events = messenger.fetch_events().expect("Failed to fetch events");
        for event in events {
            if let MessengerEvent::Message { msg, .. } = event {
                if let Some(received) = msg.downcast_ref::<TextMessage>() {
                    println!("Received: {:?}", received);
                    return;
                }
            }
        }
    }
}
