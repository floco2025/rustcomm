//! Custom Serialization Example
//!
//! This example demonstrates how to use different serialization formats with rustcomm.
//! It shows four approaches:
//!
//! 1. **Bincode** (native, with macros) - Binary format using bincode's Encode/Decode traits, no serde dependency
//! 2. **JSON** (serde-based, manual registration) - Human-readable text format using serde
//! 3. **MessagePack** (serde-based, manual registration) - Compact binary format using serde
//! 4. **Hand-written** (manual, no dependencies) - Custom binary format with manual byte manipulation
//!
//! ## Key Concepts
//!
//! - Using the `register_bincode_message!` macro for bincode (no serde required)
//! - Manually registering messages with custom serde-based serialization/deserialization
//! - Mixing multiple formats in a single application
//! - Each message type can use a different serialization format
//! - Bincode 2.0 can work without serde, while JSON and MessagePack require it
//!
//! ## How It Works
//!
//! rustcomm's message system has two function signatures:
//!
//! - **Serializer**: `fn(&dyn Message, &mut Vec<u8>)` - writes bytes to buffer
//! - **Deserializer**: `fn(&[u8]) -> Result<Box<dyn Message>, Error>` - deserializes complete message
//!
//! ## Dependencies
//!
//! Add to Cargo.toml:
//! ```toml
//! [dependencies]
//! bincode = { version = "2.0", features = ["derive"] }  # For native bincode
//! serde = { version = "1.0", features = ["derive"] }    # For JSON and MessagePack
//!
//! [dev-dependencies]
//! serde_json = "1.0"
//! rmp-serde = "1.1"
//! ```
//!
//! # Usage
//!
//! ```bash
//! cargo run --example custom_serialization
//! ```

use bincode::{Decode, Encode};
use config::Config;
use rustcomm::{impl_message, Error, Message, MessageRegistry, Messenger, MessengerEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::thread;

// ============================================================================
// Message 1: Bincode (using macro) - Pure bincode, no serde
// ============================================================================

#[derive(Encode, Decode, Debug)]
struct BincodeMessage {
    id: u32,
    data: Vec<u8>,
}
impl_message!(BincodeMessage);

// ============================================================================
// Message 2: JSON (manual registration) - Uses serde
// ============================================================================

#[derive(Serialize, Deserialize, Debug)]
struct JsonMessage {
    name: String,
    value: f64,
    active: bool,
}
impl_message!(JsonMessage);

// JSON serialization helper (generic)
fn serialize_json_message<T: Message + Serialize + 'static>(msg: &dyn Message, buf: &mut Vec<u8>) {
    let json_msg = msg.downcast_ref::<T>().expect("type mismatch");
    serde_json::to_writer(buf as &mut dyn std::io::Write, json_msg)
        .expect("JSON serialization failed");
}

// JSON deserialization helper (generic)
fn deserialize_json_message<T: Message + for<'de> Deserialize<'de> + 'static>(
    data: &[u8],
) -> Result<Box<dyn Message>, Error> {
    serde_json::from_slice::<T>(data)
        .map(|msg| Box::new(msg) as Box<dyn Message>)
        .map_err(|e| Error::MalformedData(format!("JSON: {}", e)))
}

// ============================================================================
// Message 3: MessagePack (manual registration) - Uses serde
// ============================================================================

#[derive(Serialize, Deserialize, Debug)]
struct MessagePackMessage {
    timestamp: u64,
    tags: Vec<String>,
    metadata: HashMap<String, String>,
}
impl_message!(MessagePackMessage);

// MessagePack serialization helper (generic)
fn serialize_msgpack_message<T: Message + Serialize + 'static>(
    msg: &dyn Message,
    buf: &mut Vec<u8>,
) {
    let msgpack_msg = msg.downcast_ref::<T>().expect("type mismatch");
    rmp_serde::encode::write(buf as &mut dyn std::io::Write, msgpack_msg)
        .expect("MessagePack serialization failed");
}

