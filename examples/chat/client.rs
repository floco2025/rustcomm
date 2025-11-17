#[path = "messages.rs"]
mod messages;

use clap::Parser;
use config::Config;
use messages::*;
use rustcomm::{
    register_bincode_message, Message, MessageRegistry, Messenger, MessengerEvent,
    MessengerInterface,
};
use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
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
// CLI Arguments
// ============================================================================

#[derive(Parser, Debug)]
#[command(author, version, about = "Chat client", long_about = None)]
struct Args {
    /// Server address to connect to
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    server: String,

    /// Login name
    #[arg(short, long, default_value_t = get_login_name())]
    name: String,

    /// Increase logging verbosity (-v: info, -vv: debug, -vvv: trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Skip sending login message (for testing server protocol enforcement)
    #[arg(long)]
    no_login: bool,

    /// Configuration file path (TOML format)
    #[arg(long)]
    config: Option<String>,
}

fn get_login_name() -> String {
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

// ============================================================================
// Main Entry Point
// ============================================================================

fn main() -> ExitCode {
    let args = Args::parse();

    // Initialize tracing
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
    register_bincode_message!(registry, SInit);
    register_bincode_message!(registry, SLogin);
    register_bincode_message!(registry, SLogoff);
    register_bincode_message!(registry, SRemove);
    register_bincode_message!(registry, SSay);
    register_bincode_message!(registry, SName);
    register_bincode_message!(registry, SError);

    // Transport type (TCP/TLS) is selected automatically based on config

    // Start up messenger
    let mut messenger = match Messenger::new(&config, &registry) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("Failed to initialize messenger: {err:?}");
            return ExitCode::FAILURE;
        }
    };
    let server_id = match messenger.connect(&args.server) {
        Ok((id, _peer_addr)) => {
            println!("Connection to server initiated");
            id
        }
        Err(err) => {
            eprintln!("Failed to connect to server: {err:?}");
            return ExitCode::FAILURE;
        }
    };
    let messenger_intf = messenger.get_messenger_interface();

    // Create the Client
    let client_arc = Arc::new(Client::new());

    // Send login (unless --no-login is specified for testing)
    if !args.no_login {
        messenger_intf.send_to(server_id, &CLogin { name: args.name });
    }

    // Start the input thread
    let client_arc_clone = client_arc.clone();
    let messenger_intf_clone = messenger_intf.clone();
    spawn_input_thread(client_arc_clone, messenger_intf_clone, server_id);

    // Execute the client event loop
    loop {
        let events = match messenger.fetch_events() {
            Ok(events) => events,
            Err(err) => {
                eprintln!("Fatal error fetching events: {err:?}");
                return ExitCode::FAILURE;
            }
        };

        for event in events {
            if let Some(exit_code) = client_arc.dispatch_event(event, &messenger_intf, server_id) {
                return exit_code;
            }
        }
    }
}

// ============================================================================
// Input Thread
// ============================================================================

fn spawn_input_thread(
    client_arc: Arc<Client>,
    messenger_intf: MessengerInterface,
    server_id: usize,
) {
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(text) if text.starts_with('/') => {
                    command(&client_arc, &text, &messenger_intf, server_id);
                }
                Ok(text) if !text.is_empty() => {
                    messenger_intf.send_to(server_id, &CSay { text });
                }
                Ok(_) => {} // Empty line, ignore
                Err(e) => {
                    eprintln!("Failed to read input: {}", e);
                    break;
                }
            }
        }
    });
}

fn command(
    client_arc: &Arc<Client>,
    command: &str,
    messenger_intf: &MessengerInterface,
    server_id: usize,
) {
    let parts: Vec<&str> = command[1..].splitn(2, ' ').collect();

    match parts.get(0) {
        Some(&"name") => {
            if let Some(name) = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                messenger_intf.send_to(
                    server_id,
                    &CName {
                        name: name.to_string(),
                    },
                );
            } else {
                println!("Usage: /name <new name>");
            }
        }
        Some(&"login") => {
            if let Some(name) = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                messenger_intf.send_to(
                    server_id,
                    &CLogin {
                        name: name.to_string(),
                    },
                );
            } else {
                println!("Usage: /login <name>");
            }
        }
        Some(&"who") => {
            let names = client_arc.get_all_participant_names();
            if names.is_empty() {
                println!("No participants.");
            } else {
                println!("Participants:");
                for name in names {
                    println!("  {}", name);
                }
            }
        }
        Some(&"remove") => {
            if let Some(name) = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                match client_arc.get_participant_id_by_name(name) {
                    Some(id) => {
                        messenger_intf.send_to(server_id, &CRemove { id });
                    }
                    None => {
                        println!("Participant '{}' not found", name);
                    }
                }
            } else {
                println!("Usage: /remove <name>");
            }
        }
        Some(&"quit") => {
            println!("Goodbye!");
            std::process::exit(0);
        }
        Some(&"help" | &"?") => {
            println!("Available commands:");
            println!("  /name <new name>  - Change your name");
            println!("  /login <name>     - Login with a name (for testing)");
            println!("  /who              - List current participants");
            println!("  /remove <name>    - Remove a participant from the chat");
            println!("  /quit             - Exit the chat");
            println!("  /help or /?       - Show this help message");
        }
        Some(cmd) => {
            println!("Unknown command: /{}", cmd);
            println!("Type /help or /? for available commands");
        }
        None => {
            println!("Empty command");
            println!("Type /help or /? for available commands");
        }
    }
}

