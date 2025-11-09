use thiserror::Error;

/// The error type for rustcomm operations.
///
/// This encompasses all errors that can occur when using the rustcomm library,
/// including network operations, message serialization/deserialization, and TLS.
///
/// Most errors are unrecoverable and should be handled by logging and potentially
/// shutting down. Connection-specific errors (like connection failures or
/// disconnections) are typically handled through events rather than errors.
#[derive(Error, Debug)]
pub enum Error {
    // ============================================================================
    // I/O and Networking Errors
    // ============================================================================
    
    /// Low-level I/O error from the operating system.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The provided socket address could not be parsed or resolved.
    #[error("Invalid socket address")]
    InvalidAddress,

    /// Attempted to operate on a connection ID that doesn't exist.
    #[error("Connection {id} not found")]
    ConnectionNotFound {
        /// The connection ID that was not found.
        id: usize,
    },

    /// Attempted to operate on a listener ID that doesn't exist.
    #[error("Listener {id} not found")]
    ListenerNotFound {
        /// The listener ID that was not found.
        id: usize,
    },

    /// Internal polling mechanism encountered an error.
    #[error("Poll error: {0}")]
    PollError(String),

    // ============================================================================
    // Message Serialization Errors
    // ============================================================================
    
    /// Message header doesn't start with the expected magic bytes.
    ///
    /// This typically indicates corrupted data or attempting to deserialize
    /// non-message data.
    #[error("Invalid magic bytes in message header")]
    InvalidMagicBytes,

    /// Message protocol version is incompatible with this library version.
    #[error("Protocol version mismatch: expected {expected_major}.{expected_minor}, but message uses {received_major}.{received_minor}")]
    VersionMismatch {
        expected_major: u8,
        expected_minor: u8,
        received_major: u8,
        received_minor: u8,
    },

    /// Message data is corrupted or doesn't match the expected format.
    #[error("Malformed message data: {0}")]
    MalformedData(String),

    /// Received a message with an ID that hasn't been registered.
    ///
    /// Make sure all message types are registered in the [`MessageRegistry`](crate::MessageRegistry)
    /// before deserializing.
    #[error("Unknown message ID: {0}")]
    UnknownMessageId(String),

    // ============================================================================
    // TLS Errors
    // ============================================================================
    
    /// Failed to load TLS certificate file from disk.
    #[error("Failed to load certificate from {path}: {source}")]
    TlsCertificateLoad {
        path: String,
        source: std::io::Error,
    },

    /// Failed to load TLS private key file from disk.
    #[error("Failed to load private key from {path}: {source}")]
    TlsKeyLoad {
        path: String,
        source: std::io::Error,
    },

    /// Certificate file format is invalid or unsupported.
    #[error("Invalid certificate format: {0}")]
    TlsInvalidCertificate(String),

    /// Private key file format is invalid or unsupported.
    #[error("Invalid private key format: {0}")]
    TlsInvalidKey(String),

    /// Server name for TLS SNI is invalid.
    #[error("Invalid server name '{0}'")]
    TlsInvalidServerName(String),

    /// TLS handshake failed during connection establishment.
    #[error("TLS handshake failed: {0}")]
    TlsHandshake(String),

    /// Attempted to listen with TLS but server configuration is missing.
    ///
    /// When using TLS transport, you must provide `tls_server_cert` and
    /// `tls_server_key` configuration keys before calling `listen()`.
    #[error("TLS server configuration not provided - required for listen()")]
    TlsServerConfigMissing,

    /// Attempted to connect with TLS but client configuration is missing.
    #[error("TLS client configuration not provided - required for connect()")]
    TlsClientConfigMissing,

    /// Failed to build TLS server configuration from provided settings.
    #[error("Failed to build TLS server config: {0}")]
    TlsServerConfigBuild(String),

    /// Failed to build TLS client configuration from provided settings.
    #[error("Failed to build TLS client config: {0}")]
    TlsClientConfigBuild(String),

    // ============================================================================
    // Configuration Errors
    // ============================================================================
    
    /// Configuration file parsing or key lookup failed.
    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    /// Invalid value for `transport_type` configuration key.
    ///
    /// Must be one of: "tcp" or "tls" (when TLS feature is enabled).
    #[error("Invalid transport type '{got}', expected one of: {}", .valid.join(", "))]
    InvalidTransportType { got: String, valid: Vec<String> },
}

// ============================================================================
// Request-Response Errors
// ============================================================================

/// Errors that can occur during request-response operations.
///
/// These errors are specific to the async request-response layer provided by
/// the `req-resp` feature. They represent failure modes when awaiting responses
/// to sent requests.
#[derive(Error, Debug)]
pub enum RequestError {
    /// The connection was closed before receiving a response.
    ///
    /// This happens when the peer disconnects or the connection is lost while
    /// a request is pending.
    #[error("Connection closed")]
    ConnectionClosed,
    
    /// Received a response of the wrong type.
    ///
    /// This occurs when the response message type doesn't match the expected
    /// type parameter provided to `send_request()`.
    #[error("Expected response type {expected}, but received a different type")]
    WrongResponseType {
        /// The expected response type name.
        expected: &'static str
    },
    
    /// The event loop was dropped unexpectedly.
    ///
    /// This should not happen during normal operation and indicates an internal error.
    #[error("Event loop terminated unexpectedly")]
    EventLoopTerminated,
}
