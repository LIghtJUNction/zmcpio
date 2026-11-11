//! Bounded, transport-independent MCP client state machine.
//!
//! Encoding reserves an ID only after the complete message fits. Transport send
//! failures are ambiguous: retain the encoded bytes and decide whether to close
//! or abandon the request; never automatically repeat a side-effecting call.
use crate::{
    Error, ErrorKind,
    extensions::{self, Method},
    json::{Document, JsonString, Kind, Limits, Token, Value},
    mcp::{Era, LEGACY_PROTOCOL_VERSION, PROTOCOL_VERSION},
    notifications,
    rpc::{self, Id, Message, Notification, Request, RpcError},
    writer::Writer,
};

/// Tested legacy tools profiles. No implicit fallback between protocol eras.
pub const LEGACY_COMPATIBLE_VERSIONS: [&str; 4] = [
    LEGACY_PROTOCOL_VERSION,
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientState {
    New,
    Initializing,
    AwaitInitialized,
    Ready,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Initialize,
    Discover,
    ListTools,
    CallTool,
    Ping,
    Extension(Method),
}

/// One caller-owned outstanding-request slot. No input buffer is retained here.
#[derive(Clone, Copy, Debug)]
pub struct Pending {
    id: i64,
    operation: Option<Operation>,
    progress_requested: bool,
    task_requested: bool,
    progress: Option<f64>,
}
impl Pending {
    pub const EMPTY: Self = Self {
        id: 0,
        operation: None,
        progress_requested: false,
        task_requested: false,
        progress: None,
    };
}

/// Send exactly `output[..len]` using the application-selected transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Outgoing {
    pub id: i64,
    pub len: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct ServerInfo<'a> {
    pub name: JsonString<'a>,
    pub version: JsonString<'a>,
    pub raw: Value<'a>,
}
impl<'a> ServerInfo<'a> {
    fn parse(raw: Value<'a>) -> Result<Self, Error> {
        object(raw)?;
        Ok(Self {
            name: string(raw.require("name")?)?,
            version: string(raw.require("version")?)?,
            raw,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Discovery<'a> {
    pub raw: Value<'a>,
    pub capabilities: Value<'a>,
    pub supported_versions: Value<'a>,
    pub server_info: Option<ServerInfo<'a>>,
}

/// Borrowed discovered tool. Descriptions, annotations and schemas are untrusted.
#[derive(Clone, Copy, Debug)]
pub struct ToolView<'a> {
    raw: Value<'a>,
    name: JsonString<'a>,
    description: Option<JsonString<'a>>,
    input_schema: Value<'a>,
    output_schema: Option<Value<'a>>,
}
impl<'a> ToolView<'a> {
    pub fn parse(raw: Value<'a>) -> Result<Self, Error> {
        object(raw)?;
        let name = string(raw.require("name")?)?;
        if name.decoded_len() == 0 || name.decoded_len() > 128 {
            return Err(invalid(raw));
        }
        let description = raw.get("description").map(string).transpose()?;
        let input_schema = object(raw.require("inputSchema")?)?;
        if !input_schema
            .get("type")
            .and_then(Value::string)
            .is_some_and(|v| v.matches("object"))
        {
            return Err(invalid(input_schema));
        }
        let output_schema = raw.get("outputSchema");
        if output_schema.is_some_and(|v| !matches!(v.kind(), Kind::Object | Kind::Bool)) {
            return Err(invalid(raw));
        }
        Ok(Self {
            raw,
            name,
            description,
            input_schema,
            output_schema,
        })
    }
    pub fn raw(self) -> Value<'a> {
        self.raw
    }
    pub fn name(self) -> JsonString<'a> {
        self.name
    }
    pub fn description(self) -> Option<JsonString<'a>> {
        self.description
    }
    pub fn input_schema(self) -> Value<'a> {
        self.input_schema
    }
    pub fn output_schema(self) -> Option<Value<'a>> {
        self.output_schema
    }
}

