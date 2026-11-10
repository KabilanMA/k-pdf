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
    /// The document is encrypted in a way that can't be read (not with a
    /// password, or with an unknown method); the message says how, as a
    /// sentence fragment.
    Encrypted(String),
    /// The document is protected by a password, and none (or an empty one)
    /// was given.
    PasswordNeeded,
    /// The password given doesn't open the document.
    WrongPassword,
    /// A photo could not be read (see `scan`); the message is a sentence
    /// fragment such as "not a JPEG or PNG image".
    Image(String),
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
            Error::Encrypted(msg) => write!(f, "can't read it: {msg}"),
            Error::PasswordNeeded => f.write_str("it is protected by a password"),
            Error::WrongPassword => f.write_str("the password is not correct"),
            Error::Image(msg) => f.write_str(msg),
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
