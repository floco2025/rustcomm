//! P2P Mesh Network example - Fully connected peer-to-peer topology
//!
//! This example shows how each [`Messenger`] instance can both listen for
//! connections and connect to other peers, enabling peer-to-peer and mesh
//! network architectures.
//!
//! ## Architectures
//!
//! There are three peers (A, B, C) where each peer:
//! - Has 2 listeners (one for each other peer to connect to)
//! - Makes 2 outgoing connections (to each other peer's listener)
//! - Results in 4 total bidirectional connections per peer
//!
//! **Note:** The 2 listeners per peer are for demonstration purposes only. In
//! practice, a single listener is sufficient - you don't need separate
//! listeners for different peers.
//!
//! ## Key Features
//!
//! - [`Messenger::new(config)`](Messenger::new) - Creates instance without
//!   client/server role commitment
//! - [`messenger.listen(addr)`](Messenger::listen) - Adds listener for peers to
//!   connect to
//! - [`messenger.connect(addr)`](Messenger::connect) - Initiates connections to
//!   peers
//! - Bidirectional communication over each connection
//! - Each peer sends and receives messages from all others
//!
//! # Usage
//!
//! ```bash
//! cargo run --example p2p_mesh
//! ```

use bincode::{Decode, Encode};
use config::Config;
use rustcomm::{
    impl_message, register_bincode_message, MessageRegistry, Messenger, MessengerEvent,
};
use std::collections::HashMap;
use std::net::{Shutdown, SocketAddr};
use std::thread;

// Message type for peer communication
#[derive(Debug, Encode, Decode)]
struct PeerMessage {
    from: String,
    to: String,
    text: String,
}
impl_message!(PeerMessage);

fn main() {
    println!("=== P2P Mesh Network Demo ===\n");
    println!("Starting 3 peers in a fully connected mesh...\n");

    // Create config
    let config = Config::default();

    // Create messenger registry
    let mut registry = MessageRegistry::new();
    register_bincode_message!(registry, PeerMessage);

    // Create all messengers and set up listeners
    let mut messenger_a = Messenger::new(&config, &registry).expect("Failed to create messenger A");
    messenger_a.listen("127.0.0.1:0").expect("Failed to listen");
    messenger_a.listen("127.0.0.1:0").expect("Failed to listen");
    let addrs_a = messenger_a.get_listener_addresses();
    println!("[A] Listening on {:?}", addrs_a);

    let mut messenger_b = Messenger::new(&config, &registry).expect("Failed to create messenger B");
    messenger_b.listen("127.0.0.1:0").expect("Failed to listen");
    messenger_b.listen("127.0.0.1:0").expect("Failed to listen");
    let addrs_b = messenger_b.get_listener_addresses();
    println!("[B] Listening on {:?}", addrs_b);

    let mut messenger_c = Messenger::new(&config, &registry).expect("Failed to create messenger C");
    messenger_c.listen("127.0.0.1:0").expect("Failed to listen");
    messenger_c.listen("127.0.0.1:0").expect("Failed to listen");
    let addrs_c = messenger_c.get_listener_addresses();
    println!("[C] Listening on {:?}", addrs_c);

    // Determine connection targets for each peer
    // A connects to B[0] and C[0]
    let connect_a = vec![addrs_b[0], addrs_c[0]];
    // B connects to A[0] and C[1]
    let connect_b = vec![addrs_a[0], addrs_c[1]];
    // C connects to A[1] and B[1]
    let connect_c = vec![addrs_a[1], addrs_b[1]];

    // Spawn peer threads with their messengers
    let peer_a = thread::spawn(move || run_peer("A", messenger_a, connect_a));
    let peer_b = thread::spawn(move || run_peer("B", messenger_b, connect_b));
    let peer_c = thread::spawn(move || run_peer("C", messenger_c, connect_c));

    // Wait for all peers to complete
    peer_a.join().expect("Peer A failed");
    peer_b.join().expect("Peer B failed");
    peer_c.join().expect("Peer C failed");

    println!("\n=== All peers completed successfully! ===");
}

fn run_peer(name: &str, mut messenger: Messenger, connect_addrs: Vec<SocketAddr>) {
    println!("[{name}] Starting peer...");

    // Connect to other peers and track outgoing connection IDs
    let mut outgoing_connections = Vec::new();
    for addr in &connect_addrs {
        let (conn_id, _peer_addr) = messenger.connect(addr).expect("Failed to connect");
        println!("[{name}] Connecting to {addr} (conn_id: {conn_id})");
        outgoing_connections.push(conn_id);
    }

    // Track connected peers and received messages
    let mut connected_peers: HashMap<usize, String> = HashMap::new();
    let mut messages_received = 0;
    let mut messages_sent = 0;
    let expected_connections = 4; // Each peer has 2 outgoing + 2 incoming = 4 total connections
    let expected_messages = 4; // Each peer receives 1 message from each of 4 connections

    // Main event loop
    loop {
        let events = messenger.fetch_events().expect("Failed to fetch events");

        for event in events {
            match event {
                MessengerEvent::Connected { id } => {
                    println!("[{name}] Connection established (id: {id})");
                    connected_peers.insert(id, format!("Peer-{id}"));

                    // Once all connections are established, send messages
                    if connected_peers.len() == expected_connections {
                        // Send messages over all connections (both outgoing and incoming)
                        for (&peer_id, peer_name) in &connected_peers {
                            let msg = PeerMessage {
                                from: name.to_string(),
                                to: peer_name.clone(),
                                text: format!("Hello from {name} to {peer_name}!"),
                            };
                            messenger.send_to(peer_id, &msg);
                            println!("[{name}] Sent message to {peer_name} (id: {peer_id})");
                            messages_sent += 1;
                        }
                    }
                }
                MessengerEvent::Disconnected { id } => {
                    println!("[{name}] Peer disconnected (id: {id})");
                    connected_peers.remove(&id);
                    if connected_peers.is_empty() {
                        return;
                    }
                }
                MessengerEvent::Message { id: _, msg, .. } => {
                    if let Some(peer_msg) = msg.downcast_ref::<PeerMessage>() {
                        println!(
                            "[{name}] Received from {}: '{}'",
                            peer_msg.from, peer_msg.text
                        );
                        messages_received += 1;

                        // Exit when we've received all expected messages
                        if messages_received >= expected_messages {
                            println!(
                                "[{name}] All messages received! Sent: {messages_sent}, Received: {messages_received}"
                            );
                            messenger.shutdown_all_connections(Shutdown::Read);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
