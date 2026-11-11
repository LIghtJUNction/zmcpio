//! MCP tools profile: modern per-request metadata and explicit legacy handshake.
//! Handling a tool call returns a borrowed action; it never executes a tool.
use crate::{
    Error, ErrorKind,
    json::{Document, JsonString, Kind, Limits, Token, Value},
    rpc::{self, Id, Message, Notification, Request},
    writer::Writer,
};

pub const PROTOCOL_VERSION: &str = "2026-07-28";
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
pub const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
pub const CLIENT_CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Era {
    Modern,
    Legacy,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacyState {
    New,
    AwaitInitialized,
    Ready,
}
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub tools: bool,
    pub tools_list_changed: bool,
}
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            tools: true,
            tools_list_changed: false,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    pub request: Request<'a>,
    pub era: Era,
    /// Modern capabilities are scoped to this request, never cached across calls.
    pub client_capabilities: Option<Value<'a>>,
}
#[derive(Clone, Copy, Debug)]
pub struct ToolCall<'a> {
    pub context: Context<'a>,
    pub name: JsonString<'a>,
    pub arguments: Option<Value<'a>>,
    pub progress_token: Option<Value<'a>>,
}
impl ToolCall<'_> {
    pub fn arguments_bytes(&self) -> &[u8] {
        self.arguments.map_or(b"{}", |v| v.raw().as_bytes())
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Notice<'a> {
    Initialized,
    Cancelled {
        request_id: Id<'a>,
        reason: Option<JsonString<'a>>,
    },
    Progress {
        token: Value<'a>,
        progress: f64,
        total: Option<f64>,
        message: Option<JsonString<'a>>,
    },
    Other(Notification<'a>),
}
#[derive(Clone, Copy, Debug)]
pub enum Action<'a> {
    Reply(usize),
    ListTools {
        context: Context<'a>,
        cursor: Option<JsonString<'a>>,
    },
    CallTool(ToolCall<'a>),
    Notification(Notice<'a>),
    /// Incoming replies are returned to the caller, never answered with another reply.
    IncomingResponse(Message<'a>),
}

pub struct Server<'a> {
    name: &'a str,
    version: &'a str,
    capabilities: Capabilities,
    legacy: LegacyState,
}
impl<'a> Server<'a> {
    pub const fn new(name: &'a str, version: &'a str) -> Self {
        Self {
            name,
            version,
            capabilities: Capabilities {
                tools: true,
                tools_list_changed: false,
            },
            legacy: LegacyState::New,
        }
    }
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
    pub fn legacy_state(&self) -> LegacyState {
        self.legacy
    }
    pub const fn memory_bytes(&self) -> usize {
        core::mem::size_of::<Self>()
    }
    /// Parse exactly one message. Capacity/limit errors are local and retryable.
    /// Notifications never produce wire responses, including malformed notices.
    pub fn handle<'i>(
        &mut self,
        input: &'i [u8],
        tokens: &'i mut [Token],
        limits: Limits,
        output: &mut [u8],
    ) -> Result<Action<'i>, Error> {
        let doc = match Document::parse(input, tokens, limits) {
            Ok(d) => d,
            Err(e) if matches!(e.kind, ErrorKind::Json | ErrorKind::Utf8) => {
                return rpc::failure(None, -32700, "Parse error", None, output).map(Action::Reply);
            }
            Err(e) => return Err(e),
        };
        let message = match rpc::parse(doc.root()) {
            Ok(m) => m,
            Err(_) => {
                return rpc::failure(None, -32600, "Invalid Request", None, output)
                    .map(Action::Reply);
            }
        };
        match message {
            Message::Notification(n) => self.notice(n).map(Action::Notification),
            Message::Request(r) => match self.request(r, output) {
                Err(e)
                    if matches!(
                        e.kind,
                        ErrorKind::InvalidParams | ErrorKind::Type | ErrorKind::Integer
                    ) =>
                {
                    rpc::failure(Some(r.id), -32602, "Invalid params", None, output)
                        .map(Action::Reply)
                }
                result => result,
            },
            response => Ok(Action::IncomingResponse(response)),
        }
    }
    fn request<'i>(
        &mut self,
        request: Request<'i>,
        output: &mut [u8],
    ) -> Result<Action<'i>, Error> {
        let params = object_optional(request.params)?;
        let meta = object_optional(params.and_then(|p| p.get("_meta")))?;
        let version = string_optional(meta.and_then(|m| m.get(VERSION_KEY)))?;
        let (era, client_capabilities) = if let Some(version) = version {
            if !version.matches(PROTOCOL_VERSION) {
                let mut w = Writer::new(output);
                rpc::prefix(&mut w, Some(request.id));
                w.raw(b",\"error\":{\"code\":-32022,\"message\":\"Unsupported protocol version\",\"data\":{\"supported\":[\"");
                w.raw(PROTOCOL_VERSION.as_bytes());
                w.raw(b"\"],\"requested\":");
                // The original field is already validated JSON, including its escaping.
                w.raw(
                    meta.and_then(|m| m.get(VERSION_KEY))
                        .ok_or(Error::new(ErrorKind::InvalidParams, 0))?
                        .raw()
                        .as_bytes(),
                );
                w.raw(b"}}}");
                return w.finish().map(Action::Reply);
            }
            let caps = meta
                .and_then(|m| m.get(CLIENT_CAPABILITIES_KEY))
                .ok_or(Error::new(ErrorKind::InvalidParams, 0))?;
            validate_capabilities(caps)?;
            if let Some(info) = meta.and_then(|m| m.get("io.modelcontextprotocol/clientInfo")) {
                validate_info(info)?;
            }
            (Era::Modern, Some(caps))
        } else {
            if request.method.matches("initialize") {
                return self.initialize(request, output);
            }
            (Era::Legacy, None)
        };
        if request.method.matches("ping") && era == Era::Legacy {
            return self.empty(request.id, era, output).map(Action::Reply);
        }
        if era == Era::Legacy && self.legacy != LegacyState::Ready {
            return rpc::failure(
                Some(request.id),
                -32600,
                "Initialization required",
                None,
                output,
            )
            .map(Action::Reply);
        }
        if request.method.matches("server/discover") && era == Era::Modern {
            let mut w = Writer::new(output);
            self.begin_result(&mut w, request.id, era);
            w.raw(b"\"supportedVersions\":[\"");
            w.raw(PROTOCOL_VERSION.as_bytes());
            w.raw(b"\",\"");
            w.raw(LEGACY_PROTOCOL_VERSION.as_bytes());
            w.raw(b"\"],\"capabilities\":");
            self.write_capabilities(&mut w, era);
            w.raw(b",\"ttlMs\":0,\"cacheScope\":\"private\"}}");
            return w.finish().map(Action::Reply);
        }
        let context = Context {
            request,
            era,
            client_capabilities,
        };
        if self.capabilities.tools && request.method.matches("tools/list") {
            let cursor = string_optional(params.and_then(|p| p.get("cursor")))?;
            return Ok(Action::ListTools { context, cursor });
        }
        if self.capabilities.tools && request.method.matches("tools/call") {
            let p = params.ok_or(Error::new(ErrorKind::InvalidParams, 0))?;
            let name = p
                .require("name")?
                .string()
                .ok_or(Error::new(ErrorKind::InvalidParams, p.offset()))?;
            let arguments = object_optional(p.get("arguments"))?;
            let progress_token = meta.and_then(|m| m.get("progressToken"));
            if let Some(t) = progress_token {
                validate_progress_token(t)?;
            }
            return Ok(Action::CallTool(ToolCall {
                context,
                name,
                arguments,
                progress_token,
            }));
        }
        rpc::failure(Some(request.id), -32601, "Method not found", None, output).map(Action::Reply)
    }
    fn initialize<'i>(
        &mut self,
        request: Request<'i>,
        output: &mut [u8],
    ) -> Result<Action<'i>, Error> {
        if self.legacy != LegacyState::New {
            return rpc::failure(
                Some(request.id),
                -32600,
                "Already initialized",
                None,
                output,
            )
            .map(Action::Reply);
        }
        let p = object_optional(request.params)?.ok_or(Error::new(ErrorKind::InvalidParams, 0))?;
        p.require("protocolVersion")?
            .string()
            .ok_or(Error::new(ErrorKind::InvalidParams, p.offset()))?;
        validate_capabilities(p.require("capabilities")?)?;
        validate_info(p.require("clientInfo")?)?;
        let mut w = Writer::new(output);
        self.begin_result(&mut w, request.id, Era::Legacy);
        w.raw(b"\"protocolVersion\":\"");
        w.raw(LEGACY_PROTOCOL_VERSION.as_bytes());
        w.raw(b"\",\"capabilities\":");
        self.write_capabilities(&mut w, Era::Legacy);
        w.raw(b",\"serverInfo\":");
        self.write_info(&mut w);
        w.raw(b"}}");
        let n = w.finish()?;
        self.legacy = LegacyState::AwaitInitialized;
        Ok(Action::Reply(n))
    }
    fn notice<'i>(&mut self, notification: Notification<'i>) -> Result<Notice<'i>, Error> {
        let p = object_optional(notification.params)?;
        if notification.method.matches("notifications/initialized") {
            if self.legacy != LegacyState::AwaitInitialized {
                return Err(Error::new(ErrorKind::State, 0));
            }
            self.legacy = LegacyState::Ready;
            return Ok(Notice::Initialized);
        }
        if notification.method.matches("notifications/cancelled") {
            let p = p.ok_or(Error::new(ErrorKind::InvalidParams, 0))?;
            return Ok(Notice::Cancelled {
                request_id: Id::from_value(p.require("requestId")?)?,
                reason: string_optional(p.get("reason"))?,
            });
        }
        if notification.method.matches("notifications/progress") {
            let p = p.ok_or(Error::new(ErrorKind::InvalidParams, 0))?;
            let token = p.require("progressToken")?;
            validate_progress_token(token)?;
            let progress = nonnegative(p.require("progress")?)?;
            let total = p.get("total").map(nonnegative).transpose()?;
            return Ok(Notice::Progress {
                token,
                progress,
                total,
                message: string_optional(p.get("message"))?,
            });
        }
        Ok(Notice::Other(notification))
    }
    pub fn error(
        &self,
        context: Context<'_>,
        code: i32,
        message: &str,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        rpc::failure(Some(context.request.id), code, message, None, output)
    }
    pub fn empty(&self, id: Id<'_>, era: Era, output: &mut [u8]) -> Result<usize, Error> {
        let mut w = Writer::new(output);
        rpc::prefix(&mut w, Some(id));
        w.raw(b",\"result\":{");
        if era == Era::Modern {
            w.raw(b"\"resultType\":\"complete\",");
            self.write_meta(&mut w);
        }
        w.raw(b"}}");
        w.finish()
    }
    pub(crate) fn begin_result(&self, w: &mut Writer<'_>, id: Id<'_>, era: Era) {
        rpc::prefix(w, Some(id));
        w.raw(b",\"result\":{");
        if era == Era::Modern {
            w.raw(b"\"resultType\":\"complete\",");
            self.write_meta(w);
            w.raw(b",");
        }
    }
    fn write_info(&self, w: &mut Writer<'_>) {
        w.raw(b"{\"name\":");
        w.text(self.name);
        w.raw(b",\"version\":");
        w.text(self.version);
        w.raw(b"}");
    }
    fn write_meta(&self, w: &mut Writer<'_>) {
        w.raw(b"\"_meta\":{\"io.modelcontextprotocol/serverInfo\":");
        self.write_info(w);
        w.raw(b"}");
    }
    fn write_capabilities(&self, w: &mut Writer<'_>, era: Era) {
        w.raw(b"{");
        if self.capabilities.tools {
            w.raw(b"\"tools\":{");
            if era == Era::Legacy {
                w.raw(b"\"listChanged\":");
                w.raw(if self.capabilities.tools_list_changed {
                    b"true"
                } else {
                    b"false"
                });
            }
            w.raw(b"}");
        }
        w.raw(b"}");
    }
}

