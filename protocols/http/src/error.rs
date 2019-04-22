use std::{error, fmt, io, result};

#[derive(Debug)]
pub enum Error {
    IoError(io::Error),
    ParseError(httparse::Error),
    VersionError(u8),
    ContentLengthError,
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Error")      // TODO:
    }
}

pub type Result<T> = result::Result<T, Error>;

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Error {
        Error::IoError(error)
    }
}

impl From<httparse::Error> for Error {
    fn from(error: httparse::Error) -> Error {
        Error::ParseError(error)
    }
}