/// One validated page. Keep its input and token tape alive while using its views.
#[derive(Clone, Copy, Debug)]
pub struct ToolPage<'a> {
    raw: Value<'a>,
    tools: Value<'a>,
    next_cursor: Option<JsonString<'a>>,
}
impl<'a> ToolPage<'a> {
    fn parse(raw: Value<'a>, era: Era) -> Result<Self, Error> {
        let tools = array(raw.require("tools")?)?;
        let next_cursor = raw.get("nextCursor").map(string).transpose()?;
        for (index, value) in tools.elements().enumerate() {
            let tool = ToolView::parse(value)?;
            if era == Era::Legacy
                && let Some(schema) = tool.output_schema
            {
                object(schema)?;
                if !schema
                    .get("type")
                    .and_then(Value::string)
                    .is_some_and(|v| v.matches("object"))
                {
                    return Err(invalid(schema));
                }
            }
            for prior in tools.elements().take(index) {
                if ToolView::parse(prior)?.name.equals(tool.name) {
                    return Err(Error::new(ErrorKind::DuplicateKey, value.offset()));
                }
            }
        }
        Ok(Self {
            raw,
            tools,
            next_cursor,
        })
    }
    pub fn raw(self) -> Value<'a> {
        self.raw
    }
    pub fn next_cursor(self) -> Option<JsonString<'a>> {
        self.next_cursor
    }
    pub fn tools(self) -> impl Iterator<Item = ToolView<'a>> {
        // Every element was validated above; immutable borrowed storage cannot change.
        self.tools
            .elements()
            .filter_map(|v| ToolView::parse(v).ok())
    }
}

