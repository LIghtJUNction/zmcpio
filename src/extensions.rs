//! Opt-in MCP methods. These are wire primitives, never tool execution,
//! filesystem access, sampling, user interaction, task scheduling or authorization.
//! JSON Schema and application-specific result validation remain with the host.
use crate::{
    Error, ErrorKind,
    json::{JsonString, Kind, Value},
    mcp::Era,
    rpc::{self, Id, Notification},
    writer::Writer,
};

/// Standard method identities supported by the generic request/dispatch boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    ToolsCall,
    ResourcesList,
    ResourcesTemplatesList,
    ResourcesRead,
    ResourcesSubscribe,
    ResourcesUnsubscribe,
    PromptsList,
    PromptsGet,
    CompletionComplete,
    LoggingSetLevel,
    SubscriptionsListen,
    TasksGet,
    TasksResult,
    TasksList,
    TasksCancel,
    RootsList,
    SamplingCreateMessage,
    ElicitationCreate,
}

impl Method {
    pub const ALL: [Self; 18] = [
        Self::ToolsCall,
        Self::ResourcesList,
        Self::ResourcesTemplatesList,
        Self::ResourcesRead,
        Self::ResourcesSubscribe,
        Self::ResourcesUnsubscribe,
        Self::PromptsList,
        Self::PromptsGet,
        Self::CompletionComplete,
        Self::LoggingSetLevel,
        Self::SubscriptionsListen,
        Self::TasksGet,
        Self::TasksResult,
        Self::TasksList,
        Self::TasksCancel,
        Self::RootsList,
        Self::SamplingCreateMessage,
        Self::ElicitationCreate,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolsCall => "tools/call",
            Self::ResourcesList => "resources/list",
            Self::ResourcesTemplatesList => "resources/templates/list",
            Self::ResourcesRead => "resources/read",
            Self::ResourcesSubscribe => "resources/subscribe",
            Self::ResourcesUnsubscribe => "resources/unsubscribe",
            Self::PromptsList => "prompts/list",
            Self::PromptsGet => "prompts/get",
            Self::CompletionComplete => "completion/complete",
            Self::LoggingSetLevel => "logging/setLevel",
            Self::SubscriptionsListen => "subscriptions/listen",
            Self::TasksGet => "tasks/get",
            Self::TasksResult => "tasks/result",
            Self::TasksList => "tasks/list",
            Self::TasksCancel => "tasks/cancel",
            Self::RootsList => "roots/list",
            Self::SamplingCreateMessage => "sampling/createMessage",
            Self::ElicitationCreate => "elicitation/create",
        }
    }

    pub fn parse(name: JsonString<'_>) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| name.matches(method.as_str()))
    }

    pub const fn is_callback(self) -> bool {
        matches!(
            self,
            Self::RootsList | Self::SamplingCreateMessage | Self::ElicitationCreate
        )
    }

    pub const fn supports_input(self) -> bool {
        matches!(
            self,
            Self::ToolsCall | Self::ResourcesRead | Self::PromptsGet
        )
    }

    pub const fn available(self, era: Era) -> bool {
        match self {
            Self::ResourcesSubscribe
            | Self::ResourcesUnsubscribe
            | Self::LoggingSetLevel
            | Self::RootsList
            | Self::SamplingCreateMessage
            | Self::ElicitationCreate => {
                matches!(era, Era::Legacy)
            }
            Self::SubscriptionsListen => matches!(era, Era::Modern),
            _ => true,
        }
    }

    /// Tasks were introduced in the 2025-11-25 legacy profile. Do not advertise
    /// or dispatch them after negotiating an older tools-compatible revision.
    pub fn available_at(self, era: Era, version: Option<&str>) -> bool {
        self.available(era)
            && (!matches!(
                self,
                Self::TasksGet | Self::TasksResult | Self::TasksList | Self::TasksCancel
            ) || era == Era::Modern
                || version == Some(crate::mcp::LEGACY_PROTOCOL_VERSION))
    }

    pub const fn callback_capability(self) -> Option<&'static str> {
        match self {
            Self::RootsList => Some("roots"),
            Self::SamplingCreateMessage => Some("sampling"),
            Self::ElicitationCreate => Some("elicitation"),
            _ => None,
        }
    }

    /// Validate required wire fields, not application schemas or permissions.
    pub fn validate_params(self, params: Option<Value<'_>>) -> Result<(), Error> {
        if let Some(value) = params {
            object(value)?;
            value.get("_meta").map(object).transpose()?;
            value.get("requestState").map(string).transpose()?;
            value.get("inputResponses").map(object).transpose()?;
            if !self.supports_input()
                && (value.get("requestState").is_some() || value.get("inputResponses").is_some())
            {
                return Err(invalid(value));
            }
        }
        match self {
            Self::ResourcesList
            | Self::ResourcesTemplatesList
            | Self::PromptsList
            | Self::TasksList => {
                params
                    .and_then(|v| v.get("cursor"))
                    .map(string)
                    .transpose()?;
            }
            Self::ResourcesRead | Self::ResourcesSubscribe | Self::ResourcesUnsubscribe => {
                string(required(params, "uri")?)?;
            }
            Self::PromptsGet => {
                string(required(params, "name")?)?;
                if let Some(args) = params.and_then(|v| v.get("arguments")) {
                    for (_, value) in object(args)?.members() {
                        string(value)?;
                    }
                }
            }
            Self::ToolsCall => {
                let name = string(required(params, "name")?)?;
                if name.decoded_len() == 0 || name.decoded_len() > 128 {
                    return Err(Error::new(ErrorKind::InvalidParams, 0));
                }
                params
                    .and_then(|v| v.get("arguments"))
                    .map(object)
                    .transpose()?;
            }
            Self::CompletionComplete => {
                let reference = object(required(params, "ref")?)?;
                let kind = string(reference.require("type")?)?;
                if kind.matches("ref/prompt") {
                    string(reference.require("name")?)?;
                } else if kind.matches("ref/resource") {
                    string(reference.require("uri")?)?;
                } else {
                    return Err(invalid(reference));
                }
                let argument = object(required(params, "argument")?)?;
                string(argument.require("name")?)?;
                string(argument.require("value")?)?;
                params
                    .and_then(|v| v.get("context"))
                    .map(object)
                    .transpose()?;
            }
            Self::LoggingSetLevel => {
                let level = string(required(params, "level")?)?;
                if ![
                    "debug",
                    "info",
                    "notice",
                    "warning",
                    "error",
                    "critical",
                    "alert",
                    "emergency",
                ]
                .iter()
                .any(|candidate| level.matches(candidate))
                {
                    return Err(Error::new(ErrorKind::InvalidParams, 0));
                }
            }
            Self::SubscriptionsListen => {
                validate_filter(required(params, "notifications")?)?;
            }
            Self::TasksGet | Self::TasksResult | Self::TasksCancel => {
                string(required(params, "taskId")?)?;
            }
            Self::RootsList => {}
            Self::SamplingCreateMessage => {
                let messages = array(required(params, "messages")?)?;
                for message in messages.elements() {
                    object(message)?;
                    let role = string(message.require("role")?)?;
                    if !role.matches("user") && !role.matches("assistant") {
                        return Err(invalid(message));
                    }
                    let content = message.require("content")?;
                    if !matches!(content.kind(), Kind::Object | Kind::Array) {
                        return Err(invalid(content));
                    }
                }
                if required(params, "maxTokens")?.integer()? <= 0 {
                    return Err(Error::new(ErrorKind::InvalidParams, 0));
                }
            }
            Self::ElicitationCreate => {
                string(required(params, "message")?)?;
                let mode = params.and_then(|v| v.get("mode")).map(string).transpose()?;
                if mode.is_some_and(|value| value.matches("url")) {
                    string(required(params, "url")?)?;
                    string(required(params, "elicitationId")?)?;
                } else if mode.is_none_or(|value| value.matches("form")) {
                    object(required(params, "requestedSchema")?)?;
                } else {
                    return Err(Error::new(ErrorKind::InvalidParams, 0));
                }
            }
        }
        Ok(())
    }

    /// Check required result containers while preserving their complete borrowed JSON.
    pub fn validate_result(self, result: Value<'_>) -> Result<(), Error> {
        object(result)?;
        match self {
            Self::ResourcesList => {
                let values = array(result.require("resources")?)?;
                for value in values.elements() {
                    object(value)?;
                    string(value.require("uri")?)?;
                    string(value.require("name")?)?;
                }
            }
            Self::ResourcesTemplatesList => {
                let values = array(result.require("resourceTemplates")?)?;
                for value in values.elements() {
                    object(value)?;
                    string(value.require("uriTemplate")?)?;
                    string(value.require("name")?)?;
                }
            }
            Self::ResourcesRead => {
                for value in array(result.require("contents")?)?.elements() {
                    object(value)?;
                    string(value.require("uri")?)?;
                    match (value.get("text"), value.get("blob")) {
                        (Some(content), None) | (None, Some(content)) => {
                            string(content)?;
                        }
                        _ => return Err(invalid(value)),
                    }
                }
            }
            Self::PromptsList => {
                for value in array(result.require("prompts")?)?.elements() {
                    object(value)?;
                    string(value.require("name")?)?;
                }
            }
            Self::PromptsGet => {
                for value in array(result.require("messages")?)?.elements() {
                    object(value)?;
                    let role = string(value.require("role")?)?;
                    if !role.matches("user") && !role.matches("assistant") {
                        return Err(invalid(value));
                    }
                    object(value.require("content")?)?;
                }
            }
            Self::CompletionComplete => {
                let completion = object(result.require("completion")?)?;
                for value in array(completion.require("values")?)?.elements() {
                    string(value)?;
                }
                if let Some(total) = completion.get("total")
                    && total.integer()? < 0
                {
                    return Err(invalid(total));
                }
                if let Some(more) = completion.get("hasMore")
                    && more.boolean().is_none()
                {
                    return Err(invalid(more));
                }
            }
            Self::TasksList => {
                array(result.require("tasks")?)?;
            }
            Self::RootsList => {
                for root in array(result.require("roots")?)?.elements() {
                    object(root)?;
                    string(root.require("uri")?)?;
                }
            }
            _ => {}
        }
        result.get("nextCursor").map(string).transpose()?;
        result.get("_meta").map(object).transpose()?;
        Ok(())
    }
}

