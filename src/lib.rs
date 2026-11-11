#![no_std]
#![forbid(unsafe_code)]
//! Caller-buffered MCP primitives. No I/O, allocation, executor, or model dependency.

pub mod json;
pub mod rpc;
pub mod transport;
pub mod mcp;
pub mod tools;
pub mod tracker;
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
    Json, Utf8, Depth, InputLimit, Members, StringLimit, Tokens, Buffer, Type,
    Integer, DuplicateKey, InvalidRpc, InvalidParams, UnsupportedVersion,
    State, DuplicateId, UnknownId,
}
impl Error {
    pub(crate) const fn new(kind: ErrorKind, offset: usize) -> Self { Self { kind, offset, needed: 0 } }
    pub(crate) const fn capacity(kind: ErrorKind, offset: usize, needed: usize) -> Self { Self { kind, offset, needed } }
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?} at byte {} (needed {})", self.kind, self.offset, self.needed)
    }
}
impl core::error::Error for Error {}
