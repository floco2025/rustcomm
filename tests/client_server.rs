//! Client/Server Integration Tests
//!
//! # Running with tracing
//!
//! Use TEST_LOG environment variable to control tracing verbosity (like -v, -vv, -vvv):
//!
//! ```bash
//! # Info level (equivalent to -v)
//! TEST_LOG=1 cargo test client_server_tls -- --nocapture
//!
//! # Debug level (equivalent to -vv)
//! TEST_LOG=2 cargo test client_server_tls -- --nocapture
//!
//! # Trace level (equivalent to -vvv)
//! TEST_LOG=3 cargo test client_server_tls -- --nocapture
//! ```

mod message_types;

use message_types::*;
use rustcomm::prelude::*;
use std::any::type_name;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Once;
use std::thread;

static INIT: Once = Once::new();

// ============================================================================
// Tracing Initialization
// ============================================================================

/// Initialize tracing based on TEST_LOG environment variable
///
/// Verbosity levels (like -v, -vv, -vvv):
/// - TEST_LOG=1: Info level
/// - TEST_LOG=2: Debug level  
/// - TEST_LOG=3: Trace level
///
/// Example: TEST_LOG=2 cargo test client_server_tls -- --nocapture
fn init_tracing() {
    INIT.call_once(|| {
        if let Ok(level_str) = std::env::var("TEST_LOG") {
            let verbosity = level_str.parse::<u8>().unwrap_or(0);

            if verbosity > 0 {
                let level = match verbosity {
                    1 => "info",
                    2 => "debug",
                    _ => "trace", // 3 or more
                };

                let filter = format!("rustcomm={}", level);
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
                    .with_target(true)
                    .with_writer(std::io::stderr)
                    .with_test_writer()
                    .try_init();
            }
        }
    });
}

// ============================================================================
// Helper Functions
// ============================================================================

fn register_all_messages(registry: &mut MessageRegistry) {
    register_bincode_message!(registry, SimpleMessage);
    register_bincode_message!(registry, ByteVector);
    register_bincode_message!(registry, TextMessage);
    register_bincode_message!(registry, OptionalMessage);
    register_bincode_message!(registry, ListMessage);
    register_bincode_message!(registry, NestedMessage);
    register_bincode_message!(registry, EnumMessage);
    register_bincode_message!(registry, TupleMessage);
    register_bincode_message!(registry, NumericMessage);
    register_bincode_message!(registry, EmptyMessage);
    register_bincode_message!(registry, ComplexMessage);
    register_bincode_message!(registry, HashMapMessage);
    register_bincode_message!(registry, NestedHashMapMessage);
    register_bincode_message!(registry, CompositeMessage);
    register_bincode_message!(registry, BatchMessage);
    register_bincode_message!(registry, MessageMapMessage);
    register_bincode_message!(registry, UltraNestedMessage);
}

fn build_config(transport_type: &str) -> config::Config {
    config::Config::builder()
        .set_default("transport_type", transport_type)
        .unwrap()
        .set_default("tls_server_cert", "tests/cert.pem")
        .unwrap()
        .set_default("tls_server_key", "tests/key.pem")
        .unwrap()
        .set_default("tls_ca_cert", "tests/cert.pem")
        .unwrap()
        .build()
        .unwrap()
}

fn send_and_receive<T>(messenger: &mut Messenger, server_id: usize, message: T)
where
    T: Message + Clone + PartialEq + std::fmt::Debug,
{
    // Single-threaded, so no need for MessengerInterface
    messenger.send_to(server_id, &message.clone());

    let mut events = VecDeque::from(
        messenger
            .fetch_events()
            .expect("Failed to fetch client events"),
    );

    let event = events.pop_front().expect("[Client] No events returned");

    match event {
        MessengerEvent::Inactive => {
            panic!("[Client] Unexpected Inactive event")
        }
        MessengerEvent::Connected { .. } => {
            panic!("[Client] Unexpected Connected event")
        }
        MessengerEvent::ConnectionFailed { .. } => {
            panic!("[Client] Unexpected ConnectionFailed event")
        }
        MessengerEvent::Disconnected { .. } => {
            panic!("[Client] Unexpected Disconnected event")
        }
        MessengerEvent::Message { id, msg, .. } => {
            assert_eq!(id, server_id);
            if let Some(return_message) = msg.downcast_ref::<T>() {
                assert_eq!(return_message, &message);
            } else {
                let type_name = type_name::<T>()
                    .rsplit_once(':')
                    .map(|(_, name)| name)
                    .unwrap_or(type_name::<T>());

                panic!(
                    "[Client] Received message type {}, expected message type {}",
                    msg.message_id(),
                    type_name
                );
            }
        }
    }

    assert!(
        events.is_empty(),
        "[Client] Unexpected additional events: {:?}",
        events
    );
}

