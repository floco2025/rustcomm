#[path = "messages.rs"]
mod messages;

use clap::Parser;
use config::Config;
use messages::*;
use rustcomm::{
    register_bincode_message, Message, MessageRegistry, Messenger, MessengerEvent,
    MessengerInterface,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use tracing_subscriber::EnvFilter;

// ============================================================================
// Constants
// ============================================================================

const THREAD_POOL_SIZE: usize = 5;

// ============================================================================
// Tracing Initialization
// ============================================================================

/// Initialize tracing for rustcomm crate based on verbosity level
fn init_tracing(verbosity: u8) {
    let level = match verbosity {
        0 => return, // No tracing
        1 => "info",
        2 => "debug",
        _ => "trace", // 3 or more
    };

    let filter = format!("rustcomm={}", level);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_target(true)
        .with_writer(std::io::stderr)
        .pretty()
        .init();
}

// ============================================================================
// CLI Argument Parsing
// ============================================================================

#[derive(Parser)]
#[command(author, version, about = "Chat server", long_about = None)]
struct Args {
    /// Address to bind server to
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    bind: String,

    /// Increase logging verbosity (-v: info, -vv: debug, -vvv: trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Configuration file path (TOML format)
    #[arg(long)]
    config: Option<String>,
}

// ============================================================================
// Main Entry Point
// ============================================================================

fn main() -> ExitCode {
    let args = Args::parse();

    // Initialize tracing for messaging crate
    init_tracing(args.verbose);

    // Create config - load from file if specified, otherwise use defaults
    let config = if let Some(config_path) = &args.config {
        match Config::builder()
            .add_source(config::File::with_name(config_path))
            .build()
        {
            Ok(c) => c,
            Err(err) => {
                eprintln!("Failed to load config file '{}': {}", config_path, err);
                return ExitCode::FAILURE;
            }
        }
    } else {
        Config::default()
    };

    // Create message registry
    let mut registry = MessageRegistry::new();
    register_bincode_message!(registry, CLogin);
    register_bincode_message!(registry, CName);
    register_bincode_message!(registry, CSay);
    register_bincode_message!(registry, CRemove);

    // Transport type (TCP/TLS) is selected automatically based on config

    // Start up messenger
    let mut messenger = match Messenger::new(&config, &registry) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("Failed to initialize messenger: {err:?}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = messenger.listen(&args.bind) {
        eprintln!("Failed to listen on {}: {err:?}", args.bind);
        return ExitCode::FAILURE;
    }

    let messenger_intf = messenger.get_messenger_interface();
    let messenger_arc = Arc::new(Mutex::new(messenger));

    // Create the Server
    let server_arc = Arc::new(Server::new());

    // Start the server thread pool.
    //
    // We don't really need a thread pool here, since we could run everything in
    // a single thread. But this is a demo, so we use a thread pool, just to
    // show the technique.
    //
    // If we wouldn't have a thread pool, we could remove all all the Arcs,
    // Mutexes, and clones. We also wouldn't need the MessengerInterface, since
    // we could call methods on Messenger directly.
    //
    // We use VecDeque for efficient FIFO queue operations - worker threads
    // pop from the front, which is O(1) with VecDeque but O(n) with Vec.
    let events = Arc::new(Mutex::new(VecDeque::new()));
    let mut handles = vec![];
    for _i in 0..THREAD_POOL_SIZE {
        let server_arc_clone = server_arc.clone();
        let messenger_arc_clone = messenger_arc.clone();
        let messenger_intf_clone = messenger_intf.clone();
        let events_clone = events.clone();
        let handle = thread::spawn(move || {
            worker_loop(
                server_arc_clone,
                messenger_arc_clone,
                messenger_intf_clone,
                events_clone,
            )
        });
        handles.push(handle);
    }

    // Now we just wait for all the threads from the thread pool to finish.
    let mut exit_code = ExitCode::SUCCESS;
    for handle in handles {
        let result = handle.join().unwrap();
        if result == ExitCode::FAILURE {
            exit_code = ExitCode::FAILURE;
        }
    }

    exit_code
}

// ============================================================================
// Worker Thread
// ============================================================================

fn worker_loop(
    server_arc: Arc<Server>,
    messenger_arc: Arc<Mutex<Messenger>>,
    messenger_intf: MessengerInterface,
    events: Arc<Mutex<VecDeque<MessengerEvent>>>,
) -> ExitCode {
    loop {
        // Lock the event queue so that no other thread can access it.
        let mut events_guard = events.lock().unwrap();
        while events_guard.is_empty() {
            let mut messenger_guard = messenger_arc.lock().unwrap();
            let new_events = match messenger_guard.fetch_events() {
                Ok(new_events) => new_events,
                Err(err) => {
                    eprintln!("Fatal error fetching messenger events: {err:?}");
                    return ExitCode::FAILURE;
                }
            };

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
        server_arc.dispatch_event(event, &messenger_intf);
    }
}

// ============================================================================
// Server Implementation
// ============================================================================

struct Participant {
    name: String,
}

impl Participant {
    fn new(name: String) -> Self {
        Self { name }
    }
}

/// Chat server that manages connected clients.
///
/// Terminology:
/// - **Client**: Any connection (in the clients map)
/// - **Participant**: A logged-in client with a name (in both clients and participants maps)
///
/// When a client first connects, they are added to the clients map only. Once they send
/// `CLogin`, they are added to the participants map. Only participants can send chat
/// messages and appear in the participant list. All messages except `CLogin` are ignored
/// from clients who are not yet participants.
struct Server {
    /// Set of all connected client IDs
    clients: Mutex<HashSet<usize>>,
    /// Map of participant IDs to their participant data
    participants: Mutex<HashMap<usize, Participant>>,
}

impl Server {
    fn new() -> Self {
        Self {
            clients: Mutex::new(HashSet::new()),
            participants: Mutex::new(HashMap::new()),
        }
    }
}

// ============================================================================
// Server - Client Management
// ============================================================================

impl Server {
    fn add_client(&self, id: usize) {
        let mut clients = self.clients.lock().unwrap();
        clients.insert(id);
    }

    fn remove_client(&self, id: usize) {
        let mut clients = self.clients.lock().unwrap();
        clients.remove(&id);
        let mut participants = self.participants.lock().unwrap();
        participants.remove(&id);
    }
}

// ============================================================================
// Server - Participant Management
// ============================================================================

impl Server {
    fn add_participant(&self, id: usize, participant: Participant) {
        let mut participants = self.participants.lock().unwrap();
        participants.insert(id, participant);
    }

    fn modify_participant(&self, id: usize, participant: Participant) {
        let mut participants = self.participants.lock().unwrap();
        if let Some(p) = participants.get_mut(&id) {
            *p = participant;
        }
    }

    fn is_participant(&self, id: usize) -> bool {
        self.participants.lock().unwrap().contains_key(&id)
    }

    fn get_participant_names(&self) -> Vec<(usize, String)> {
        self.participants
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, v.name.clone()))
            .collect()
    }

    fn get_participant_ids(&self) -> Vec<usize> {
        self.participants.lock().unwrap().keys().copied().collect()
    }

    fn get_participant_name(&self, id: usize) -> Option<String> {
        self.participants
            .lock()
            .unwrap()
            .get(&id)
            .map(|p| p.name.clone())
    }
}

