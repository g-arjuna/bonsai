use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("BMP parse error: {0}")]
    BmpParse(String),

    #[error("BGP parse error: {0}")]
    BgpParse(String),

    #[error("Unexpected end of buffer (need {needed} bytes, have {have})")]
    UnexpectedEof { needed: usize, have: usize },

    #[error("Invalid BMP version: got {0}, expected 3")]
    InvalidBmpVersion(u8),

    #[error("Invalid BMP message type: {0}")]
    InvalidMessageType(u8),

    #[error("BMP frame too large: {0} bytes (max {1})")]
    FrameTooLarge(u32, u32),

    #[error("Invalid BGP marker (expected 16x 0xFF)")]
    InvalidBgpMarker,

    #[error("Invalid BGP message type: {0}")]
    InvalidBgpMessageType(u8),

    #[error("Invalid AFI: {0}")]
    InvalidAfi(u16),

    #[error("Invalid SAFI: {0}")]
    InvalidSafi(u8),

    #[error("Invalid prefix length {prefix_len} for AFI {afi}")]
    InvalidPrefixLen { prefix_len: u8, afi: u16 },

    #[error("Truncated path attribute (type {attr_type}, declared {declared}, available {available})")]
    TruncatedAttribute { attr_type: u8, declared: usize, available: usize },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
