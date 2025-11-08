use thiserror::Error;

/// The error type for rustcomm operations.
///
/// This encompasses all errors that can occur when using the rustcomm library,
/// including network operations, message serialization/deserialization, and
/// TLS.
#[derive(Error, Debug)]
pub enum Error {
    // ============================================================================
    // I/O and Networking Errors
    // ============================================================================
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    // Networking errors
    #[error("Invalid socket address")]
    InvalidAddress,

    #[error("Connection {id} not found")]
    ConnectionNotFound { id: usize },

    #[error("Listener {id} not found")]
    ListenerNotFound { id: usize },

    #[error("Poll error: {0}")]
    PollError(String),

    // ============================================================================
    // Message Serialization Errors
    // ============================================================================
    #[error("Invalid magic bytes in message header")]
    InvalidMagicBytes,

    #[error("Protocol version mismatch: expected {expected_major}.{expected_minor}, but message uses {received_major}.{received_minor}")]
    VersionMismatch {
        expected_major: u8,
        expected_minor: u8,
        received_major: u8,
        received_minor: u8,
    },

    #[error("Malformed message data: {0}")]
    MalformedData(String),

    #[error("Unknown message ID: {0}")]
    UnknownMessageId(String),

    // ============================================================================
    // TLS Errors
    // ============================================================================
    #[error("Failed to load certificate from {path}: {source}")]
    TlsCertificateLoad {
        path: String,
        source: std::io::Error,
    },

    #[error("Failed to load private key from {path}: {source}")]
    TlsKeyLoad {
        path: String,
        source: std::io::Error,
    },

    #[error("Invalid certificate format: {0}")]
    TlsInvalidCertificate(String),

    #[error("Invalid private key format: {0}")]
    TlsInvalidKey(String),

    #[error("Invalid server name '{0}'")]
    TlsInvalidServerName(String),

    #[error("TLS handshake failed: {0}")]
    TlsHandshake(String),

    #[error("TLS server configuration not provided - required for listen()")]
    TlsServerConfigMissing,

    #[error("TLS client configuration not provided - required for connect()")]
    TlsClientConfigMissing,

    #[error("Failed to build TLS server config: {0}")]
    TlsServerConfigBuild(String),

    #[error("Failed to build TLS client config: {0}")]
    TlsClientConfigBuild(String),

    // ============================================================================
    // Configuration Errors
    // ============================================================================
    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("Invalid transport type '{got}', expected one of: {}", .valid.join(", "))]
    InvalidTransportType { got: String, valid: Vec<String> },
}
