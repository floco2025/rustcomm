use bincode::{Decode, Encode};
use rustcomm::impl_message;

// ============================================================================
// Client Messages
// ============================================================================

/// Client to Server: Login request.
#[derive(Encode, Decode, Debug)]
pub struct CLogin {
    pub name: String, // The login name
}
impl_message!(CLogin);

/// Client to Server: Name change request.
#[derive(Encode, Decode, Debug)]
pub struct CName {
    pub name: String, // The new name
}
impl_message!(CName);

/// Client to Server: Chat message.
#[derive(Encode, Decode, Debug)]
pub struct CSay {
    pub text: String, // The chat message
}
impl_message!(CSay);

/// Client to Server: Remove a participant.
#[derive(Encode, Decode, Debug)]
pub struct CRemove {
    pub id: usize, // The id of the participant to remove
}
impl_message!(CRemove);

// ============================================================================
// Server Messages
// ============================================================================

/// Server to Client: Error message.
#[derive(Encode, Decode, Debug)]
pub struct SError {
    pub message: String, // The error message to display
}
impl_message!(SError);

/// Server to Client: Initial server state upon connection.
#[derive(Encode, Decode, Debug)]
pub struct SInit {
    pub id: usize,                    // The id that the server uses for the client
    pub logins: Vec<(usize, String)>, // All other participant ids and their names
}
impl_message!(SInit);

/// Server to Client: Another participant connected.
#[derive(Encode, Decode, Debug)]
pub struct SLogin {
    pub id: usize,    // The id for the new participant
    pub name: String, // The name of the new participant
}
impl_message!(SLogin);

/// Server to Client: A participant disconnected.
#[derive(Encode, Decode, Debug)]
pub struct SLogoff {
    pub id: usize, // The id of the participant who disconnected
}
impl_message!(SLogoff);

/// Server to Client: Chat message from a participant.
#[derive(Encode, Decode, Debug)]
pub struct SSay {
    pub id: usize,    // The id of the participant who sends the chat message
    pub text: String, // The chat message
}
impl_message!(SSay);

/// Server to Client: Participant name change.
#[derive(Encode, Decode, Debug)]
pub struct SName {
    pub id: usize,    // The id of the participant who changed their name
    pub name: String, // The new name
}
impl_message!(SName);

/// Server to Client: A participant was removed.
#[derive(Encode, Decode, Debug)]
pub struct SRemove {
    pub id: usize, // The id of the participant who was removed
}
impl_message!(SRemove);