/// A final tools/call result, distinct from JSON-RPC failure and MRTR input requests.
#[derive(Clone, Copy, Debug)]
pub struct CallResult<'a> {
    raw: Value<'a>,
    content: Value<'a>,
    structured_content: Option<Value<'a>>,
    is_error: bool,
}
impl<'a> CallResult<'a> {
    pub fn parse(raw: Value<'a>, era: Era) -> Result<Self, Error> {
        object(raw)?;
        if era == Era::Modern && !complete_result(raw)? {
            return Err(Error::new(ErrorKind::UnsupportedResult, raw.offset()));
        }
        let content = array(raw.require("content")?)?;
        for block in content.elements() {
            object(block)?;
            let kind = string(block.require("type")?)?;
            if kind.matches("text") {
                string(block.require("text")?)?;
            } else if kind.matches("image") || kind.matches("audio") {
                string(block.require("data")?)?;
                string(block.require("mimeType")?)?;
            } else if kind.matches("resource_link") {
                string(block.require("uri")?)?;
                string(block.require("name")?)?;
            } else if kind.matches("resource") {
                let resource = object(block.require("resource")?)?;
                string(resource.require("uri")?)?;
                resource.get("mimeType").map(string).transpose()?;
                match (resource.get("text"), resource.get("blob")) {
                    (Some(v), None) | (None, Some(v)) => {
                        string(v)?;
                    }
                    _ => return Err(invalid(resource)),
                }
            } else {
                return Err(Error::new(ErrorKind::UnsupportedResult, block.offset()));
            }
        }
        let is_error = raw
            .get("isError")
            .map(|v| v.boolean().ok_or_else(|| invalid(v)))
            .transpose()?
            .unwrap_or(false);
        let structured_content = raw.get("structuredContent");
        if era == Era::Legacy
            && let Some(value) = structured_content
        {
            object(value)?;
        }
        Ok(Self {
            raw,
            content,
            structured_content,
            is_error,
        })
    }
    pub fn raw(self) -> Value<'a> {
        self.raw
    }
    pub fn content(self) -> Value<'a> {
        self.content
    }
    pub fn structured_content(self) -> Option<Value<'a>> {
        self.structured_content
    }
    pub fn is_error(self) -> bool {
        self.is_error
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Event<'a> {
    Initialized {
        id: i64,
        server: ServerInfo<'a>,
        capabilities: Value<'a>,
    },
    Discovered {
        id: i64,
        discovery: Discovery<'a>,
    },
    Tools {
        id: i64,
        page: ToolPage<'a>,
    },
    ToolResult {
        id: i64,
        result: CallResult<'a>,
    },
    Pong {
        id: i64,
    },
    /// A durable task handle, not a completed tool result. Poll/update/cancel
    /// explicitly; the client never starts a scheduler or repeats the tool call.
    TaskCreated {
        id: i64,
        task: extensions::TaskView<'a>,
    },
    /// Complete extension result. Application schemas and effects remain caller-owned.
    ExtensionResult {
        id: i64,
        method: Method,
        result: Value<'a>,
    },
    /// An uncorrelated error has no ID and does not complete any pending request.
    Failure {
        id: Option<i64>,
        error: RpcError<'a>,
    },
    /// The application owns user interaction and explicit continuation; no auto-retry.
    InputRequired {
        id: i64,
        result: Value<'a>,
    },
    Notification(Notification<'a>),
    /// Legacy only. The client does not execute server-provided callbacks.
    ServerRequest(Request<'a>),
}

pub struct Client<'a> {
    name: &'a str,
    version: &'a str,
    era: Era,
    state: ClientState,
    slots: &'a mut [Pending],
    next_id: u64,
    tools_supported: Option<bool>,
    legacy_tool_tasks: bool,
    negotiated_version: Option<&'static str>,
    client_capabilities: Option<Value<'a>>,
}
impl<'a> Client<'a> {
    /// Modern mode is immediately ready; legacy requires initialize/initialized.
    /// Reusing storage starts a new client identity and clears all pending slots.
    pub fn new(name: &'a str, version: &'a str, era: Era, slots: &'a mut [Pending]) -> Self {
        slots.fill(Pending::EMPTY);
        Self {
            name,
            version,
            era,
            state: if era == Era::Modern {
                ClientState::Ready
            } else {
                ClientState::New
            },
            slots,
            next_id: 1,
            tools_supported: None,
            legacy_tool_tasks: false,
            client_capabilities: None,
            negotiated_version: if era == Era::Modern {
                Some(PROTOCOL_VERSION)
            } else {
                None
            },
        }
    }
    /// Advertise only callbacks/extensions the application actually implements.
    /// Retain the capability input and token tape for the lifetime of this client.
    pub fn with_client_capabilities(mut self, capabilities: Value<'a>) -> Result<Self, Error> {
        extensions::validate_client_capabilities(capabilities)?;
        if self.pending_count() != 0
            || (self.era == Era::Legacy && self.state != ClientState::New)
            || self.state == ClientState::Closed
        {
            return Err(state());
        }
        self.client_capabilities = Some(capabilities);
        Ok(self)
    }

    pub fn state(&self) -> ClientState {
        self.state
    }
    pub fn era(&self) -> Era {
        self.era
    }
    /// Use this value for an application-owned HTTP protocol-version header.
    pub fn protocol_version(&self) -> Option<&'static str> {
        self.negotiated_version
    }
    pub fn pending_count(&self) -> usize {
        self.slots.iter().filter(|s| s.operation.is_some()).count()
    }
    pub fn memory_bytes(&self) -> usize {
        core::mem::size_of::<Self>() + core::mem::size_of_val(self.slots)
    }
    pub fn close(&mut self) {
        self.slots.fill(Pending::EMPTY);
        self.state = ClientState::Closed;
        self.negotiated_version = None;
    }

    pub fn initialize(&mut self, output: &mut [u8]) -> Result<Outgoing, Error> {
        self.initialize_with_version(LEGACY_PROTOCOL_VERSION, output)
    }

    /// Explicit legacy revision selection for old SSE peers; no automatic downgrade.
    pub fn initialize_with_version(
        &mut self,
        version: &str,
        output: &mut [u8],
    ) -> Result<Outgoing, Error> {
        if !LEGACY_COMPATIBLE_VERSIONS.contains(&version) {
            return Err(Error::new(ErrorKind::UnsupportedVersion, 0));
        }
        if self.era != Era::Legacy || self.state != ClientState::New {
            return Err(state());
        }
        let (id, slot) = self.prepare()?;
        let mut w = Writer::new(output);
        self.prefix(&mut w, id, "initialize");
        w.raw(b"\"protocolVersion\":");
        w.text(version);
        w.raw(b",\"capabilities\":");
        self.write_client_capabilities(&mut w);
        w.raw(b",\"clientInfo\":");
        self.info(&mut w);
        w.raw(b"}}");
        let len = w.finish()?;
        self.commit(slot, id, Operation::Initialize, false);
        self.state = ClientState::Initializing;
        Ok(Outgoing { id, len })
    }

    /// State changes only after encoding succeeds. Send these exact bytes before
    /// issuing tools requests. A failed send requires transport recovery or close.
    pub fn initialized(&mut self, output: &mut [u8]) -> Result<usize, Error> {
        if self.era != Era::Legacy || self.state != ClientState::AwaitInitialized {
            return Err(state());
        }
        let len = notifications::initialized(output)?;
        self.state = ClientState::Ready;
        Ok(len)
    }

    pub fn discover(&mut self, output: &mut [u8]) -> Result<Outgoing, Error> {
        if self.era != Era::Modern {
            return Err(state());
        }
        self.simple(Operation::Discover, "server/discover", output)
    }
    pub fn ping(&mut self, output: &mut [u8]) -> Result<Outgoing, Error> {
        if self.era != Era::Legacy {
            return Err(state());
        }
        self.simple(Operation::Ping, "ping", output)
    }
    pub fn list_tools(
        &mut self,
        cursor: Option<&str>,
        output: &mut [u8],
    ) -> Result<Outgoing, Error> {
        self.require_tools()?;
        let (id, slot) = self.prepare()?;
        let mut w = Writer::new(output);
        self.prefix(&mut w, id, "tools/list");
        self.meta(&mut w, None);
        if let Some(cursor) = cursor {
            if self.era == Era::Modern {
                w.raw(b",");
            }
            w.raw(b"\"cursor\":");
            w.text(cursor);
        }
        w.raw(b"}}");
        let len = w.finish()?;
        self.commit(slot, id, Operation::ListTools, false);
        Ok(Outgoing { id, len })
    }
    pub fn call_tool(
        &mut self,
        name: &str,
        arguments: Option<Value<'_>>,
        output: &mut [u8],
    ) -> Result<Outgoing, Error> {
        self.call(name, arguments, false, output)
    }
    /// Uses the MCP request ID as a numeric progress token, not a model call ID.
    pub fn call_tool_with_progress(
        &mut self,
        name: &str,
        arguments: Option<Value<'_>>,
        output: &mut [u8],
    ) -> Result<Outgoing, Error> {
        self.call(name, arguments, true, output)
    }
    fn call(
        &mut self,
        name: &str,
        arguments: Option<Value<'_>>,
        progress: bool,
        output: &mut [u8],
    ) -> Result<Outgoing, Error> {
        self.require_tools()?;
        if name.is_empty() || name.len() > 128 {
            return Err(Error::new(ErrorKind::InvalidParams, 0));
        }
        if let Some(args) = arguments {
            object(args)?;
        }
        let (id, slot) = self.prepare()?;
        let mut w = Writer::new(output);
        self.prefix(&mut w, id, "tools/call");
        self.meta(&mut w, progress.then_some(id));
        if self.era == Era::Modern || progress {
            w.raw(b",");
        }
        w.raw(b"\"name\":");
        w.text(name);
        w.raw(b",\"arguments\":");
        w.raw(arguments.map_or(b"{}", |v| v.raw().as_bytes()));
        w.raw(b"}}");
        let len = w.finish()?;
        self.commit(slot, id, Operation::CallTool, progress);
        Ok(Outgoing { id, len })
    }

    /// Encode an opted-in method with caller-provided parameters. Modern reserved
    /// metadata is added automatically; custom metadata and opaque requestState
    /// are retained. MRTR retries are explicit new requests with new IDs.
    pub fn request(
        &mut self,
        method: Method,
        params: Option<Value<'_>>,
        output: &mut [u8],
    ) -> Result<Outgoing, Error> {
        self.ready()?;
        if method.is_callback() || !method.available_at(self.era, self.protocol_version()) {
            return Err(state());
        }
        if method == Method::ToolsCall {
            self.require_tools()?;
        }
        method.validate_params(params)?;
        if self.era == Era::Modern
            && method.needs_task_capability(params)
            && !extensions::has_task_capability(self.client_capabilities)
        {
            return Err(state());
        }
        let task_requested = params.and_then(|value| value.get("task"));
        if let Some(task) = task_requested {
            object(task)?;
            if self.era != Era::Legacy
                || method != Method::ToolsCall
                || self.protocol_version() != Some(LEGACY_PROTOCOL_VERSION)
                || !self.legacy_tool_tasks
            {
                return Err(state());
            }
            task.get("ttl").map(nonnegative).transpose()?;
        }
        let meta = params.and_then(|value| value.get("_meta"));
        if let Some(meta) = meta {
            object(meta)?;
            for reserved in [
                crate::mcp::VERSION_KEY,
                crate::mcp::CLIENT_CAPABILITIES_KEY,
                "io.modelcontextprotocol/clientInfo",
            ] {
                if meta.get(reserved).is_some() {
                    return Err(invalid(meta));
                }
            }
            if let Some(progress) = meta.get("progressToken") {
                crate::mcp::validate_progress_token(progress)?;
            }
        }
        let (id, slot) = self.prepare()?;
        let mut w = Writer::new(output);
        self.prefix(&mut w, id, method.as_str());
        let mut comma = false;
        if self.era == Era::Modern {
            w.raw(b"\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":");
            w.text(PROTOCOL_VERSION);
            w.raw(b",\"io.modelcontextprotocol/clientCapabilities\":");
            self.write_client_capabilities(&mut w);
            w.raw(b",\"io.modelcontextprotocol/clientInfo\":");
            self.info(&mut w);
            if let Some(meta) = meta {
                for (key, value) in meta.members() {
                    w.raw(b",");
                    extensions::write_key(&mut w, key);
                    w.raw(value.raw().as_bytes());
                }
            }
            w.raw(b"}");
            comma = true;
        }
        if let Some(params) = params {
            for (key, value) in params.members() {
                if self.era == Era::Modern && key.matches("_meta") {
                    continue;
                }
                if comma {
                    w.raw(b",");
                }
                extensions::write_key(&mut w, key);
                w.raw(value.raw().as_bytes());
                comma = true;
            }
        }
        w.raw(b"}}");
        let len = w.finish()?;
        let progress = meta
            .and_then(|value| value.get("progressToken"))
            .is_some_and(|value| value.integer().ok() == Some(id));
        let operation = if method == Method::ToolsCall {
            Operation::CallTool
        } else {
            Operation::Extension(method)
        };
        self.commit(slot, id, operation, progress);
        self.slots[slot].task_requested = task_requested.is_some();
        Ok(Outgoing { id, len })
    }

    /// Encode cancellation for stdio or legacy HTTP. Modern HTTP cancels by
    /// closing the applicable response stream. Neither proves effects were undone.
    pub fn cancel(&self, id: i64, reason: Option<&str>, output: &mut [u8]) -> Result<usize, Error> {
        let slot = self.find(id)?;
        if self.slots[slot].operation == Some(Operation::Initialize) {
            return Err(state());
        }
        notifications::cancelled(Id::Number(id), reason, output)
    }
    /// Stop retaining this request locally, e.g. after an application timeout.
    /// Its ID is never reused; a late response is rejected as UnknownId.
    pub fn abandon(&mut self, id: i64) -> Result<(), Error> {
        let slot = self.find(id)?;
        self.release(slot);
        Ok(())
    }

    /// Validate and correlate one complete message. Parsing/validation errors
    /// leave pending slots and lifecycle intact so larger buffers can be retried.
    pub fn receive<'i>(
        &mut self,
        input: &'i [u8],
        tokens: &'i mut [Token],
        limits: Limits,
    ) -> Result<Event<'i>, Error> {
        if self.state == ClientState::Closed {
            return Err(state());
        }
        let doc = Document::parse(input, tokens, limits)?;
        match rpc::parse(doc.root())? {
            Message::Notification(n) => {
                self.notice(n)?;
                Ok(Event::Notification(n))
            }
            Message::Request(r) => {
                if self.era == Era::Modern {
                    return Err(Error::new(ErrorKind::InvalidRpc, 0));
                }
                Ok(Event::ServerRequest(r))
            }
            Message::Failure { id: None, error } => Ok(Event::Failure { id: None, error }),
            Message::Failure {
                id: Some(id),
                error,
            } => {
                let id = numeric_id(id)?;
                let slot = self.find(id)?;
                self.release(slot);
                Ok(Event::Failure {
                    id: Some(id),
                    error,
                })
            }
            Message::Success { id, result } => {
                let id = numeric_id(id)?;
                let slot = self.find(id)?;
                object(result)?;
                if self.era == Era::Modern {
                    validate_meta(result)?;
                    if result
                        .get("resultType")
                        .and_then(Value::string)
                        .is_some_and(|kind| kind.matches("task"))
                    {
                        if self.slots[slot].operation != Some(Operation::CallTool)
                            || !extensions::has_task_capability(self.client_capabilities)
                        {
                            return Err(Error::new(ErrorKind::UnsupportedResult, result.offset()));
                        }
                        let task = extensions::TaskView::parse(result, self.era)?;
                        self.slots[slot] = Pending::EMPTY;
                        return Ok(Event::TaskCreated { id, task });
                    }
                    if !complete_result(result)? {
                        let supports_input = match self.slots[slot].operation {
                            Some(Operation::CallTool) => true,
                            Some(Operation::Extension(method)) => method.supports_input(),
                            _ => false,
                        };
                        if !supports_input {
                            return Err(Error::new(ErrorKind::UnsupportedResult, result.offset()));
                        }
                        validate_input_required(result)?;
                        self.validate_result_inputs(result)?;
                        self.slots[slot] = Pending::EMPTY;
                        return Ok(Event::InputRequired { id, result });
                    }
                }
                let event = match self.slots[slot].operation {
                    Some(Operation::Initialize) => {
                        let requested = string(result.require("protocolVersion")?)?;
                        let negotiated = LEGACY_COMPATIBLE_VERSIONS
                            .iter()
                            .copied()
                            .find(|version| requested.matches(version))
                            .ok_or(Error::new(ErrorKind::UnsupportedVersion, result.offset()))?;
                        let caps = capabilities(result.require("capabilities")?)?;
                        let server = ServerInfo::parse(result.require("serverInfo")?)?;
                        self.tools_supported = Some(caps.get("tools").is_some());
                        self.legacy_tool_tasks = caps
                            .get("tasks")
                            .and_then(|v| v.get("requests"))
                            .and_then(|v| v.get("tools"))
                            .and_then(|v| v.get("call"))
                            .is_some_and(|v| v.kind() == Kind::Object);
                        self.negotiated_version = Some(negotiated);
                        self.state = ClientState::AwaitInitialized;
                        Event::Initialized {
                            id,
                            server,
                            capabilities: caps,
                        }
                    }
                    Some(Operation::Discover) => {
                        let versions = array(result.require("supportedVersions")?)?;
                        let mut supported = false;
                        for v in versions.elements() {
                            supported |= string(v)?.matches(PROTOCOL_VERSION);
                        }
                        if !supported {
                            return Err(Error::new(ErrorKind::UnsupportedVersion, result.offset()));
                        }
                        let caps = capabilities(result.require("capabilities")?)?;
                        let server_info = validate_meta(result)?;
                        self.tools_supported = Some(caps.get("tools").is_some());
                        Event::Discovered {
                            id,
                            discovery: Discovery {
                                raw: result,
                                capabilities: caps,
                                supported_versions: versions,
                                server_info,
                            },
                        }
                    }
                    Some(Operation::ListTools) => Event::Tools {
                        id,
                        page: ToolPage::parse(result, self.era)?,
                    },
                    Some(Operation::CallTool) => {
                        if self.era == Era::Legacy && self.slots[slot].task_requested {
                            let task =
                                extensions::TaskView::parse(result.require("task")?, self.era)?;
                            Event::TaskCreated { id, task }
                        } else {
                            Event::ToolResult {
                                id,
                                result: CallResult::parse(result, self.era)?,
                            }
                        }
                    }
                    Some(Operation::Ping) => Event::Pong { id },
                    Some(Operation::Extension(method)) => {
                        method.validate_result_for(self.era, result)?;
                        if self.era == Era::Modern && method == Method::TasksGet {
                            self.validate_result_inputs(result)?;
                        }
                        if method == Method::SubscriptionsListen {
                            let meta = object(result.require("_meta")?)?;
                            let subscription_id = Id::from_value(
                                meta.require("io.modelcontextprotocol/subscriptionId")?,
                            )?;
                            if !subscription_id.equals(Id::Number(id)) {
                                return Err(Error::new(
                                    ErrorKind::UnexpectedResponse,
                                    result.offset(),
                                ));
                            }
                        }
                        Event::ExtensionResult { id, method, result }
                    }
                    None => return Err(Error::new(ErrorKind::UnknownId, 0)),
                };
                self.slots[slot] = Pending::EMPTY;
                Ok(event)
            }
        }
    }
    fn simple(
        &mut self,
        operation: Operation,
        method: &str,
        output: &mut [u8],
    ) -> Result<Outgoing, Error> {
        self.ready()?;
        let (id, slot) = self.prepare()?;
        let mut w = Writer::new(output);
        self.prefix(&mut w, id, method);
        self.meta(&mut w, None);
        w.raw(b"}}");
        let len = w.finish()?;
        self.commit(slot, id, operation, false);
        Ok(Outgoing { id, len })
    }
    fn ready(&self) -> Result<(), Error> {
        if self.state == ClientState::Ready {
            Ok(())
        } else {
            Err(state())
        }
    }
    fn require_tools(&self) -> Result<(), Error> {
        self.ready()?;
        if self.tools_supported == Some(false) {
            return Err(state());
        }
        Ok(())
    }
    fn prepare(&self) -> Result<(i64, usize), Error> {
        let id = i64::try_from(self.next_id).map_err(|_| Error::new(ErrorKind::Integer, 0))?;
        let slot = self
            .slots
            .iter()
            .position(|s| s.operation.is_none())
            .ok_or_else(|| {
                Error::capacity(
                    ErrorKind::RequestSlots,
                    0,
                    self.slots.len().saturating_add(1),
                )
            })?;
        Ok((id, slot))
    }
    fn commit(&mut self, slot: usize, id: i64, operation: Operation, progress_requested: bool) {
        self.slots[slot] = Pending {
            id,
            operation: Some(operation),
            progress_requested,
            task_requested: false,
            progress: None,
        };
        self.next_id += 1;
    }
    fn find(&self, id: i64) -> Result<usize, Error> {
        self.slots
            .iter()
            .position(|s| s.operation.is_some() && s.id == id)
            .ok_or(Error::new(ErrorKind::UnknownId, 0))
    }
    fn release(&mut self, slot: usize) {
        if self.slots[slot].operation == Some(Operation::Initialize) {
            self.state = ClientState::New;
        }
        self.slots[slot] = Pending::EMPTY;
    }
    fn prefix(&self, w: &mut Writer<'_>, id: i64, method: &str) {
        rpc::prefix(w, Some(Id::Number(id)));
        w.raw(b",\"method\":");
        w.text(method);
        w.raw(b",\"params\":{");
    }
    fn info(&self, w: &mut Writer<'_>) {
        w.raw(b"{\"name\":");
        w.text(self.name);
        w.raw(b",\"version\":");
        w.text(self.version);
        w.raw(b"}");
    }
    fn meta(&self, w: &mut Writer<'_>, progress: Option<i64>) {
        if self.era == Era::Legacy && progress.is_none() {
            return;
        }
        w.raw(b"\"_meta\":{");
        if self.era == Era::Modern {
            w.raw(b"\"io.modelcontextprotocol/protocolVersion\":");
            w.text(PROTOCOL_VERSION);
            w.raw(b",\"io.modelcontextprotocol/clientCapabilities\":");
            self.write_client_capabilities(w);
            w.raw(b",\"io.modelcontextprotocol/clientInfo\":");
            self.info(w);
            if progress.is_some() {
                w.raw(b",");
            }
        }
        if let Some(id) = progress {
            w.raw(b"\"progressToken\":");
            w.integer(id);
        }
        w.raw(b"}");
    }
    fn write_client_capabilities(&self, w: &mut Writer<'_>) {
        w.raw(
            self.client_capabilities
                .map_or(b"{}", |value| value.raw().as_bytes()),
        );
    }
    fn validate_result_inputs(&self, result: Value<'_>) -> Result<(), Error> {
        if let Some(requests) = result.get("inputRequests") {
            object(requests)?;
            if requests.members().next().is_some() {
                let caps = self.client_capabilities.ok_or_else(state)?;
                extensions::validate_input_requests(requests, caps)?;
            }
        }
        Ok(())
    }
    fn notice(&mut self, n: Notification<'_>) -> Result<(), Error> {
        if !n.method.matches("notifications/progress") {
            return Ok(());
        }
        let params = object(n.params.ok_or(Error::new(ErrorKind::InvalidParams, 0))?)?;
        let token = params.require("progressToken")?;
        crate::mcp::validate_progress_token(token)?;
        let value = nonnegative(params.require("progress")?)?;
        params.get("total").map(nonnegative).transpose()?;
        params.get("message").map(string).transpose()?;
        // Unsolicited/late tokens are exposed but never complete or change a call.
        if let Ok(id) = token.integer()
            && let Ok(slot) = self.find(id)
            && self.slots[slot].progress_requested
        {
            if self.slots[slot].progress.is_some_and(|old| value <= old) {
                return Err(invalid(params));
            }
            self.slots[slot].progress = Some(value);
        }
        Ok(())
    }
}

