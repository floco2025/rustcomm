#[path = "messages.rs"]
mod messages;

use clap::Parser;
use config::Config;
use messages::*;
use rustcomm::{register_bincode_message, Message, MessageRegistry, Messenger, MessengerEvent};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

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

    // Create the Server
    let mut server = Server::new();

    // The single-threaded event dispatch loop.
    loop {
        let events = match messenger.fetch_events() {
            Ok(events) => events,
            Err(err) => {
                eprintln!("Fatal error fetching messenger events: {err:?}");
                return ExitCode::FAILURE;
            }
        };

        for event in events {
            server.dispatch_event(event, &mut messenger);
        }
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
    clients: HashSet<usize>,
    /// Map of participant IDs to their participant data
    participants: HashMap<usize, Participant>,
}

impl Server {
    fn new() -> Self {
        Self {
            clients: HashSet::new(),
            participants: HashMap::new(),
        }
    }
}

// ============================================================================
// Server - Client Management
// ============================================================================

impl Server {
    fn add_client(&mut self, id: usize) {
        self.clients.insert(id);
    }

    fn remove_client(&mut self, id: usize) {
        self.clients.remove(&id);
        self.participants.remove(&id);
    }
}

// ============================================================================
// Server - Participant Management
// ============================================================================

impl Server {
    fn add_participant(&mut self, id: usize, participant: Participant) {
        self.participants.insert(id, participant);
    }

    fn modify_participant(&mut self, id: usize, participant: Participant) {
        if let Some(p) = self.participants.get_mut(&id) {
            *p = participant;
        }
    }

    fn is_participant(&self, id: usize) -> bool {
        self.participants.contains_key(&id)
    }

    fn get_participant_names(&self) -> Vec<(usize, String)> {
        self.participants
            .iter()
            .map(|(k, v)| (*k, v.name.clone()))
            .collect()
    }

    fn get_participant_ids(&self) -> Vec<usize> {
        self.participants.keys().copied().collect()
    }

    fn get_participant_name(&self, id: usize) -> Option<String> {
        self.participants.get(&id).map(|p| p.name.clone())
    }
}

// ============================================================================
// Server - Event Dispatching
// ============================================================================

impl Server {
    fn dispatch_event(&mut self, event: MessengerEvent, messenger: &mut Messenger) {
        match event {
            MessengerEvent::Inactive => panic!("Unexpected Inactive event"),
            MessengerEvent::Connected { id } => self.handle_connect(id, messenger),
            MessengerEvent::ConnectionFailed { .. } => panic!("Unexpected ConnectionFailed event"),
            MessengerEvent::Disconnected { id } => self.handle_disconnect(id, messenger),
            MessengerEvent::Message { id, msg } => self.handle_message(id, msg, messenger),
        }
    }
}

// ============================================================================
// Server - Event Handlers
// ============================================================================

impl Server {
    fn handle_connect(&mut self, id: usize, _messenger: &mut Messenger) {
        self.add_client(id);
    }

    fn handle_disconnect(&mut self, id: usize, messenger: &mut Messenger) {
        let was_participant = self.is_participant(id);
        self.remove_client(id);

        if was_participant {
            let ids = self.get_participant_ids();
            messenger.send_to_many(&ids, &SLogoff { id });
        }
    }

    fn handle_message(&mut self, id: usize, msg: Box<dyn Message>, messenger: &mut Messenger) {
        // Only process CLogin for non-participants; disconnect all other messages
        if !self.is_participant(id) {
            if let Some(msg) = msg.downcast_ref::<CLogin>() {
                // Validate name
                if msg.name.len() < 2 {
                    messenger.send_to(
                        id,
                        &SError {
                            message: "Login failed: Name must be at least 2 characters".to_string(),
                        },
                    );
                    self.remove_client(id);
                    messenger.close_connection(id);
                    return;
                }

                if msg.name.len() > 50 {
                    messenger.send_to(
                        id,
                        &SError {
                            message: "Login failed: Name too long (max 50 characters)".to_string(),
                        },
                    );
                    self.remove_client(id);
                    messenger.close_connection(id);
                    return;
                }

                self.add_participant(id, Participant::new(msg.name.clone()));
                let logins = self.get_participant_names();
                messenger.send_to(id, &SInit { id, logins });
                let ids = self.get_participant_ids();
                messenger.send_to_many(
                    &ids,
                    &SLogin {
                        id,
                        name: msg.name.clone(),
                    },
                );
            } else {
                // Clients that don't send CLogin first are disconnected (protocol violation)
                self.remove_client(id);
                messenger.close_connection(id);
            }
            return;
        }

        // Process messages from participants
        if let Some(msg) = msg.downcast_ref::<CSay>() {
            let ids = self.get_participant_ids();
            messenger.send_to_many(
                &ids,
                &SSay {
                    id,
                    text: msg.text.clone(),
                },
            );
        } else if let Some(msg) = msg.downcast_ref::<CName>() {
            if msg.name.len() < 2 {
                messenger.send_to(
                    id,
                    &SError {
                        message: "Name change failed: Name must be at least 2 characters"
                            .to_string(),
                    },
                );
                return;
            }

            if msg.name.len() > 50 {
                messenger.send_to(
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
            messenger.send_to_many(
                &ids,
                &SName {
                    id,
                    name: msg.name.clone(),
                },
            );
        } else if let Some(msg) = msg.downcast_ref::<CRemove>() {
            if self.get_participant_name(msg.id).is_some() {
                self.remove_client(msg.id);
                messenger.close_connection(msg.id);
                let ids = self.get_participant_ids();
                messenger.send_to_many(&ids, &SRemove { id: msg.id });
            } else {
                messenger.send_to(
                    id,
                    &SError {
                        message: format!("Cannot remove participant {}: not found", msg.id),
                    },
                );
            }
        } else {
            // Clients that send invalid messages are disconnected (protocol violation)
            self.remove_client(id);
            messenger.close_connection(id);
            let ids = self.get_participant_ids();
            messenger.send_to_many(&ids, &SRemove { id });
        }
    }
}
