use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Packet too large: {0} bytes (max 16MB)")]
    PacketTooLarge(usize),

    #[error("Out of sequence: expected {expected}, got {got}")]
    OutOfSequence { expected: u8, got: u8 },

    #[error("Invalid packet format: {0}")]
    InvalidFormat(String),

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Backend connection error: {0}")]
    #[allow(dead_code)]
    Backend(String),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;
