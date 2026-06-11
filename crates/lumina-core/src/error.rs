//! Error types for lumina-core

use std::fmt;

#[derive(Debug)]
pub enum LuminaError {
    Config(String),
    Network(reqwest::Error),
    Parse(serde_json::Error),
    Chord(String),
    Io(std::io::Error),
    SecurityViolation(String),
    /// Internal runtime error (e.g. WASM execution failure that is not a security policy violation)
    Internal(String),
}

impl fmt::Display for LuminaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LuminaError::Config(msg) => write!(f, "Configuration error: {}", msg),
            LuminaError::Network(err) => write!(f, "Network error: {}", err),
            LuminaError::Parse(err) => write!(f, "Parse error: {}", err),
            LuminaError::Chord(msg) => write!(f, "Chord API error: {}", msg),
            LuminaError::Io(err) => write!(f, "IO error: {}", err),
            LuminaError::SecurityViolation(msg) => write!(f, "Security violation: {}", msg),
            LuminaError::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for LuminaError {}

impl From<reqwest::Error> for LuminaError {
    fn from(err: reqwest::Error) -> Self {
        LuminaError::Network(err)
    }
}

impl From<serde_json::Error> for LuminaError {
    fn from(err: serde_json::Error) -> Self {
        LuminaError::Parse(err)
    }
}

impl From<std::io::Error> for LuminaError {
    fn from(err: std::io::Error) -> Self {
        LuminaError::Io(err)
    }
}

pub type Result<T> = std::result::Result<T, LuminaError>;