// MessagePack deserialization helper (generic)
fn deserialize_msgpack_message<T: Message + for<'de> Deserialize<'de> + 'static>(
    data: &[u8],
) -> Result<Box<dyn Message>, Error> {
    rmp_serde::from_slice::<T>(data)
        .map(|msg| Box::new(msg) as Box<dyn Message>)
        .map_err(|e| Error::MalformedData(format!("MessagePack: {}", e)))
}

// ============================================================================
// Message 4: Hand-written serialization (no dependencies)
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
struct ManualMessage {
    player_id: u32,
    x: f32,
    y: f32,
    health: u8,
    name: String,
}
impl_message!(ManualMessage);

// Hand-written serialization (similar to message header format)
// Format: [player_id: u32][x: f32][y: f32][health: u8][name_len: u32][name: bytes]
fn serialize_manual_message(msg: &dyn Message, buf: &mut Vec<u8>) {
    let manual_msg = msg.downcast_ref::<ManualMessage>().expect("type mismatch");

    // Write player_id (4 bytes, little-endian)
    buf.extend(&manual_msg.player_id.to_le_bytes());

    // Write x coordinate (4 bytes, little-endian)
    buf.extend(&manual_msg.x.to_le_bytes());

    // Write y coordinate (4 bytes, little-endian)
    buf.extend(&manual_msg.y.to_le_bytes());

    // Write health (1 byte)
    buf.push(manual_msg.health);

    // Write name with length prefix
    let name_bytes = manual_msg.name.as_bytes();
    buf.extend(&(name_bytes.len() as u32).to_le_bytes());
    buf.extend(name_bytes);
}

// Hand-written deserialization
fn deserialize_manual_message(data: &[u8]) -> Result<Box<dyn Message>, Error> {
    // Check minimum size: player_id(4) + x(4) + y(4) + health(1) + name_len(4)
    if data.len() < 17 {
        return Err(Error::MalformedData(
            "Manual: insufficient data".to_string(),
        ));
    }

    // Read player_id
    let player_id = u32::from_le_bytes(data[0..4].try_into().unwrap());

    // Read x coordinate
    let x = f32::from_le_bytes(data[4..8].try_into().unwrap());

    // Read y coordinate
    let y = f32::from_le_bytes(data[8..12].try_into().unwrap());

    // Read health
    let health = data[12];

    // Read name length
    let name_len = u32::from_le_bytes(data[13..17].try_into().unwrap()) as usize;

    // Verify we have enough data for the name
    if 17 + name_len > data.len() {
        return Err(Error::MalformedData(
            "Manual: name length exceeds data".to_string(),
        ));
    }

    // Read name
    let name = String::from_utf8(data[17..17 + name_len].to_vec())
        .map_err(|e| Error::MalformedData(format!("Manual: invalid UTF-8: {}", e)))?;

    Ok(Box::new(ManualMessage {
        player_id,
        x,
        y,
        health,
        name,
    }))
}

// ============================================================================
// Server
// ============================================================================

