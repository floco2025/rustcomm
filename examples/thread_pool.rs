//! Minimal thread pool demonstration using rustcomm messaging.
//!
//! This example demonstrates how a thread pool can process multiple requests in
//! parallel. A client sends 10 ProcessData messages to a server. The server
//! uses a thread pool of 5 threads to process requests concurrently, with each
//! request taking 1 second to process.
//!
//! With 5 threads processing 10 messages (2 batches), total time is ~2 seconds
//! instead of the 10 seconds it would take sequentially.
//!
//! This example also demonstrates graceful server shutdown. After the client
//! completes, it calls `close_all()` on the server interface, which closes
//! all connections and listeners. When worker threads call `fetch_events()`,
//! they receive an `Inactive` event and exit cleanly.

use bincode::{Decode, Encode};
use config::Config;
use rustcomm::{
    impl_message, register_bincode_message, MessageRegistry, Messenger, MessengerEvent,
    MessengerInterface,
};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

// ============================================================================
// Messages
// ============================================================================

#[derive(Encode, Decode, Debug)]
struct ProcessData {
    id: u32,
}
impl_message!(ProcessData);

#[derive(Encode, Decode, Debug)]
struct ProcessingComplete {
    id: u32,
}
impl_message!(ProcessingComplete);

const THREAD_POOL_SIZE: usize = 5;
const NUM_MESSAGES: u32 = 10;
const PROCESSING_TIME_MS: u64 = 1000;

fn run_server(
    config: &Config,
    registry: &MessageRegistry,
) -> (MessengerInterface, SocketAddr, Vec<JoinHandle<()>>) {
    // Create message registry
    let mut messenger = Messenger::new(config, registry).expect("Failed to create messenger");
    let (_listener_id, listener_addr) = messenger.listen("127.0.0.1:0").expect("Failed to listen");
    let messenger_intf = messenger.get_messenger_interface();
    let messenger_arc = Arc::new(Mutex::new(messenger));
    let events = Arc::new(Mutex::new(VecDeque::new()));

    println!(
        "[Server] Starting with {} threads, listening on {}",
        THREAD_POOL_SIZE, listener_addr
    );

    // Start the threads for our thread pool
    let mut handles = vec![];
    for _ in 0..THREAD_POOL_SIZE {
        let messenger = messenger_arc.clone();
        let messenger_intf = messenger_intf.clone();
        let events = events.clone();
        let handle = thread::spawn(move || worker_loop(messenger, messenger_intf, events));
        handles.push(handle);
    }

    (messenger_intf, listener_addr, handles)
}

fn worker_loop(
    messenger_arc: Arc<Mutex<Messenger>>,
    messenger_intf: MessengerInterface,
    events: Arc<Mutex<VecDeque<MessengerEvent>>>,
) {
    loop {
        // Lock the event queue so that no other thread can access it.
        let mut events_guard = events.lock().unwrap();

        // If the event queue is empty, we will fetch new events.
        while events_guard.is_empty() {
            let mut messenger_guard = messenger_arc.lock().unwrap();
            let new_events = messenger_guard
                .fetch_events()
                .expect("Failed to fetch events");

            // Convert Vec to VecDeque for efficient FIFO queue operations.
            // Worker threads use pop_front() to process events in the order
            // they occurred, ensuring correct sequencing.
            *events_guard = VecDeque::from(new_events);
        }

        // We want each thread to only process one event at a time. Other
        // threads will pick up additional events, if there are any. Using
        // pop_front() gives us FIFO (first-in, first-out) order.
        let event = events_guard.pop_front().unwrap();

        // We must release the lock before dispatch, so that other threads can
        // run.
        drop(events_guard);

        // Now this thread can process the event.
        if dispatch_event(event, &messenger_intf) {
            break; // dispatch_event returns true when we are done
        }
    }
}

fn dispatch_event(event: MessengerEvent, messenger_intf: &MessengerInterface) -> bool {
    match event {
        MessengerEvent::Inactive => {
            let thread_id = thread::current().id();
            println!("[Server] Thread {:?} terminating", thread_id);
            return true; // We are done
        }
        MessengerEvent::Message { id, msg } => {
            if let Some(msg) = msg.downcast_ref::<ProcessData>() {
                let thread_id = thread::current().id();
                println!(
                    "[Server] Thread {:?} processing message {}...",
                    thread_id, msg.id
                );

                // Simulate processing time
                thread::sleep(Duration::from_millis(PROCESSING_TIME_MS));

                println!(
                    "[Server] Thread {:?} completed message {}",
                    thread_id, msg.id
                );

                messenger_intf.send_to(id, &ProcessingComplete { id: msg.id });
            }
        }
        _ => {}
    }

    false
}

fn main() {
    // Create config
    let config = Config::default();

    // Create message registry
    let mut registry = MessageRegistry::new();
    register_bincode_message!(registry, ProcessData);
    register_bincode_message!(registry, ProcessingComplete);

    // Start the server with a thread pool
    let (server_intf, server_addr, handles) = run_server(&config, &registry);

    // Run the client in this thread
    let mut messenger = Messenger::new(&config, &registry).expect("Failed to create messenger");
    let (conn_id, _peer_addr) = messenger
        .connect(server_addr)
        .expect("Failed to connect to server");

    println!(
        "[Client] Connected to server, sending {} messages",
        NUM_MESSAGES
    );
    let start = Instant::now();

    // Send all messages
    for i in 0..NUM_MESSAGES {
        messenger.send_to(conn_id, &ProcessData { id: i });
    }

    // Wait for all responses
    let mut completed = 0;
    while completed < NUM_MESSAGES {
        let events = messenger.fetch_events().expect("Failed to fetch events");
        for event in events {
            if let MessengerEvent::Message { msg, .. } = event {
                if let Some(msg) = msg.downcast_ref::<ProcessingComplete>() {
                    println!("[Client] Received completion for message {}", msg.id);
                    completed += 1;
                }
            }
        }
    }

    let elapsed = start.elapsed();
    println!(
        "[Client] All {} messages processed in {:.2} seconds",
        NUM_MESSAGES,
        elapsed.as_secs_f64()
    );
    println!(
        "[Client] With {} threads, expected time: ~{} seconds (vs {} seconds sequential)",
        THREAD_POOL_SIZE,
        (NUM_MESSAGES as f64 / THREAD_POOL_SIZE as f64).ceil() as u64,
        NUM_MESSAGES
    );

    // Shut down the server by closing all of its connections and listeners. The
    // server will then get an Inactive event.
    server_intf.close_all();

    // Wait for all server threads to finish
    for handle in handles {
        let _ = handle.join();
    }
}
