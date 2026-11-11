//! Bounded JSON-RPC batches for MCP 2025-03-26 only.
//!
//! That revision requires receiving batches; 2024-11-05 predates them and
//! 2025-06-18 removes them. Initialize is never allowed inside a batch.
//! Parse the whole batch before dispatch, then handle entries explicitly.
//! This module performs no execution, lifecycle transitions, retries, or I/O.
use crate::{
    Error, ErrorKind,
    json::{Kind, Value},
    mcp::VERSION_KEY,
    rpc::{self, Message},
    writer::Writer,
};

pub const LEGACY_BATCH_VERSION: &str = "2025-03-26";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchKind {
    /// Contains requests or malformed request entries requiring a response.
    Requests,
    /// Every entry is a valid notification; send no JSON-RPC response.
    Notifications,
    /// Response batch. Malformed entries are local errors, never answered.
    Responses,
}

#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    /// Original item bytes may be passed to Server::handle or Client::receive.
    pub raw: Value<'a>,
    /// An invalid item does not discard its valid siblings.
    pub message: Result<Message<'a>, Error>,
}

/// A validated nonempty batch envelope borrowing caller-owned input/token storage.
/// It does not retain or change client/server request state.
#[derive(Clone, Copy, Debug)]
pub struct Batch<'a> {
    raw: Value<'a>,
    len: usize,
    kind: BatchKind,
}
impl<'a> Batch<'a> {
    pub fn parse(raw: Value<'a>, version: &str, max_items: usize) -> Result<Self, Error> {
        check_version(version)?;
        if raw.kind() != Kind::Array { return Err(invalid(raw)); }
        let mut len = 0;
        let mut requests = false;
        let mut notifications = false;
        let mut responses = false;
        let mut invalid_items = false;
        for value in raw.elements() {
            len += 1;
            if len > max_items {
                return Err(Error::capacity(ErrorKind::Members, value.offset(), len));
            }
            check_profile(value)?;
            match rpc::parse(value) {
                Ok(Message::Request(_)) => requests = true,
                Ok(Message::Notification(_)) => notifications = true,
                Ok(Message::Success { .. } | Message::Failure { .. }) => responses = true,
                Err(_) => invalid_items = true,
            }
        }
        if len == 0 || (responses && (requests || notifications)) {
            return Err(invalid(raw));
        }
        let kind = if responses {
            BatchKind::Responses
        } else if requests || invalid_items {
            BatchKind::Requests
        } else {
            BatchKind::Notifications
        };
        Ok(Self { raw, len, kind })
    }

    pub const fn version(self) -> &'static str { LEGACY_BATCH_VERSION }
    pub const fn len(self) -> usize { self.len }
    pub const fn is_empty(self) -> bool { false }
    pub const fn kind(self) -> BatchKind { self.kind }
    pub fn raw(self) -> Value<'a> { self.raw }

    /// Incoming request-batch invalid entries need an id:null/-32600 response.
    /// Do not answer response entries, including malformed response-batch items.
    /// ID uniqueness, lifecycle ordering, authorization and execution are host-owned.
    pub fn entries(self) -> impl Iterator<Item = Entry<'a>> {
        self.raw.elements().map(|raw| Entry { raw, message: rpc::parse(raw) })
    }
}

/// Encode a nonempty batch of previously encoded/validated requests or notifications.
/// It does not reserve IDs: callers may compose Client-generated requests here.
/// Output remains unchanged on any error; do not resend calls after send ambiguity.
pub fn encode_requests(
    version: &str,
    messages: &[Value<'_>],
    max_items: usize,
    output: &mut [u8],
) -> Result<usize, Error> {
    check_version(version)?;
    check_count(messages.len(), max_items)?;
    if messages.is_empty() { return Err(Error::new(ErrorKind::InvalidRpc, 0)); }
    let mut needed = 1;
    for value in messages {
        check_profile(*value)?;
        if !matches!(rpc::parse(*value)?, Message::Request(_) | Message::Notification(_)) {
            return Err(invalid(*value));
        }
        needed = append_size(needed, value.raw().len())?;
    }
    write_array(messages.iter().copied(), needed, output)
}

/// Encode only responses. Use None for notifications or other entries with no reply.
/// All-None (or empty) input returns zero bytes and never emits an empty JSON array.
/// The entry bound includes None slots; every response is validated before writing.
/// Retain completed results when output is too small; never rerun their side effects.
pub fn encode_responses(
    version: &str,
    responses: &[Option<Value<'_>>],
    max_items: usize,
    output: &mut [u8],
) -> Result<usize, Error> {
    check_version(version)?;
    check_count(responses.len(), max_items)?;
    let mut needed = 1;
    let mut count = 0;
    for value in responses.iter().flatten() {
        if !matches!(rpc::parse(*value)?, Message::Success { .. } | Message::Failure { .. }) {
            return Err(invalid(*value));
        }
        needed = append_size(needed, value.raw().len())?;
        count += 1;
    }
    if count == 0 { return Ok(0); }
    write_array(responses.iter().flatten().copied(), needed, output)
}

fn check_version(version: &str) -> Result<(), Error> {
    if version == LEGACY_BATCH_VERSION { Ok(()) }
    else { Err(Error::new(ErrorKind::UnsupportedVersion, 0)) }
}
fn check_count(count: usize, max: usize) -> Result<(), Error> {
    if count <= max { Ok(()) }
    else { Err(Error::capacity(ErrorKind::Members, 0, count)) }
}
fn check_profile(value: Value<'_>) -> Result<(), Error> {
    // Check before dispatch even if the surrounding JSON-RPC envelope is invalid.
    if value.get("method").and_then(Value::string).is_some_and(|s| s.matches("initialize")) {
        return Err(invalid(value));
    }
    // Server::handle uses this metadata to select modern semantics. A legacy
    // batch must not smuggle a modern per-request envelope through that boundary.
    if value.get("params").and_then(|v| v.get("_meta"))
        .and_then(|v| v.get(VERSION_KEY)).is_some()
    {
        return Err(Error::new(ErrorKind::UnsupportedVersion, value.offset()));
    }
    Ok(())
}
fn invalid(value: Value<'_>) -> Error { Error::new(ErrorKind::InvalidRpc, value.offset()) }

// '[' + N values + (N-1) commas + ']' = 1 + sum(value.len() + 1).
fn append_size(size: usize, item: usize) -> Result<usize, Error> {
    size.checked_add(item).and_then(|v| v.checked_add(1))
        .ok_or(Error::capacity(ErrorKind::Buffer, 0, usize::MAX))
}
fn write_array<'a>(
    values: impl Iterator<Item = Value<'a>>,
    needed: usize,
    output: &mut [u8],
) -> Result<usize, Error> {
    if output.len() < needed {
        return Err(Error::capacity(ErrorKind::Buffer, 0, needed));
    }
    let mut w = Writer::new(output);
    w.raw(b"[");
    let mut comma = false;
    for value in values {
        if comma { w.raw(b","); }
        w.raw(value.raw().as_bytes());
        comma = true;
    }
    w.raw(b"]");
    w.finish()
}