fn state() -> Error {
    Error::new(ErrorKind::State, 0)
}
fn invalid(value: Value<'_>) -> Error {
    Error::new(ErrorKind::InvalidParams, value.offset())
}
fn object(value: Value<'_>) -> Result<Value<'_>, Error> {
    if value.kind() == Kind::Object {
        Ok(value)
    } else {
        Err(invalid(value))
    }
}
fn array(value: Value<'_>) -> Result<Value<'_>, Error> {
    if value.kind() == Kind::Array {
        Ok(value)
    } else {
        Err(invalid(value))
    }
}
fn string(value: Value<'_>) -> Result<JsonString<'_>, Error> {
    value.string().ok_or_else(|| invalid(value))
}
fn numeric_id(id: Id<'_>) -> Result<i64, Error> {
    if let Id::Number(n) = id {
        Ok(n)
    } else {
        Err(Error::new(ErrorKind::UnexpectedResponse, 0))
    }
}
fn capabilities(value: Value<'_>) -> Result<Value<'_>, Error> {
    object(value)?;
    for name in [
        "tools",
        "prompts",
        "resources",
        "logging",
        "completions",
        "experimental",
        "extensions",
        "tasks",
    ] {
        value.get(name).map(object).transpose()?;
    }
    Ok(value)
}
fn validate_meta(result: Value<'_>) -> Result<Option<ServerInfo<'_>>, Error> {
    if let Some(meta) = result.get("_meta") {
        object(meta)?;
        meta.get("io.modelcontextprotocol/serverInfo")
            .map(ServerInfo::parse)
            .transpose()
    } else {
        Ok(None)
    }
}
fn complete_result(result: Value<'_>) -> Result<bool, Error> {
    match result.get("resultType") {
        None => Ok(true),
        Some(v) => {
            let kind = string(v)?;
            if kind.matches("complete") {
                Ok(true)
            } else if kind.matches("input_required") {
                Ok(false)
            } else {
                Err(Error::new(ErrorKind::UnsupportedResult, v.offset()))
            }
        }
    }
}
fn validate_input_required(result: Value<'_>) -> Result<(), Error> {
    let requests = result.get("inputRequests");
    let continuation = result.get("requestState");
    if requests.is_none() && continuation.is_none() {
        return Err(invalid(result));
    }
    requests.map(object).transpose()?;
    continuation.map(string).transpose()?;
    Ok(())
}
fn nonnegative(value: Value<'_>) -> Result<f64, Error> {
    if value.kind() != Kind::Number {
        return Err(invalid(value));
    }
    let n = value.raw().parse::<f64>().map_err(|_| invalid(value))?;
    if n.is_finite() && n >= 0.0 {
        Ok(n)
    } else {
        Err(invalid(value))
    }
}
