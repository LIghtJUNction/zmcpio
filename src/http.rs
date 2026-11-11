//! Allocation-free, runtime-independent Streamable HTTP rules.
//!
//! HTTP/TLS implementations own sockets, HTTP framing, deadlines and credentials.
//! This module owns the MCP envelope and deliberately distinguishes 2026 POST-only
//! semantics from 2025 session/GET/resumption semantics. It never retries a tool.
//! Pass only complete JSON-RPC messages to the protocol client or server.
use crate::{
    json::{JsonString, Kind, Value},
    mcp::VERSION_KEY,
    rpc::{self, Id, Message, Request},
};
use core::fmt::Write as _;

pub const JSON: &str = "application/json";
pub const SSE: &str = "text/event-stream";
pub const ACCEPT: &str = "application/json, text/event-stream";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Version {
    V2025_03_26,
    V2025_06_18,
    V2025_11_25,
    V2026_07_28,
}
impl Version {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V2025_03_26 => "2025-03-26",
            Self::V2025_06_18 => "2025-06-18",
            Self::V2025_11_25 => "2025-11-25",
            Self::V2026_07_28 => "2026-07-28",
        }
    }
    pub const fn is_modern(self) -> bool {
        matches!(self, Self::V2026_07_28)
    }
    pub fn parse(value: &str) -> Result<Self, HttpError> {
        match value {
            "2025-03-26" => Ok(Self::V2025_03_26),
            "2025-06-18" => Ok(Self::V2025_06_18),
            "2025-11-25" => Ok(Self::V2025_11_25),
            "2026-07-28" => Ok(Self::V2026_07_28),
            _ => Err(HttpError::UnsupportedVersion),
        }
    }
    pub const fn cancellation(self) -> Cancellation {
        if self.is_modern() {
            Cancellation::CloseResponse
        } else {
            Cancellation::Notification
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cancellation {
    CloseResponse,
    Notification,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Post,
    Get,
    Delete,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyKind {
    Request,
    Notification,
    Response,
}
impl BodyKind {
    pub fn of(message: Message<'_>) -> Self {
        match message {
            Message::Request(_) => Self::Request,
            Message::Notification(_) => Self::Notification,
            _ => Self::Response,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseMode {
    Json,
    Sse,
    Empty,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpError {
    BadRequest,
    HeaderMismatch,
    UnsupportedVersion,
    Unauthorized,
    Forbidden,
    SessionExpired,
    MethodNotAllowed,
    NotAcceptable,
    UnsupportedMediaType,
    InvalidHeader,
    InvalidSchema,
    Capacity { needed: usize },
    UnexpectedStatus(u16),
}
impl HttpError {
    pub const fn status(self) -> u16 {
        match self {
            Self::BadRequest
            | Self::HeaderMismatch
            | Self::UnsupportedVersion
            | Self::InvalidHeader
            | Self::InvalidSchema => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::SessionExpired => 404,
            Self::MethodNotAllowed => 405,
            Self::NotAcceptable => 406,
            Self::UnsupportedMediaType => 415,
            Self::Capacity { .. } => 500,
            Self::UnexpectedStatus(status) => status,
        }
    }
    pub const fn rpc_code(self) -> Option<i32> {
        match self {
            Self::HeaderMismatch => Some(-32020),
            Self::UnsupportedVersion => Some(-32022),
            _ => None,
        }
    }
    /// Encode a transport error; preserve the originating request ID if known.
    /// Unsupported-version data includes the actual configured supported version.
    pub fn write_rpc(
        self,
        id: Option<Id<'_>>,
        supported: Version,
        requested: &str,
        output: &mut [u8],
    ) -> Result<usize, crate::Error> {
        if self == Self::UnsupportedVersion {
            let mut w = crate::writer::Writer::new(output);
            rpc::prefix(&mut w, id);
            w.raw(b",\"error\":{\"code\":-32022,\"message\":\"Unsupported protocol version\",\"data\":{\"supported\":[");
            w.text(supported.as_str());
            w.raw(b"],\"requested\":");
            w.text(requested);
            w.raw(b"}}}");
            w.finish()
        } else {
            rpc::failure(
                id,
                self.rpc_code().unwrap_or(-32600),
                if self == Self::HeaderMismatch {
                    "Header mismatch"
                } else {
                    "Invalid HTTP request"
                },
                None,
                output,
            )
        }
    }
}
impl core::fmt::Display for HttpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl core::error::Error for HttpError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header<'a> {
    pub name: &'a str,
    pub value: &'a str,
}
/// Singleton lookup is case-insensitive and rejects duplicates rather than letting
/// an intermediary and application select different values.
pub fn header<'a>(headers: &'a [Header<'a>], name: &str) -> Result<Option<&'a str>, HttpError> {
    let mut result = None;
    for h in headers {
        if !token(h.name) {
            return Err(HttpError::InvalidHeader);
        }
        if h.name.eq_ignore_ascii_case(name) {
            if result.is_some() || !field_value(h.value) {
                return Err(HttpError::InvalidHeader);
            }
            result = Some(h.value);
        }
    }
    Ok(result)
}
fn field_value(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
}
fn token(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(tchar)
}
fn tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_".contains(&b) || b == 0x60 || b == b'|' || b == b'~'
}
fn visible_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
}
fn media(value: &str, expected: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|s| s.trim().eq_ignore_ascii_case(expected))
}
fn accepts(headers: &[Header<'_>], expected: &str) -> bool {
    headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("accept"))
        .any(|h| {
            field_value(h.value)
                && h.value.split(',').any(|part| {
                    let mut pieces = part.trim().split(';');
                    if !pieces
                        .next()
                        .is_some_and(|v| v.trim().eq_ignore_ascii_case(expected))
                    {
                        return false;
                    }
                    // MCP requires listing the concrete media types, not merely */*.
                    pieces.all(|p| {
                        let p = p.trim();
                        if let Some((key, value)) = p.split_once('=')
                            && key.trim().eq_ignore_ascii_case("q")
                        {
                            let Ok(q) = value.trim().parse::<f32>() else {
                                return false;
                            };
                            q > 0.0 && q <= 1.0
                        } else {
                            true
                        }
                    })
                })
        })
}
/// Select decoding before reading a response body. Empty responses MUST also be
/// checked by the HTTP implementation for a zero-length body.
pub fn response_mode(
    status: u16,
    content_type: Option<&str>,
    kind: BodyKind,
) -> Result<ResponseMode, HttpError> {
    match (status, kind) {
        (202, BodyKind::Notification | BodyKind::Response) => Ok(ResponseMode::Empty),
        (200, BodyKind::Request) if content_type.is_some_and(|v| media(v, JSON)) => {
            Ok(ResponseMode::Json)
        }
        (200, BodyKind::Request) if content_type.is_some_and(|v| media(v, SSE)) => {
            Ok(ResponseMode::Sse)
        }
        (200, BodyKind::Request) => Err(HttpError::UnsupportedMediaType),
        (401, _) => Err(HttpError::Unauthorized),
        (403, _) => Err(HttpError::Forbidden),
        (405, _) => Err(HttpError::MethodNotAllowed),
        _ => Err(HttpError::UnexpectedStatus(status)),
    }
}

/// Runtime-owned authentication and session lookup. Neither a session ID nor an
/// Origin match grants authorization by itself. Bind sessions to the verified
/// identity in session_valid; reject stolen/mismatched identities there.
pub trait SecurityPolicy {
    fn origin_allowed(&self, origin: &str) -> bool;
    fn authorized(&self, headers: &[Header<'_>]) -> bool;
    fn session_valid(&self, session_id: &str, headers: &[Header<'_>]) -> bool;
}
#[derive(Clone, Copy, Debug)]
pub struct Incoming<'a> {
    pub method: Method,
    pub headers: &'a [Header<'a>],
    /// POST requires a parsed, complete JSON-RPC message. GET/DELETE require None.
    pub message: Option<Message<'a>>,
}
#[derive(Clone, Copy, Debug)]
pub struct ServerConfig {
    pub version: Version,
    pub allow_get: bool,
    pub allow_delete: bool,
    /// Legacy only. Session creation/storage/randomness belong to the host.
    pub sessions: bool,
}
impl ServerConfig {
    pub const fn stateless(version: Version) -> Self {
        Self {
            version,
            allow_get: false,
            allow_delete: false,
            sessions: false,
        }
    }
    /// Validate envelope before calling a tool or taking any other side effect.
    /// For tools with x-mcp-header annotations, also call validate_parameter_headers
    /// with the selected tool schema before executing it.
    pub fn validate<'a>(
        &self,
        incoming: Incoming<'a>,
        security: &impl SecurityPolicy,
        scratch: &mut [u8],
    ) -> Result<ServerAction<'a>, HttpError> {
        let headers = incoming.headers;
        if let Some(origin) = header(headers, "origin")?
            && !security.origin_allowed(origin)
        {
            return Err(HttpError::Forbidden);
        }
        // Disallow ambiguous credentials even if the host would select one.
        let _ = header(headers, "authorization")?;
        if !security.authorized(headers) {
            return Err(HttpError::Unauthorized);
        }
        if self.version.is_modern() && incoming.method != Method::Post {
            return Err(HttpError::MethodNotAllowed);
        }
        if incoming.method != Method::Post && incoming.message.is_some() {
            return Err(HttpError::BadRequest);
        }
        let initialize =
            matches!(incoming.message, Some(Message::Request(r)) if r.method.matches("initialize"));
        let wire_version = header(headers, "mcp-protocol-version").map_err(|e| {
            if self.version.is_modern() {
                HttpError::HeaderMismatch
            } else {
                e
            }
        })?;
        if self.version.is_modern() {
            if wire_version.is_none() {
                return Err(HttpError::HeaderMismatch);
            }
        } else if wire_version.is_none() && !initialize && self.version != Version::V2025_03_26 {
            // Missing headers can only select an explicitly configured 2025-03-26 binding.
            return Err(HttpError::UnsupportedVersion);
        }
        if let Some(version) = wire_version
            && version != self.version.as_str()
        {
            return Err(HttpError::UnsupportedVersion);
        }
        let session_id = if self.version.is_modern() {
            // Per 2026-07-28, ignore obsolete session/resumption metadata.
            None
        } else {
            let id = header(headers, "mcp-session-id")?;
            if id.is_some_and(|v| !visible_id(v)) {
                return Err(HttpError::InvalidHeader);
            }
            if self.sessions {
                if let Some(id) = id {
                    if !security.session_valid(id, headers) {
                        return Err(HttpError::SessionExpired);
                    }
                } else if !initialize {
                    return Err(HttpError::BadRequest);
                }
            }
            id
        };
        match incoming.method {
            Method::Post => {
                if !header(headers, "content-type")?.is_some_and(|v| media(v, JSON)) {
                    return Err(HttpError::UnsupportedMediaType);
                }
                if !accepts(headers, JSON) || !accepts(headers, SSE) {
                    return Err(HttpError::NotAcceptable);
                }
                let message = incoming.message.ok_or(HttpError::BadRequest)?;
                if self.version.is_modern() {
                    match message {
                        Message::Request(request) => {
                            validate_standard_headers(headers, request, scratch)?
                        }
                        Message::Notification(n) if n.method.matches("notifications/cancelled") => {
                            return Err(HttpError::BadRequest);
                        }
                        Message::Notification(_) => {}
                        _ => return Err(HttpError::BadRequest),
                    }
                }
                Ok(ServerAction::Message {
                    kind: BodyKind::of(message),
                })
            }
            Method::Get => {
                if !self.allow_get {
                    return Err(HttpError::MethodNotAllowed);
                }
                if !accepts(headers, SSE) {
                    return Err(HttpError::NotAcceptable);
                }
                Ok(ServerAction::Listen {
                    last_event_id: header(headers, "last-event-id")?,
                })
            }
            Method::Delete => {
                if !self.sessions || !self.allow_delete {
                    return Err(HttpError::MethodNotAllowed);
                }
                Ok(ServerAction::Terminate {
                    session_id: session_id.ok_or(HttpError::BadRequest)?,
                })
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerAction<'a> {
    Message { kind: BodyKind },
    Listen { last_event_id: Option<&'a str> },
    Terminate { session_id: &'a str },
}
/// The HTTP status for a JSON-RPC error (do not use for a successful result).
pub const fn rpc_error_status(version: Version, code: i32) -> u16 {
    if version.is_modern() {
        match code {
            -32601 => 404,
            -32020 | -32021 | -32022 | -32600 | -32700 => 400,
            _ => 200,
        }
    } else {
        200
    }
}
pub fn validate_standard_headers(
    headers: &[Header<'_>],
    request: Request<'_>,
    scratch: &mut [u8],
) -> Result<(), HttpError> {
    let version = header(headers, "mcp-protocol-version")
        .map_err(|_| HttpError::HeaderMismatch)?
        .ok_or(HttpError::HeaderMismatch)?;
    let method = header(headers, "mcp-method")
        .map_err(|_| HttpError::HeaderMismatch)?
        .ok_or(HttpError::HeaderMismatch)?;
    let params = request.params.ok_or(HttpError::HeaderMismatch)?;
    let body_version = params
        .get("_meta")
        .and_then(|v| v.get(VERSION_KEY))
        .and_then(Value::string)
        .ok_or(HttpError::HeaderMismatch)?;
    if !body_version.matches(version) || !request.method.matches(method) {
        return Err(HttpError::HeaderMismatch);
    }
    let name_key = if request.method.matches("resources/read") {
        Some("uri")
    } else if request.method.matches("tools/call") || request.method.matches("prompts/get") {
        Some("name")
    } else {
        None
    };
    if let Some(key) = name_key {
        let value = params
            .get(key)
            .and_then(Value::string)
            .ok_or(HttpError::HeaderMismatch)?;
        let wire = header(headers, "mcp-name")
            .map_err(|_| HttpError::HeaderMismatch)?
            .ok_or(HttpError::HeaderMismatch)?;
        let decoded = decode_header_value(wire, scratch).map_err(|e| {
            if matches!(e, HttpError::Capacity { .. }) {
                e
            } else {
                HttpError::HeaderMismatch
            }
        })?;
        if !value.matches(decoded) {
            return Err(HttpError::HeaderMismatch);
        }
    }
    Ok(())
}

/// Fixed storage for a legacy session ID. New-protocol clients never emit sessions.
/// Keep one instance per authenticated remote endpoint; do not share across origins.
pub struct ClientSession<'a> {
    version: Version,
    storage: &'a mut [u8],
    used: usize,
}
impl<'a> ClientSession<'a> {
    pub fn new(version: Version, storage: &'a mut [u8]) -> Self {
        Self {
            version,
            storage,
            used: 0,
        }
    }
    pub const fn version(&self) -> Version {
        self.version
    }
    pub fn session_id(&self) -> Option<&str> {
        if self.used == 0 {
            None
        } else {
            core::str::from_utf8(&self.storage[..self.used]).ok()
        }
    }
    /// Call only for a successfully validated legacy initialize result.
    /// A capacity/validation failure preserves the old session exactly.
    pub fn set_session_id(&mut self, id: Option<&str>) -> Result<(), HttpError> {
        if self.version.is_modern() {
            self.clear();
            return Ok(());
        }
        let Some(id) = id else {
            self.clear();
            return Ok(());
        };
        if !visible_id(id) {
            return Err(HttpError::InvalidHeader);
        }
        if id.len() > self.storage.len() {
            return Err(HttpError::Capacity { needed: id.len() });
        }
        if id.len() < self.used {
            self.storage[id.len()..self.used].fill(0);
        }
        self.storage[..id.len()].copy_from_slice(id.as_bytes());
        self.used = id.len();
        Ok(())
    }
    pub fn clear(&mut self) {
        self.storage[..self.used].fill(0);
        self.used = 0;
    }
    /// Return true when the exact session used for a request has expired.
    /// The host must initialize again; this never retries the original tool.
    pub fn observe_status(&mut self, status: u16, request_session: Option<&str>) -> bool {
        if !self.version.is_modern()
            && status == 404
            && request_session.is_some()
            && request_session == self.session_id()
        {
            self.clear();
            true
        } else {
            false
        }
    }
    /// Header block without the final empty line. Append validated custom headers,
    /// Content-Length and CRLF in the host. Never append LF to the JSON body.
    pub fn post_headers(
        &self,
        method: &str,
        name: Option<&str>,
        output: &mut [u8],
    ) -> Result<usize, HttpError> {
        if !field_value(method) || method.is_empty() {
            return Err(HttpError::InvalidHeader);
        }
        let needs_name = matches!(method, "tools/call" | "resources/read" | "prompts/get");
        if self.version.is_modern() && needs_name && name.is_none() {
            return Err(HttpError::HeaderMismatch);
        }
        let mut w = Bytes::new(output);
        w.header("Accept", ACCEPT);
        w.header("Content-Type", JSON);
        w.header("MCP-Protocol-Version", self.version.as_str());
        if self.version.is_modern() {
            w.header("Mcp-Method", method);
            if let Some(name) = name.filter(|_| needs_name) {
                w.raw(b"Mcp-Name: ");
                encode_value(name, &mut w);
                w.raw(b"\r\n");
            }
        } else if method != "initialize"
            && let Some(id) = self.session_id()
        {
            w.header("Mcp-Session-Id", id);
        }
        w.finish()
    }
    pub fn get_headers(
        &self,
        last_event_id: Option<&str>,
        output: &mut [u8],
    ) -> Result<usize, HttpError> {
        if self.version.is_modern() {
            return Err(HttpError::MethodNotAllowed);
        }
        if last_event_id.is_some_and(|s| !field_value(s)) {
            return Err(HttpError::InvalidHeader);
        }
        let mut w = Bytes::new(output);
        w.header("Accept", SSE);
        w.header("MCP-Protocol-Version", self.version.as_str());
        if let Some(id) = self.session_id() {
            w.header("Mcp-Session-Id", id);
        }
        if let Some(id) = last_event_id.filter(|id| !id.is_empty()) {
            w.header("Last-Event-ID", id);
        }
        w.finish()
    }
    pub fn delete_headers(&self, output: &mut [u8]) -> Result<usize, HttpError> {
        if self.version.is_modern() {
            return Err(HttpError::MethodNotAllowed);
        }
        let id = self.session_id().ok_or(HttpError::BadRequest)?;
        let mut w = Bytes::new(output);
        w.header("MCP-Protocol-Version", self.version.as_str());
        w.header("Mcp-Session-Id", id);
        w.finish()
    }
}

/// Encode the 2026 header sentinel, including sentinel-looking literal strings.
/// Capacity failures report the entire required length; discard partial output.
pub fn encode_header_value<'a>(value: &str, output: &'a mut [u8]) -> Result<&'a str, HttpError> {
    let mut w = Bytes::new(output);
    encode_value(value, &mut w);
    let used = w.finish()?;
    core::str::from_utf8(&output[..used]).map_err(|_| HttpError::InvalidHeader)
}
fn encode_value(value: &str, w: &mut Bytes<'_>) {
    let sentinel = value.starts_with("=?base64?") && value.ends_with("?=");
    if field_value(value) && value.trim_matches([' ', '\t']) == value && !sentinel {
        w.raw(value.as_bytes());
        return;
    }
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    w.raw(b"=?base64?");
    for chunk in value.as_bytes().chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        w.raw(&[
            TABLE[(a >> 2) as usize],
            TABLE[(((a & 3) << 4) | (b >> 4)) as usize],
            if chunk.len() > 1 {
                TABLE[(((b & 15) << 2) | (c >> 6)) as usize]
            } else {
                b'='
            },
            if chunk.len() > 2 {
                TABLE[(c & 63) as usize]
            } else {
                b'='
            },
        ]);
    }
    w.raw(b"?=");
}
/// Decode exactly one sentinel layer; reject non-canonical padding and non-UTF-8.
pub fn decode_header_value<'a>(value: &'a str, output: &'a mut [u8]) -> Result<&'a str, HttpError> {
    if !field_value(value) {
        return Err(HttpError::InvalidHeader);
    }
    let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|s| s.strip_suffix("?="))
    else {
        if value.trim_matches([' ', '\t']) != value {
            return Err(HttpError::InvalidHeader);
        }
        return Ok(value);
    };
    let input = encoded.as_bytes();
    if input.len() % 4 != 0 {
        return Err(HttpError::InvalidHeader);
    }
    let padding = usize::from(input.last() == Some(&b'='))
        + usize::from(input.len() >= 2 && input[input.len() - 2] == b'=');
    let needed = input.len() / 4 * 3 - padding;
    if output.len() < needed {
        return Err(HttpError::Capacity { needed });
    }
    let mut at = 0;
    for (index, chunk) in input.chunks_exact(4).enumerate() {
        let last = index + 1 == input.len() / 4;
        let a = base64(chunk[0])?;
        let b = base64(chunk[1])?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            base64(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            base64(chunk[3])?
        };
        if (chunk[2] == b'=' && (chunk[3] != b'=' || b & 15 != 0))
            || (chunk[3] == b'=' && chunk[2] != b'=' && c & 3 != 0)
            || (!last && chunk.contains(&b'='))
        {
            return Err(HttpError::InvalidHeader);
        }
        output[at] = (a << 2) | (b >> 4);
        at += 1;
        if chunk[2] != b'=' {
            output[at] = (b << 4) | (c >> 2);
            at += 1;
        }
        if chunk[3] != b'=' {
            output[at] = (c << 6) | d;
            at += 1;
        }
    }
    core::str::from_utf8(&output[..at]).map_err(|_| HttpError::InvalidHeader)
}
fn base64(b: u8) -> Result<u8, HttpError> {
    match b {
        b'A'..=b'Z' => Ok(b - b'A'),
        b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(HttpError::InvalidHeader),
    }
}
struct Bytes<'a> {
    output: &'a mut [u8],
    used: usize,
}
impl<'a> Bytes<'a> {
    fn new(output: &'a mut [u8]) -> Self {
        Self { output, used: 0 }
    }
    fn raw(&mut self, bytes: &[u8]) {
        let end = self.used.saturating_add(bytes.len());
        if end <= self.output.len() {
            self.output[self.used..end].copy_from_slice(bytes);
        }
        self.used = end;
    }
    fn header(&mut self, name: &str, value: &str) {
        self.raw(name.as_bytes());
        self.raw(b": ");
        self.raw(value.as_bytes());
        self.raw(b"\r\n");
    }
    fn finish(self) -> Result<usize, HttpError> {
        if self.used > self.output.len() {
            Err(HttpError::Capacity { needed: self.used })
        } else {
            Ok(self.used)
        }
    }
}
impl core::fmt::Write for Bytes<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.raw(s.as_bytes());
        Ok(())
    }
}

/// One schema-approved mirrored parameter. Entries borrow schema and arguments.
#[derive(Clone, Copy, Debug)]
pub struct Parameter<'a> {
    name: Option<JsonString<'a>>,
    value: Option<Value<'a>>,
    kind: Primitive,
}
impl Parameter<'_> {
    pub const EMPTY: Self = Self {
        name: None,
        value: None,
        kind: Primitive::String,
    };
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Primitive {
    String,
    Integer,
    Boolean,
}
/// Validate x-mcp-header annotations before advertising an HTTP tool. Exclude a
/// tool whose schema fails this check; do not silently omit its required headers.
/// Only inline properties paths are traversed for extraction; annotations under
/// array/composition/reference-definition schema locations are rejected.
/// The returned entries borrow both inputs. Discard storage contents on failure.
pub fn parameters<'a, 's>(
    schema: Value<'a>,
    arguments: Option<Value<'a>>,
    storage: &'s mut [Parameter<'a>],
) -> Result<&'s [Parameter<'a>], HttpError> {
    if schema.kind() != Kind::Object || arguments.is_some_and(|v| v.kind() != Kind::Object) {
        return Err(HttpError::InvalidSchema);
    }
    storage.fill(Parameter::EMPTY);
    let used = {
        let mut collector = Collector { storage, used: 0 };
        collector.walk(schema, arguments, true, true, 0)?;
        collector.used
    };
    Ok(&storage[..used])
}
struct Collector<'a, 's> {
    storage: &'s mut [Parameter<'a>],
    used: usize,
}
impl<'a> Collector<'a, '_> {
    fn walk(
        &mut self,
        schema: Value<'a>,
        value: Option<Value<'a>>,
        reachable: bool,
        root: bool,
        depth: usize,
    ) -> Result<(), HttpError> {
        if depth > 64 {
            return Err(HttpError::InvalidSchema);
        }
        if schema.kind() == Kind::Bool {
            return Ok(());
        }
        if schema.kind() != Kind::Object {
            return Err(HttpError::InvalidSchema);
        }
        if let Some(annotation) = schema.get("x-mcp-header") {
            if !reachable || root {
                return Err(HttpError::InvalidSchema);
            }
            let name = annotation.string().ok_or(HttpError::InvalidSchema)?;
            if name.decoded_len() == 0 || !name.chars().all(|c| c.is_ascii() && tchar(c as u8)) {
                return Err(HttpError::InvalidSchema);
            }
            if self.storage[..self.used].iter().any(|p| {
                p.name.is_some_and(|other| {
                    name.chars()
                        .map(|c| c.to_ascii_lowercase())
                        .eq(other.chars().map(|c| c.to_ascii_lowercase()))
                })
            }) {
                return Err(HttpError::InvalidSchema);
            }
            let kind = primitive(schema.get("type").ok_or(HttpError::InvalidSchema)?)?;
            let value = value.filter(|v| v.kind() != Kind::Null);
            if let Some(value) = value {
                check_primitive(kind, value)?;
            }
            if self.used == self.storage.len() {
                return Err(HttpError::Capacity {
                    needed: self.used.saturating_add(1),
                });
            }
            self.storage[self.used] = Parameter {
                name: Some(name),
                value,
                kind,
            };
            self.used += 1;
        }
        for (key, child) in schema.members() {
            if key.matches("properties") {
                if child.kind() != Kind::Object {
                    return Err(HttpError::InvalidSchema);
                }
                for (property, nested) in child.members() {
                    let instance = value.and_then(|v| {
                        v.members()
                            .find_map(|(k, v)| k.equals(property).then_some(v))
                    });
                    self.walk(nested, instance, reachable, false, depth + 1)?;
                }
            } else if [
                "$defs",
                "definitions",
                "patternProperties",
                "dependentSchemas",
            ]
            .iter()
            .any(|name| key.matches(name))
            {
                if child.kind() != Kind::Object {
                    return Err(HttpError::InvalidSchema);
                }
                for (_, nested) in child.members() {
                    self.walk(nested, None, false, false, depth + 1)?;
                }
            } else if ["allOf", "anyOf", "oneOf", "prefixItems"]
                .iter()
                .any(|name| key.matches(name))
            {
                if child.kind() != Kind::Array {
                    return Err(HttpError::InvalidSchema);
                }
                for nested in child.elements() {
                    self.walk(nested, None, false, false, depth + 1)?;
                }
            } else if [
                "items",
                "additionalItems",
                "contains",
                "not",
                "if",
                "then",
                "else",
                "additionalProperties",
                "unevaluatedProperties",
                "unevaluatedItems",
                "propertyNames",
            ]
            .iter()
            .any(|name| key.matches(name))
            {
                if child.kind() == Kind::Array {
                    for nested in child.elements() {
                        self.walk(nested, None, false, false, depth + 1)?;
                    }
                } else {
                    self.walk(child, None, false, false, depth + 1)?;
                }
            }
        }
        Ok(())
    }
}
fn primitive(value: Value<'_>) -> Result<Primitive, HttpError> {
    fn one(value: Value<'_>) -> Result<Primitive, HttpError> {
        let name = value.string().ok_or(HttpError::InvalidSchema)?;
        if name.matches("string") {
            Ok(Primitive::String)
        } else if name.matches("integer") {
            Ok(Primitive::Integer)
        } else if name.matches("boolean") {
            Ok(Primitive::Boolean)
        } else {
            Err(HttpError::InvalidSchema)
        }
    }
    if value.kind() != Kind::Array {
        return one(value);
    }
    let mut found = None;
    let mut nullable = false;
    for item in value.elements() {
        if item.string().is_some_and(|s| s.matches("null")) {
            if nullable {
                return Err(HttpError::InvalidSchema);
            }
            nullable = true;
        } else {
            if found.is_some() {
                return Err(HttpError::InvalidSchema);
            }
            found = Some(one(item)?);
        }
    }
    found.ok_or(HttpError::InvalidSchema)
}
fn integer(value: Value<'_>) -> Result<i64, HttpError> {
    if value.kind() != Kind::Number {
        return Err(HttpError::BadRequest);
    }
    exact_integer(value.raw())
}
fn exact_integer(text: &str) -> Result<i64, HttpError> {
    let (negative, text) = if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else {
        (false, text)
    };
    let (mantissa, exponent) = if let Some(at) = text.find(['e', 'E']) {
        let exponent = text[at + 1..]
            .parse::<i64>()
            .map_err(|_| HttpError::BadRequest)?;
        (&text[..at], exponent)
    } else {
        (text, 0)
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (mantissa.contains('.') && fraction.is_empty())
    {
        return Err(HttpError::BadRequest);
    }
    let point = (whole.len() as i64).saturating_add(exponent);
    let mut value: i64 = 0;
    const LIMIT: i64 = 9_007_199_254_740_991;
    for (at, digit) in whole.bytes().chain(fraction.bytes()).enumerate() {
        let digit = i64::from(digit - b'0');
        if (at as i64) < point {
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(digit))
                .filter(|&v| v <= LIMIT)
                .ok_or(HttpError::BadRequest)?;
        } else if digit != 0 {
            return Err(HttpError::BadRequest);
        }
    }
    let zeros = point.saturating_sub((whole.len() + fraction.len()) as i64);
    if value != 0 && zeros > 16 {
        return Err(HttpError::BadRequest);
    }
    if value != 0 {
        for _ in 0..zeros.max(0) {
            value = value
                .checked_mul(10)
                .filter(|&v| v <= LIMIT)
                .ok_or(HttpError::BadRequest)?;
        }
    }
    Ok(if negative { -value } else { value })
}
fn check_primitive(kind: Primitive, value: Value<'_>) -> Result<(), HttpError> {
    match kind {
        Primitive::String if value.kind() == Kind::String => Ok(()),
        Primitive::Boolean if value.kind() == Kind::Bool => Ok(()),
        Primitive::Integer => integer(value).map(|_| ()),
        _ => Err(HttpError::BadRequest),
    }
}
/// Append CRLF-terminated Mcp-Param headers. Omitted/null properties are omitted.
/// scratch stores only one decoded string at a time; output contains all headers.
pub fn write_parameter_headers(
    parameters: &[Parameter<'_>],
    output: &mut [u8],
    scratch: &mut [u8],
) -> Result<usize, HttpError> {
    let mut w = Bytes::new(output);
    for parameter in parameters {
        let (Some(name), Some(value)) = (parameter.name, parameter.value) else {
            continue;
        };
        w.raw(b"Mcp-Param-");
        for c in name.chars() {
            w.raw(&[c as u8]);
        }
        w.raw(b": ");
        match parameter.kind {
            Primitive::String => {
                let text = value
                    .string()
                    .ok_or(HttpError::BadRequest)?
                    .decode_into(scratch)
                    .map_err(|e| HttpError::Capacity { needed: e.needed })?;
                encode_value(text, &mut w);
            }
            Primitive::Boolean => w.raw(if value.boolean() == Some(true) {
                b"true"
            } else {
                b"false"
            }),
            Primitive::Integer => {
                let n = integer(value)?;
                let _ = write!(w, "{n}");
            }
        }
        w.raw(b"\r\n");
    }
    w.finish()
}
/// Reject missing, duplicate, malformed, unexpected-null and mismatching recognized
/// Mcp-Param headers. Unknown custom names remain the host/intermediary's concern.
pub fn validate_parameter_headers(
    parameters: &[Parameter<'_>],
    headers: &[Header<'_>],
    scratch: &mut [u8],
) -> Result<(), HttpError> {
    for parameter in parameters {
        let Some(name) = parameter.name else {
            continue;
        };
        let mut found = None;
        for h in headers {
            let matches = h
                .name
                .get(..10)
                .is_some_and(|p| p.eq_ignore_ascii_case("Mcp-Param-"))
                && h.name.get(10..).is_some_and(|suffix| {
                    name.chars()
                        .map(|c| c.to_ascii_lowercase())
                        .eq(suffix.chars().map(|c| c.to_ascii_lowercase()))
                });
            if matches {
                if found.is_some() {
                    return Err(HttpError::HeaderMismatch);
                }
                found = Some(h.value);
            }
        }
        match (parameter.value, found) {
            (None, None) => {}
            (Some(value), Some(wire)) => {
                let decoded = decode_header_value(wire, scratch).map_err(|e| {
                    if matches!(e, HttpError::Capacity { .. }) {
                        e
                    } else {
                        HttpError::HeaderMismatch
                    }
                })?;
                let matches = match parameter.kind {
                    Primitive::String => value.string().is_some_and(|s| s.matches(decoded)),
                    Primitive::Boolean => decoded == value.raw(),
                    Primitive::Integer => {
                        let expected = integer(value)?;
                        exact_integer(decoded).ok() == Some(expected)
                    }
                };
                if !matches {
                    return Err(HttpError::HeaderMismatch);
                }
            }
            _ => return Err(HttpError::HeaderMismatch),
        }
    }
    Ok(())
}