// ============================================================================
// Server - Event Dispatching
// ============================================================================

impl Server {
    fn dispatch_event(&self, event: MessengerEvent, messenger_intf: &MessengerInterface) {
        match event {
            MessengerEvent::Inactive => panic!("Unexpected Inactive event"),
            MessengerEvent::Connected { id } => self.handle_connect(id, messenger_intf),
            MessengerEvent::ConnectionFailed { .. } => panic!("Unexpected ConnectionFailed event"),
            MessengerEvent::Disconnected { id } => self.handle_disconnect(id, messenger_intf),
            MessengerEvent::Message { id, msg } => self.handle_message(id, msg, messenger_intf),
        }
    }
}

// ============================================================================
// Server - Event Handlers
// ============================================================================

impl Server {
    fn handle_connect(&self, id: usize, _messenger_intf: &MessengerInterface) {
        self.add_client(id);
    }

    fn handle_disconnect(&self, id: usize, messenger_intf: &MessengerInterface) {
        let was_participant = self.is_participant(id);
        self.remove_client(id);

        if was_participant {
            let ids = self.get_participant_ids();
            messenger_intf.send_to_many(ids, &SLogoff { id });
        }
    }

    fn handle_message(
        &self,
        id: usize,
        msg: Box<dyn Message>,
        messenger_intf: &MessengerInterface,
    ) {
        // Only process CLogin for non-participants; disconnect all other messages
        if !self.is_participant(id) {
            if let Some(msg) = msg.downcast_ref::<CLogin>() {
                if msg.name.len() < 2 {
                    messenger_intf.send_to(
                        id,
                        &SError {
                            message: "Login failed: Name must be at least 2 characters".to_string(),
                        },
                    );
                    self.remove_client(id);
                    messenger_intf.close_connection(id);
                    return;
                }

                if msg.name.len() > 50 {
                    messenger_intf.send_to(
                        id,
                        &SError {
                            message: "Login failed: Name too long (max 50 characters)".to_string(),
                        },
                    );
                    self.remove_client(id);
                    messenger_intf.close_connection(id);
                    return;
                }

                self.add_participant(id, Participant::new(msg.name.clone()));
                let logins = self.get_participant_names();
                messenger_intf.send_to(id, &SInit { id, logins });
                let ids = self.get_participant_ids();
                messenger_intf.send_to_many(
                    ids,
                    &SLogin {
                        id,
                        name: msg.name.clone(),
                    },
                );
            } else {
                // Clients that don't send CLogin first are disconnected (protocol violation)
                self.remove_client(id);
                messenger_intf.close_connection(id);
            }
            return;
        }

        // Process messages from participants
        if let Some(msg) = msg.downcast_ref::<CSay>() {
            let ids = self.get_participant_ids();
            messenger_intf.send_to_many(
                ids,
                &SSay {
                    id,
                    text: msg.text.clone(),
                },
            );
        } else if let Some(msg) = msg.downcast_ref::<CName>() {
            if msg.name.len() < 2 {
                messenger_intf.send_to(
                    id,
                    &SError {
                        message: "Name change failed: Name must be at least 2 characters"
                            .to_string(),
                    },
                );
                return;
            }

            if msg.name.len() > 50 {
                messenger_intf.send_to(
                    id,
                    &SError {
                        message: "Name change failed: Name too long (max 50 characters)"
                            .to_string(),
                    },
                );
                return;
            }

            self.modify_participant(id, Participant::new(msg.name.clone()));
            let ids = self.get_participant_ids();
            messenger_intf.send_to_many(
                ids,
                &SName {
                    id,
                    name: msg.name.clone(),
                },
            );
        } else if let Some(msg) = msg.downcast_ref::<CRemove>() {
            if self.get_participant_name(msg.id).is_some() {
                self.remove_client(msg.id);
                messenger_intf.close_connection(msg.id);
                let ids = self.get_participant_ids();
                messenger_intf.send_to_many(ids, &SRemove { id: msg.id });
            } else {
                messenger_intf.send_to(
                    id,
                    &SError {
                        message: format!("Cannot remove participant {}: not found", msg.id),
                    },
                );
            }
        } else {
            // Clients that send invalid messages are disconnected (protocol violation)
            self.remove_client(id);
            messenger_intf.close_connection(id);
            let ids = self.get_participant_ids();
            messenger_intf.send_to_many(ids, &SRemove { id });
        }
    }
}
