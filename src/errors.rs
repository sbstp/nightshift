use rusqlite::types::FromSqlError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotEmpty,
    NotFound,
    InvalidArgument,
    Overflow,
    Other(String),
    InvalidCompression,
}

impl Error {
    pub fn errno(self) -> libc::c_int {
        match self {
            Error::NotEmpty => libc::ENOTEMPTY,
            Error::NotFound => libc::ENOENT,
            Error::InvalidArgument => libc::EINVAL,
            Error::Overflow => libc::EOVERFLOW,
            Error::InvalidCompression => libc::EINVAL,
            Error::Other(_) => libc::ENOTSUP, // Need better code
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        match err {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound,
            _ => Error::Other(err.to_string()),
        }
    }
}

impl From<FromSqlError> for Error {
    fn from(err: FromSqlError) -> Self {
        Error::Other(err.to_string())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotEmpty => write!(f, "Not Empty"),
            Error::NotFound => write!(f, "Not Found"),
            Error::InvalidArgument => write!(f, "Invalid Argument"),
            Error::Overflow => write!(f, "Overflow"),
            Error::InvalidCompression => write!(f, "Invalid Compression Scheme"),
            Error::Other(msg) => write!(f, "Other: {}", msg),
        }
    }
}

impl std::error::Error for Error {}
