use std::fmt;

/// Errors produced while reading a PDF.
#[derive(Debug)]
pub enum Error {
    /// Underlying I/O failure (opening / mapping the file).
    Io(std::io::Error),
    /// The data is not a PDF, or is damaged beyond recovery.
    Parse { offset: usize, msg: String },
    /// A referenced object does not exist or has the wrong type.
    Object(String),
    /// A stream filter failed or is not supported.
    Filter(String),
    /// The document is encrypted and decryption is not implemented yet.
    Encrypted,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn parse(offset: usize, msg: impl Into<String>) -> Self {
        Error::Parse { offset, msg: msg.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Parse { offset, msg } => write!(f, "parse error at byte {offset}: {msg}"),
            Error::Object(msg) => write!(f, "object error: {msg}"),
            Error::Filter(msg) => write!(f, "filter error: {msg}"),
            Error::Encrypted => write!(f, "document is encrypted (decryption not supported yet)"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