fn run_server(config: &Config, registry: &MessageRegistry) -> SocketAddr {
    let mut messenger = Messenger::new(config, registry).expect("Failed to create messenger");
    let (_listener_id, listener_addr) = messenger.listen("127.0.0.1:0").expect("Failed to listen");

    println!("[Server] Listening on {}", listener_addr);

    thread::spawn(move || {
        loop {
            let events = messenger.fetch_events().expect("Failed to fetch events");
            for event in events {
                match event {
                    MessengerEvent::Connected { id } => {
                        println!("[Server] Client {} connected", id);
                    }
                    MessengerEvent::Message { id, msg } => {
                        // Identify and log the message type
                        if msg.downcast_ref::<BincodeMessage>().is_some() {
                            println!("[Server] Received Bincode message from {}", id);
                        } else if msg.downcast_ref::<JsonMessage>().is_some() {
                            println!("[Server] Received JSON message from {}", id);
                        } else if msg.downcast_ref::<MessagePackMessage>().is_some() {
                            println!("[Server] Received MessagePack message from {}", id);
                        } else if msg.downcast_ref::<ManualMessage>().is_some() {
                            println!("[Server] Received Manual message from {}", id);
                        }

                        // Echo back the message
                        messenger.send_to(id, &*msg);
                    }
                    MessengerEvent::Disconnected { id } => {
                        println!("[Server] Client {} disconnected", id);
                    }
                    _ => {}
                }
            }
        }
    });

    listener_addr
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    println!("=== Custom Serialization Example ===");

    // Create config
    let config = Config::default();

    // Create message registry and register all message types
    let mut registry = MessageRegistry::new();

    // 1. Register Bincode message using the macro (easiest)
    println!("Registering BincodeMessage with macro...");
    rustcomm::bincode::register_message!(registry, BincodeMessage);

    // 2. Register JSON message manually with helper functions
    println!("Registering JsonMessage with custom JSON serialization...");
    registry.register(
        stringify!(JsonMessage),
        serialize_json_message::<JsonMessage>,
        deserialize_json_message::<JsonMessage>,
    );

    // 3. Register MessagePack message manually with helper functions
    println!("Registering MessagePackMessage with custom MessagePack serialization...");
    registry.register(
        stringify!(MessagePackMessage),
        serialize_msgpack_message::<MessagePackMessage>,
        deserialize_msgpack_message::<MessagePackMessage>,
    );

    // 4. Register Manual message with hand-written serialization
    println!("Registering ManualMessage with hand-written serialization...");
    registry.register(
        stringify!(ManualMessage),
        serialize_manual_message,
        deserialize_manual_message,
    );

    // Start the server
    let server_addr = run_server(&config, &registry);

    // Run the client
    let mut messenger = Messenger::new(&config, &registry).expect("Failed to create messenger");
    let (server_id, _peer_addr) = messenger
        .connect(server_addr)
        .expect("Failed to connect to server");

    println!("[Client] Connected to server");

    // Send Bincode message
    let bincode_msg = BincodeMessage {
        id: 42,
        data: vec![1, 2, 3, 4, 5],
    };
    println!("[Client] Sending Bincode message: {:?}", bincode_msg);
    messenger.send_to(server_id, &bincode_msg);

    // Send JSON message
    let json_msg = JsonMessage {
        name: "example".to_string(),
        value: 3.14159,
        active: true,
    };
    println!("[Client] Sending JSON message: {:?}", json_msg);
    messenger.send_to(server_id, &json_msg);

    // Send MessagePack message
    let mut metadata = HashMap::new();
    metadata.insert("author".to_string(), "alice".to_string());
    metadata.insert("version".to_string(), "1.0".to_string());

    let msgpack_msg = MessagePackMessage {
        timestamp: 1699372800,
        tags: vec!["important".to_string(), "urgent".to_string()],
        metadata,
    };
    println!("[Client] Sending MessagePack message: {:?}", msgpack_msg);
    messenger.send_to(server_id, &msgpack_msg);

    // Send Manual message
    let manual_msg = ManualMessage {
        player_id: 1337,
        x: 42.5,
        y: -17.25,
        health: 95,
        name: "PlayerOne".to_string(),
    };
    println!("[Client] Sending Manual message: {:?}", manual_msg);
    messenger.send_to(server_id, &manual_msg);

    // Receive echoed messages
    println!("[Client] Waiting for echoed messages...");
    let mut received_count = 0;
    loop {
        let events = messenger.fetch_events().expect("Failed to fetch events");
        for event in events {
            if let MessengerEvent::Message { msg, .. } = event {
                if let Some(received) = msg.downcast_ref::<BincodeMessage>() {
                    println!("[Client] Received Bincode echo: {:?}", received);
                    received_count += 1;
                } else if let Some(received) = msg.downcast_ref::<JsonMessage>() {
                    println!("[Client] Received JSON echo: {:?}", received);
                    received_count += 1;
                } else if let Some(received) = msg.downcast_ref::<MessagePackMessage>() {
                    println!("[Client] Received MessagePack echo: {:?}", received);
                    received_count += 1;
                } else if let Some(received) = msg.downcast_ref::<ManualMessage>() {
                    println!("[Client] Received Manual echo: {:?}", received);
                    received_count += 1;
                }

                if received_count == 4 {
                    println!("[Client] All messages received successfully!");
                    println!("=== Demo Complete ===");
                    return;
                }
            }
        }
    }
}