// ============================================================================
// Client/Server Test
// ============================================================================

#[test]
fn client_server_tcp() {
    client_server("tcp");
}

#[test]
fn client_server_tls() {
    client_server("tls");
}

fn client_server(transport_type: &str) {
    // Initialize tracing (controlled by TEST_LOG environment variable)
    init_tracing();

    let config = build_config(transport_type);

    let mut registry = MessageRegistry::new();
    register_all_messages(&mut registry);

    // Start server
    let server_messenger =
        Messenger::new(&config, &registry).expect("Failed to create server messenger");
    let local_addr = run_server(server_messenger);

    // Create client
    let client_messenger =
        Messenger::new(&config, &registry).expect("Failed to create client messenger");

    client_server_with_messenger(client_messenger, local_addr);
}

fn client_server_with_messenger(mut messenger: Messenger, local_addr: SocketAddr) {
    let (server_id, _peer_addr) = messenger
        .connect(local_addr)
        .expect("Failed to connect to server");

    let mut events = VecDeque::from(
        messenger
            .fetch_events()
            .expect("Failed to fetch client events"),
    );

    let event = events.pop_front().expect("[Client] No events returned");

    match event {
        MessengerEvent::Connected { .. } => {}
        MessengerEvent::Inactive => {
            panic!("[Client] Unexpected Inactive event")
        }
        MessengerEvent::ConnectionFailed { .. } => {
            panic!("[Client] Unexpected ConnectionFailed event")
        }
        MessengerEvent::Disconnected { .. } => {
            panic!("[Client] Unexpected Disconnected event")
        }
        MessengerEvent::Message { .. } => {
            panic!("[Client] Received message before Connected event")
        }
    }

    assert!(
        events.is_empty(),
        "[Client] Unexpected additional events: {:?}",
        events
    );

    // Test SimpleMessage
    {
        let message = SimpleMessage {
            id: 42,
            value: -123,
            flag: true,
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test TextMessage
    {
        let message = TextMessage {
            text: "Test Message".to_string(),
            count: 42,
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test OptionalMessage
    {
        let message = OptionalMessage {
            required: 100,
            optional_int: Some(-456),
            optional_string: Some("present".to_string()),
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test ListMessage
    {
        let message = ListMessage {
            items: vec![1, 2, 3, 4, 5],
            names: vec!["Alice".to_string(), "Bob".to_string()],
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test NestedMessage
    {
        let message = NestedMessage {
            id: 999,
            position: Point { x: 10.5, y: 20.3 },
            waypoints: vec![Point { x: 1.0, y: 2.0 }, Point { x: 3.0, y: 4.0 }],
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test EnumMessage
    {
        let message = EnumMessage {
            player_id: 123,
            action: Action::Move { x: 10.0, y: 20.0 },
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test TupleMessage
    {
        let message = TupleMessage(42, "test".to_string(), true);
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test NumericMessage
    {
        let message = NumericMessage {
            u8_val: 255,
            u16_val: 65535,
            u32_val: 4294967295,
            u64_val: 18446744073709551615,
            i8_val: -128,
            i16_val: -32768,
            i32_val: -2147483648,
            i64_val: -9223372036854775808,
            f32_val: std::f32::consts::PI,
            f64_val: std::f64::consts::E,
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test EmptyMessage
    {
        let message = EmptyMessage;
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test ComplexMessage
    {
        let message = ComplexMessage {
            id: 1,
            tags: Some(vec!["tag1".to_string(), "tag2".to_string()]),
            scores: Some(vec![100, 200, 300]),
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test HashMapMessage
    {
        let mut player_scores = HashMap::new();
        player_scores.insert("Alice".to_string(), 100);
        player_scores.insert("Bob".to_string(), 200);

        let mut config = HashMap::new();
        config.insert("difficulty".to_string(), "hard".to_string());

        let message = HashMapMessage {
            player_scores,
            config,
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test NestedHashMapMessage
    {
        let mut player_data = HashMap::new();
        player_data.insert(1, Point { x: 10.0, y: 20.0 });
        player_data.insert(2, Point { x: 30.0, y: 40.0 });

        let mut tags_per_player = HashMap::new();
        tags_per_player.insert(1, vec!["admin".to_string(), "vip".to_string()]);
        tags_per_player.insert(2, vec!["player".to_string()]);

        let message = NestedHashMapMessage {
            player_data,
            tags_per_player,
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test CompositeMessage
    {
        let message = CompositeMessage {
            id: 1,
            simple: SimpleMessage {
                id: 42,
                value: -100,
                flag: true,
            },
            optional_text: Some(TextMessage {
                text: "nested text".to_string(),
                count: 5,
            }),
            nested: NestedMessage {
                id: 999,
                position: Point { x: 1.5, y: 2.5 },
                waypoints: vec![Point { x: 10.0, y: 20.0 }],
            },
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test BatchMessage
    {
        let message = BatchMessage {
            batch_id: 3,
            simple_messages: vec![
                SimpleMessage {
                    id: 1,
                    value: 100,
                    flag: true,
                },
                SimpleMessage {
                    id: 2,
                    value: 200,
                    flag: false,
                },
            ],
            points: vec![Point { x: 1.0, y: 2.0 }, Point { x: 3.0, y: 4.0 }],
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test MessageMapMessage
    {
        let mut messages_by_id = HashMap::new();
        messages_by_id.insert(
            1,
            SimpleMessage {
                id: 1,
                value: 100,
                flag: true,
            },
        );
        messages_by_id.insert(
            2,
            SimpleMessage {
                id: 2,
                value: 200,
                flag: false,
            },
        );

        let mut positions_by_player = HashMap::new();
        positions_by_player.insert("Alice".to_string(), Point { x: 10.0, y: 20.0 });
        positions_by_player.insert("Bob".to_string(), Point { x: 30.0, y: 40.0 });

        let message = MessageMapMessage {
            map_id: 3,
            messages_by_id,
            positions_by_player,
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test UltraNestedMessage
    {
        let batch = BatchMessage {
            batch_id: 10,
            simple_messages: vec![SimpleMessage {
                id: 1,
                value: 100,
                flag: true,
            }],
            points: vec![Point { x: 1.0, y: 2.0 }],
        };

        let mut composite_map = HashMap::new();
        composite_map.insert(
            "player1".to_string(),
            CompositeMessage {
                id: 1,
                simple: SimpleMessage {
                    id: 10,
                    value: 500,
                    flag: true,
                },
                optional_text: Some(TextMessage {
                    text: "hello".to_string(),
                    count: 1,
                }),
                nested: NestedMessage {
                    id: 20,
                    position: Point { x: 0.0, y: 0.0 },
                    waypoints: vec![],
                },
            },
        );

        let optional_batches = Some(vec![BatchMessage {
            batch_id: 100,
            simple_messages: vec![SimpleMessage {
                id: 999,
                value: -1,
                flag: true,
            }],
            points: vec![],
        }]);

        let message = UltraNestedMessage {
            id: 1,
            batch,
            composite_map,
            optional_batches,
        };
        send_and_receive(&mut messenger, server_id, message);
    }

    // Test large message
    {
        let message = ByteVector {
            data: vec![42u8; 2 * 1024 * 1024],
        };

        send_and_receive(&mut messenger, server_id, message);
    }
}

fn run_server(messenger: Messenger) -> SocketAddr {
    let mut messenger = messenger;
    let (_listener_id, listener_addr) = messenger.listen("127.0.0.1:0").expect("Failed to listen");
    let messenger_intf = messenger.get_messenger_interface();

    thread::spawn(move || {
        let mut connected = false;
        'outer: loop {
            let events = messenger
                .fetch_events()
                .expect("Failed to fetch server events");

            for event in events {
                match event {
                    MessengerEvent::Inactive { .. } => {
                        panic!("[Server] Unexpected Inactive event")
                    }
                    MessengerEvent::Connected { .. } => {
                        if connected {
                            panic!("[Server] Unexpected Connected event")
                        }
                        connected = true;
                    }
                    MessengerEvent::ConnectionFailed { .. } => {
                        panic!("[Server] Unexpected ConnectionFailed event")
                    }
                    MessengerEvent::Disconnected { .. } => {
                        break 'outer;
                    }
                    MessengerEvent::Message { id, msg, .. } => {
                        messenger_intf.send_to(id, &*msg);
                    }
                }
            }
        }

        assert!(connected, "[Server] No Connected event received");
    });

    listener_addr
}
