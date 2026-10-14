use std::fmt;
use std::io;

/// An error that stops the pod runtime.
#[derive(Debug)]
pub enum Error {
    /// Reading from or writing to the client failed.
    Io(io::Error),
    /// The client sent something the runtime cannot make sense of.
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "pod i/o error: {e}"),
            Error::Protocol(msg) => write!(f, "pod protocol error: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Protocol(_) => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// An error returned by a pod function.
///
/// It is sent to the client as an error reply, which babashka raises as an
/// `ex-info` with this message and data. Any `std::error::Error` converts into
/// it, so `?` works inside handlers.
#[derive(Debug, Clone, PartialEq)]
pub struct PodError {
    message: String,
    data: Option<serde_json::Value>,
}

impl PodError {
    /// Creates an error with a message and no data.
    pub fn new(message: impl Into<String>) -> Self {
        PodError {
            message: message.into(),
            data: None,
        }
    }

    /// Attaches data, delivered to the client as `ex-data`.
    ///
    /// The data should be a JSON object so that it reads back as a map.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    /// The error message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The error data, if any.
    pub fn data(&self) -> Option<&serde_json::Value> {
        self.data.as_ref()
    }
}

impl fmt::Display for PodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl<E: std::error::Error> From<E> for PodError {
    fn from(e: E) -> Self {
        PodError::new(e.to_string())
    }
}