/// Validate opt-in client capabilities. No capability is enabled by default.
pub fn validate_client_capabilities(value: Value<'_>) -> Result<(), Error> {
    object(value)?;
    for field in [
        "roots",
        "sampling",
        "elicitation",
        "experimental",
        "extensions",
        "tasks",
    ] {
        value.get(field).map(object).transpose()?;
    }
    Ok(())
}

/// Explicitly encode a legacy server-to-client request after the host authorizes it.
/// Correlation, timeouts and execution of the result are the host's responsibility.
pub fn callback_request(
    id: Id<'_>,
    method: Method,
    params: Option<Value<'_>>,
    client_capabilities: Value<'_>,
    output: &mut [u8],
) -> Result<usize, Error> {
    validate_callback(method, params, client_capabilities)?;
    rpc::request(id, method.as_str(), params, output)
}

/// Validate modern MRTR input requests against the capabilities of this request.
pub fn validate_input_requests(
    requests: Value<'_>,
    client_capabilities: Value<'_>,
) -> Result<(), Error> {
    object(requests)?;
    validate_client_capabilities(client_capabilities)?;
    for (_, request) in requests.members() {
        object(request)?;
        if request.get("id").is_some() || request.get("jsonrpc").is_some() {
            return Err(invalid(request));
        }
        let method = Method::parse(string(request.require("method")?)?)
            .filter(|method| method.is_callback())
            .ok_or_else(|| invalid(request))?;
        validate_callback(method, request.get("params"), client_capabilities)?;
    }
    Ok(())
}

