#![no_std]
#![forbid(unsafe_code)]
//! Caller-buffered MCP client/server primitives and a zcall tool bridge.
//! The core performs no I/O or allocation. Stdio process and HTTP/1.1 stream
//! adapters are opt-in through `std`; TLS and application execution remain external.

#[cfg(feature = "std")]
extern crate std;

/// Re-export the exact model-call contract used by the bridge.
pub use zcall;

#[cfg(feature = "std")]
pub mod stdio;

#[cfg(feature = "std")]
pub mod http_io;

pub mod batch;
pub mod bridge;
pub mod client;
pub mod extensions;
pub mod http;
pub mod json;
pub mod legacy_sse;
pub mod mcp;
pub mod notifications;
pub mod rpc;
pub mod sse;
pub mod tools;
pub mod tracker;
pub mod transport;
mod writer;

/// A bounded failure. Offsets are zero-based byte positions in the original input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: usize,
    /// Required capacity when known; zero for non-capacity errors.
    pub needed: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    Json,
    Utf8,
    Depth,
    InputLimit,
    Members,
    StringLimit,
    Tokens,
    Buffer,
    Type,
    Integer,
    DuplicateKey,
    InvalidRpc,
    InvalidParams,
    UnsupportedVersion,
    State,
    DuplicateId,
    UnknownId,
    RequestSlots,
    UnexpectedResponse,
    UnsupportedResult,
}
impl Error {
    pub(crate) const fn new(kind: ErrorKind, offset: usize) -> Self {
        Self {
            kind,
            offset,
            needed: 0,
        }
    }
    pub(crate) const fn capacity(kind: ErrorKind, offset: usize, needed: usize) -> Self {
        Self {
            kind,
            offset,
            needed,
        }
    }
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:?} at byte {} (needed {})",
            self.kind, self.offset, self.needed
        )
    }
}
impl core::error::Error for Error {}