// ============================================================================
// Participant
// ============================================================================

struct Participant {
    name: String,
}

impl Participant {
    fn new(name: String) -> Self {
        Self { name }
    }
}

// ============================================================================
// Client
// ============================================================================

struct Client {
    participants: Mutex<HashMap<usize, Participant>>,
}

impl Client {
    fn new() -> Self {
        Self {
            participants: Mutex::new(HashMap::new()),
        }
    }
}

// ============================================================================
// Client - Participant Management
// ============================================================================

impl Client {
    fn add_participant(&self, id: usize, participant: Participant) {
        let mut participants = self.participants.lock().unwrap();
        participants.insert(id, participant);
    }

    fn remove_participant(&self, id: usize) {
        let mut participants = self.participants.lock().unwrap();
        participants.remove(&id);
    }

    fn get_participant_name(&self, id: usize) -> Option<String> {
        let participants = self.participants.lock().unwrap();
        participants.get(&id).map(|p| p.name.clone())
    }

    fn modify_participant_name(&self, id: usize, new_name: String) {
        let mut participants = self.participants.lock().unwrap();
        if let Some(participant) = participants.get_mut(&id) {
            participant.name = new_name;
        }
    }

    fn get_all_participant_names(&self) -> Vec<String> {
        let participants = self.participants.lock().unwrap();
        participants.values().map(|p| p.name.clone()).collect()
    }

    fn get_participant_id_by_name(&self, name: &str) -> Option<usize> {
        let participants = self.participants.lock().unwrap();
        participants
            .iter()
            .find(|(_, p)| p.name == name)
            .map(|(id, _)| *id)
    }
}

// ============================================================================
// Client - Event Handling
// ============================================================================

impl Client {
    fn dispatch_event(
        &self,
        event: MessengerEvent,
        messenger_intf: &MessengerInterface,
        server_id: usize,
    ) -> Option<ExitCode> {
        match event {
            MessengerEvent::Inactive => {
                panic!("Unexpected Inactive event");
            }
            MessengerEvent::Connected { .. } => {
                println!("Connection to server established");
                None
            }
            MessengerEvent::ConnectionFailed { .. } => {
                eprintln!("Connection to server could not be established");
                Some(ExitCode::FAILURE)
            }
            MessengerEvent::Disconnected { .. } => {
                eprintln!("Lost connection to server");
                Some(ExitCode::FAILURE)
            }
            MessengerEvent::Message { id, msg, .. } => {
                assert_eq!(id, server_id);
                self.handle_message(id, msg, messenger_intf);
                None
            }
        }
    }

    fn handle_message(
        &self,
        _id: usize,
        msg: Box<dyn Message>,
        _messenger_intf: &MessengerInterface,
    ) {
        if let Some(msg) = msg.downcast_ref::<SInit>() {
            let my_id = msg.id;
            for (id, name) in &*msg.logins {
                if id != &my_id {
                    println!("{name} is here.");
                }
                self.add_participant(*id, Participant::new(name.clone()));
            }
            println!("[Connected] You are now logged in.");
        } else if let Some(msg) = msg.downcast_ref::<SError>() {
            eprintln!("[Server Error] {}", msg.message);
        } else if let Some(msg) = msg.downcast_ref::<SLogin>() {
            println!("{} joined.", msg.name);
            self.add_participant(msg.id, Participant::new(msg.name.clone()));
        } else if let Some(msg) = msg.downcast_ref::<SLogoff>() {
            if let Some(name) = self.get_participant_name(msg.id) {
                println!("{} left.", name);
            }
            self.remove_participant(msg.id);
        } else if let Some(msg) = msg.downcast_ref::<SRemove>() {
            if let Some(name) = self.get_participant_name(msg.id) {
                println!("{} was removed.", name);
            }
            self.remove_participant(msg.id);
        } else if let Some(msg) = msg.downcast_ref::<SSay>() {
            if let Some(name) = self.get_participant_name(msg.id) {
                println!("{}: {}", name, msg.text);
            }
        } else if let Some(msg) = msg.downcast_ref::<SName>() {
            if let Some(old_name) = self.get_participant_name(msg.id) {
                println!("{} is now known as \"{}\".", old_name, msg.name);
                self.modify_participant_name(msg.id, msg.name.clone());
            }
        } else {
            eprintln!(
                "[Warning] Received unknown message type: {}",
                msg.message_id()
            );
        }
    }
}