fn validate_callback(
    method: Method,
    params: Option<Value<'_>>,
    capabilities: Value<'_>,
) -> Result<(), Error> {
    let name = method
        .callback_capability()
        .ok_or(Error::new(ErrorKind::State, 0))?;
    validate_client_capabilities(capabilities)?;
    let capability = capabilities
        .get(name)
        .ok_or(Error::new(ErrorKind::State, 0))?;
    method.validate_params(params)?;
    if method == Method::ElicitationCreate
        && params
            .and_then(|value| value.get("mode"))
            .and_then(Value::string)
            .is_some_and(|value| value.matches("url"))
        && capability.get("url").is_none()
    {
        return Err(Error::new(ErrorKind::State, capability.offset()));
    }
    if method == Method::SamplingCreateMessage {
        if params.and_then(|value| value.get("tools")).is_some()
            && capability.get("tools").is_none()
        {
            return Err(Error::new(ErrorKind::State, capability.offset()));
        }
        if params
            .and_then(|value| value.get("includeContext"))
            .and_then(Value::string)
            .is_some_and(|value| !value.matches("none"))
            && capability.get("context").is_none()
        {
            return Err(Error::new(ErrorKind::State, capability.offset()));
        }
    }
    Ok(())
}

pub(crate) fn write_key(w: &mut Writer<'_>, key: JsonString<'_>) {
    Id::String(key).write(w);
    w.raw(b":");
}

