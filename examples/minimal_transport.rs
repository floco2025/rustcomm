//! Minimal Transport Example - Raw byte echo
//!
//! This example demonstrates the most basic usage of the Transport layer
//! without the message serialization provided by Messenger.
//!
//! ## What it shows
//!
//! - Server echoes back raw bytes
//! - Client sends one byte sequence and receives the echo
//!
//! //! ## Architecture
//!
//! - Creates thread for the server = Client uses main thread
//! - Client blocks until response is received
//!
//! Compare with `minimal` example to see the difference between [`Transport`]
//! (raw bytes) and [`Messenger`] (typed messages).
//!
//! # Usage
//!
//! ```bash
//! cargo run --example minimal_transport
//! ```

use config::Config;
use rustcomm::{Transport, TransportEvent};
use std::net::SocketAddr;
use std::str::from_utf8;
use std::thread;

/// Server echoes back every byte sequence it receives
fn run_server(config: &Config) -> SocketAddr {
    let mut transport = Transport::new(config).expect("Failed to create transport");
    let (_listener_id, listener_addr) = transport.listen("127.0.0.1:0").expect("Failed to listen");

    thread::spawn(move || {
        loop {
            let events = transport.fetch_events().expect("Failed to fetch events");
            for event in events {
                match event {
                    TransportEvent::Data { id, data } => {
                        // Echo back the data
                        transport.send_to(id, data);
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

    // Start the server in a thread
    let server_addr = run_server(&config);
    println!("Server started on {}\n", server_addr);

    // Run the client in this thread
    let mut transport = Transport::new(&config).expect("Failed to create transport");
    let (server_id, _peer_addr) = transport
        .connect(server_addr)
        .expect("Failed to connect to server");

    // Send data
    let data = b"Hello, Transport!".to_vec();
    println!("Sending: {:?}", from_utf8(&data).unwrap());
    transport.send_to(server_id, data.clone());

    // Receive echo
    loop {
        let events = transport.fetch_events().expect("Failed to fetch events");
        for event in events {
            if let TransportEvent::Data { data: received, .. } = event {
                let received_vec: Vec<u8> = received.into_iter().collect();
                println!("Received: {:?}", from_utf8(&received_vec).unwrap());
                return;
            }
        }
    }
}
