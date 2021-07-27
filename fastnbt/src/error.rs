//! Contains the Error and Result type used by the deserializer.
use std::fmt::Display;

/// Various errors that can occur during deserialization.
#[derive(Debug, Clone)]
pub struct Error(String);

/// Convenience type for Result.
pub type Result<T> = std::result::Result<T, Error>;

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl serde::de::Error for Error {
    fn custom<T: Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(format!("io error: {}", e))
    }
}

impl Error {
    pub(crate) fn invalid_tag(tag: u8) -> Error {
        Error(format!("invalid nbt tag value: {}", tag))
    }

    pub(crate) fn invalid_size(size: i32) -> Error {
        Error(format!("invalid nbt list/array size: {}", size))
    }

    pub(crate) fn no_root_compound() -> Error {
        Error("invalid nbt: no root compound".to_string())
    }

    pub(crate) fn nonunicode_string(data: &[u8]) -> Error {
        Error(format!(
            "invalid nbt string: nonunicode: {}",
            String::from_utf8_lossy(data)
        ))
    }

    pub(crate) fn unexpected_eof() -> Error {
        Error("eof: unexpectedly ran out of input".to_string())
    }

    pub(crate) fn bespoke(msg: String) -> Error {
        Error(msg)
    }
}