pub(crate) fn object(value: Value<'_>) -> Result<Value<'_>, Error> {
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
fn required<'a>(params: Option<Value<'a>>, field: &str) -> Result<Value<'a>, Error> {
    params
        .ok_or(Error::new(ErrorKind::InvalidParams, 0))?
        .require(field)
}
fn invalid(value: Value<'_>) -> Error {
    Error::new(ErrorKind::InvalidParams, value.offset())
}

pub fn validate_filter(filter: Value<'_>) -> Result<(), Error> {
    object(filter)?;
    for name in [
        "toolsListChanged",
        "promptsListChanged",
        "resourcesListChanged",
    ] {
        if let Some(value) = filter.get(name)
            && value.boolean().is_none()
        {
            return Err(invalid(value));
        }
    }
    if let Some(uris) = filter.get("resourceSubscriptions") {
        for uri in array(uris)?.elements() {
            string(uri)?;
        }
    }
    Ok(())
}

fn filter_subset(subset: Value<'_>, superset: Value<'_>) -> bool {
    for name in [
        "toolsListChanged",
        "promptsListChanged",
        "resourcesListChanged",
    ] {
        if subset.get(name).and_then(Value::boolean) == Some(true)
            && superset.get(name).and_then(Value::boolean) != Some(true)
        {
            return false;
        }
    }
    if let Some(uris) = subset.get("resourceSubscriptions") {
        for uri in uris.elements() {
            let Some(uri) = uri.string() else {
                return false;
            };
            if !superset.get("resourceSubscriptions").is_some_and(|values| {
                values
                    .elements()
                    .any(|value| value.string().is_some_and(|other| uri.equals(other)))
            }) {
                return false;
            }
        }
    }
    true
}

/// Caller-retained subscription filters and acknowledgment; no allocation or I/O.
/// Keep the request filter and acknowledgment input/token storage alive while used.
pub struct Subscription<'a> {
    id: Id<'a>,
    requested: Value<'a>,
    accepted: Option<Value<'a>>,
}
impl<'a> Subscription<'a> {
    pub fn new(id: Id<'a>, requested: Value<'a>) -> Result<Self, Error> {
        validate_filter(requested)?;
        Ok(Self {
            id,
            requested,
            accepted: None,
        })
    }

    pub fn acknowledged(&self) -> bool {
        self.accepted.is_some()
    }

    /// Check ordering, correlation and opt-in filters before delivering a notification.
    pub fn observe(&mut self, notification: Notification<'a>) -> Result<(), Error> {
        let params = object(
            notification
                .params
                .ok_or(Error::new(ErrorKind::InvalidParams, 0))?,
        )?;
        let meta = object(params.require("_meta")?)?;
        let id = Id::from_value(meta.require("io.modelcontextprotocol/subscriptionId")?)?;
        if !id.equals(self.id) {
            return Err(Error::new(ErrorKind::UnknownId, params.offset()));
        }
        if notification
            .method
            .matches("notifications/subscriptions/acknowledged")
        {
            if self.accepted.is_some() {
                return Err(Error::new(ErrorKind::State, params.offset()));
            }
            let accepted = params.require("notifications")?;
            validate_filter(accepted)?;
            if !filter_subset(accepted, self.requested) {
                return Err(invalid(accepted));
            }
            self.accepted = Some(accepted);
            return Ok(());
        }
        let accepted = self
            .accepted
            .ok_or(Error::new(ErrorKind::State, params.offset()))?;
        let fields = [
            ("notifications/tools/list_changed", "toolsListChanged"),
            ("notifications/prompts/list_changed", "promptsListChanged"),
            (
                "notifications/resources/list_changed",
                "resourcesListChanged",
            ),
        ];
        for (method, field) in fields {
            if notification.method.matches(method) {
                return if accepted.get(field).and_then(Value::boolean) == Some(true) {
                    Ok(())
                } else {
                    Err(invalid(params))
                };
            }
        }
        if notification
            .method
            .matches("notifications/resources/updated")
        {
            let uri = string(params.require("uri")?)?;
            if accepted.get("resourceSubscriptions").is_some_and(|values| {
                values
                    .elements()
                    .any(|value| value.string().is_some_and(|other| uri.equals(other)))
            }) {
                return Ok(());
            }
        }
        Err(invalid(params))
    }
}