pub(crate) fn object_optional(value: Option<Value<'_>>) -> Result<Option<Value<'_>>, Error> {
    if let Some(v) = value
        && v.kind() != Kind::Object
    {
        return Err(Error::new(ErrorKind::InvalidParams, v.offset()));
    }
    Ok(value)
}
fn string_optional(value: Option<Value<'_>>) -> Result<Option<JsonString<'_>>, Error> {
    value
        .map(|v| {
            v.string()
                .ok_or(Error::new(ErrorKind::InvalidParams, v.offset()))
        })
        .transpose()
}
fn validate_info(value: Value<'_>) -> Result<(), Error> {
    object_optional(Some(value))?;
    for field in ["name", "version"] {
        value
            .require(field)?
            .string()
            .ok_or(Error::new(ErrorKind::InvalidParams, value.offset()))?;
    }
    Ok(())
}
fn validate_capabilities(value: Value<'_>) -> Result<(), Error> {
    object_optional(Some(value))?;
    for name in [
        "roots",
        "sampling",
        "elicitation",
        "experimental",
        "extensions",
    ] {
        object_optional(value.get(name))?;
    }
    Ok(())
}
pub(crate) fn validate_progress_token(value: Value<'_>) -> Result<(), Error> {
    if !matches!(value.kind(), Kind::String | Kind::Number) {
        return Err(Error::new(ErrorKind::InvalidParams, value.offset()));
    }
    if value.kind() == Kind::Number {
        let n: f64 = value
            .raw()
            .parse()
            .map_err(|_| Error::new(ErrorKind::InvalidParams, value.offset()))?;
        if !n.is_finite() {
            return Err(Error::new(ErrorKind::InvalidParams, value.offset()));
        }
    }
    Ok(())
}
fn nonnegative(value: Value<'_>) -> Result<f64, Error> {
    if value.kind() != Kind::Number {
        return Err(Error::new(ErrorKind::InvalidParams, value.offset()));
    }
    let n: f64 = value
        .raw()
        .parse()
        .map_err(|_| Error::new(ErrorKind::InvalidParams, value.offset()))?;
    if n.is_finite() && n >= 0.0 {
        Ok(n)
    } else {
        Err(Error::new(ErrorKind::InvalidParams, value.offset()))
    }
}

/// Validate the HTTP routing envelope without performing HTTP I/O.
/// The adapter must turn a mismatch into HTTP 400 / MCP -32020.
pub struct HttpHeaders<'a> {
    pub protocol_version: &'a str,
    pub method: &'a str,
    pub name: Option<&'a str>,
}
impl HttpHeaders<'_> {
    pub fn validate(&self, request: Request<'_>) -> Result<(), Error> {
        let p = object_optional(request.params)?.ok_or(Error::new(ErrorKind::InvalidParams, 0))?;
        let meta =
            object_optional(p.get("_meta"))?.ok_or(Error::new(ErrorKind::InvalidParams, 0))?;
        let version = meta
            .require(VERSION_KEY)?
            .string()
            .ok_or(Error::new(ErrorKind::InvalidParams, 0))?;
        if !version.matches(self.protocol_version) || !request.method.matches(self.method) {
            return Err(Error::new(ErrorKind::InvalidParams, 0));
        }
        if request.method.matches("tools/call") {
            let name = p
                .require("name")?
                .string()
                .ok_or(Error::new(ErrorKind::InvalidParams, 0))?;
            if !self.name.is_some_and(|n| name.matches(n)) {
                return Err(Error::new(ErrorKind::InvalidParams, 0));
            }
        }
        Ok(())
    }
}
