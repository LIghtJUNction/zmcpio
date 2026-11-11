//! Allocation-free adapters between MCP tools and the `zcall::v1` contract.
//!
//! A model invocation ID and a JSON-RPC request ID are different identities.
//! [`Invocation`] retains both and checks the MCP response ID before producing
//! a model tool result. The caller still authorizes tools and owns transport.
//!
//! Tool results retain the complete MCP result object as JSON. This preserves
//! structured content, text, image/audio payloads and extension metadata; it does
//! not silently flatten multimodal content into text or select one content item.
//! Provider-native multimodal tool-result rendering is a separate caller policy.
//! The default portable representation retains `isError` inside that JSON
//! envelope; it does not add a provider-native error flag unsupported by some
//! zcall targets. [`ResultEncoding::NativeErrorFlag`] is an explicit opt-in.

use crate::{
    Error, ErrorKind,
    client::{CallResult, Client, Outgoing, ToolView},
    json::{Document, Kind, Limits, Token, Value},
};
use zcall::v1 as model;

/// The source of a bridge failure stays explicit, including capacity details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeError {
    Mcp(Error),
    Model(model::Error),
}
impl From<Error> for BridgeError {
    fn from(value: Error) -> Self {
        Self::Mcp(value)
    }
}
impl From<model::Error> for BridgeError {
    fn from(value: model::Error) -> Self {
        Self::Model(value)
    }
}
impl core::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Mcp(error) => write!(f, "MCP bridge: {error}"),
            Self::Model(error) => write!(f, "zcall bridge: {error}"),
        }
    }
}
impl core::error::Error for BridgeError {}

/// Where to expose MCP's tool-execution error flag to the model protocol.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResultEncoding {
    /// Keep the whole MCP object, including its `isError` field, in the result
    /// JSON. Do not add a separate native flag. Omitted `isError` means success
    /// under MCP. This representation works across zcall's tool-capable targets.
    #[default]
    Portable,
    /// Additionally populate zcall's native `is_error` field. Choose this only
    /// for a target that supports it, such as Anthropic Messages. Other zcall
    /// targets may reject it rather than silently discard the flag.
    NativeErrorFlag,
}
impl ResultEncoding {
    fn flag(self, result: CallResult<'_>) -> model::Presence<bool> {
        match self {
            Self::Portable => model::Presence::Missing,
            Self::NativeErrorFlag => model::Presence::Value(result.is_error()),
        }
    }
}

/// Convert one discovered MCP input signature into a model tool definition.
///
/// Strings and schema JSON borrow the discovery response. `tokens` is reusable
/// immediately after return. Keep the response input and its MCP token tape
/// alive while the returned definition is retained by a zcall request.
///
/// `strict=false` is explicit: MCP JSON Schema does not imply provider-specific
/// strict-mode requirements. Output schemas and annotations stay available on
/// `ToolView`; they are not input parameters or an authorization decision.
pub fn tool_definition<'a, const N: usize>(
    tool: ToolView<'a>,
    tokens: &mut model::TokenStorage<N>,
    limits: model::Limits,
) -> Result<model::ToolDefinition<'a>, BridgeError> {
    let raw = tool.raw();
    let name = model_text(raw.require("name")?, tokens, limits)?;
    if name.decoded_len() == 0 {
        return Err(model::Error::invalid("tool name", "empty MCP tool name").into());
    }
    let description = raw
        .get("description")
        .map(|value| model_text(value, tokens, limits))
        .transpose()?
        .map_or(model::Presence::Missing, model::Presence::Value);
    let schema = raw.require("inputSchema")?;
    if schema.kind() != Kind::Object {
        return Err(Error::new(ErrorKind::InvalidParams, schema.offset()).into());
    }
    let parameters = model::Json::parse(schema.raw().as_bytes(), tokens, limits)?;
    Ok(model::ToolDefinition {
        name,
        description,
        parameters: model::Presence::Value(parameters),
        strict: model::Presence::Value(false),
        legacy: false,
    })
}

fn model_text<'a, const N: usize>(
    value: Value<'a>,
    tokens: &mut model::TokenStorage<N>,
    limits: model::Limits,
) -> Result<model::Text<'a>, BridgeError> {
    model::Json::parse(value.raw().as_bytes(), tokens, limits)?
        .as_text()
        .ok_or_else(|| Error::new(ErrorKind::Type, value.offset()).into())
}

/// A validated model tool call ready for MCP encoding, not execution.
///
/// Identity storage and argument storage have separate lifetimes. Once `start`
/// has serialized the call, the argument bytes/token tape may be reused while
/// the returned `Invocation` keeps the model call ID and tool name alive.
#[derive(Clone, Copy, Debug)]
pub struct PreparedCall<'model, 'args> {
    model_call_id: &'model str,
    name: &'model str,
    arguments: Value<'args>,
}
impl<'model> PreparedCall<'model, '_> {
    pub fn model_call_id(&self) -> &'model str {
        self.model_call_id
    }
    pub fn name(&self) -> &'model str {
        self.name
    }
    pub fn arguments(&self) -> Value<'_> {
        self.arguments
    }
    /// Serialize only. The caller must authorize before sending these bytes.
    /// A capacity error queues no MCP request; grow output and retry encoding.
    pub fn start(
        self,
        client: &mut Client<'_>,
        output: &mut [u8],
    ) -> Result<Invocation<'model>, BridgeError> {
        let outgoing = client.call_tool(self.name, Some(self.arguments), output)?;
        Ok(Invocation {
            outgoing,
            model_call_id: self.model_call_id,
            name: self.name,
        })
    }
}

/// Prepare a complete zcall tool-call content block for MCP.
///
/// Unescaped IDs/names and JSON object arguments remain borrowed. Only escaped
/// identity strings use `identity`; only escaped `Data::JsonText` uses
/// `argument_bytes`. All arguments are checked as a complete JSON object, with
/// the caller's MCP byte/depth/member bounds. `Data::Text` is rejected rather
/// than guessed to be JSON. Missing/null/empty model IDs are not fabricated.
pub fn prepare_call<'source, 'identity, 'args>(
    content: model::Content<'source>,
    identity: &'identity mut [u8],
    argument_bytes: &'args mut [u8],
    tokens: &'args mut [Token],
    limits: Limits,
) -> Result<PreparedCall<'identity, 'args>, BridgeError>
where
    'source: 'identity + 'args,
{
    let model::Content::ToolCall {
        id: model::Presence::Value(id),
        name,
        arguments,
        ..
    } = content
    else {
        return Err(model::Error::invalid("tool call", "expected a tool call with an ID").into());
    };
    if id.decoded_len() == 0 || name.decoded_len() == 0 {
        return Err(model::Error::invalid("tool call", "empty call ID or tool name").into());
    }
    let id_bytes = if id.as_str().is_some() {
        0
    } else {
        id.decoded_len()
    };
    let name_bytes = if name.as_str().is_some() {
        0
    } else {
        name.decoded_len()
    };
    let needed = id_bytes.saturating_add(name_bytes);
    if identity.len() < needed {
        return Err(Error::capacity(ErrorKind::Buffer, 0, needed).into());
    }
    let (id_storage, name_storage) = identity.split_at_mut(id_bytes);
    let model_call_id = decoded(id, id_storage)?;
    let name = decoded(name, name_storage)?;
    let bytes = match arguments {
        model::Data::Json(value) => value.raw(),
        model::Data::JsonText(value) => decoded(value, argument_bytes)?.as_bytes(),
        model::Data::EmptyObject => b"{}",
        model::Data::Text(_) => {
            return Err(
                model::Error::invalid("tool arguments", "prose is not JSON arguments").into(),
            );
        }
    };
    let arguments = object_arguments(bytes, tokens, limits)?;
    Ok(PreparedCall {
        model_call_id,
        name,
        arguments,
    })
}

/// Prepare an already-assembled streaming `ToolReady` event. Partial tool deltas
/// cannot enter this adapter. The returned invocation retains `call_id`, never
/// the separate provider output `item_id`.
///
/// This zero-copy variant keeps the zcall Session borrowed through the returned
/// identity. Use [`prepare_ready_detached`] when the caller must mutate the
/// Session while the invocation is retained.
pub fn prepare_ready<'model: 'args, 'args>(
    tool: model::ReadyTool<'model>,
    tokens: &'args mut [Token],
    limits: Limits,
) -> Result<PreparedCall<'model, 'args>, BridgeError> {
    if tool.call_id.is_empty() || tool.name.is_empty() {
        return Err(model::Error::invalid("tool call", "empty call ID or tool name").into());
    }
    Ok(PreparedCall {
        model_call_id: tool.call_id,
        name: tool.name,
        arguments: object_arguments(tool.arguments.as_bytes(), tokens, limits)?,
    })
}

/// Copy a streaming call's ID/name into caller-owned identity storage. Argument
/// bytes remain borrowed until `PreparedCall::start` has encoded the request.
/// After that, the returned Invocation no longer borrows the zcall Session, so
/// `Session::submit_tool_result(invocation.model_call_id())` is possible without
/// allocating a String or inventing a second call ID.
pub fn prepare_ready_detached<'model: 'args, 'identity, 'args>(
    tool: model::ReadyTool<'model>,
    identity: &'identity mut [u8],
    tokens: &'args mut [Token],
    limits: Limits,
) -> Result<PreparedCall<'identity, 'args>, BridgeError> {
    if tool.call_id.is_empty() || tool.name.is_empty() {
        return Err(model::Error::invalid("tool call", "empty call ID or tool name").into());
    }
    let needed = tool.call_id.len().saturating_add(tool.name.len());
    if identity.len() < needed {
        return Err(Error::capacity(ErrorKind::Buffer, 0, needed).into());
    }
    let (id_bytes, name_bytes) = identity[..needed].split_at_mut(tool.call_id.len());
    id_bytes.copy_from_slice(tool.call_id.as_bytes());
    name_bytes.copy_from_slice(tool.name.as_bytes());
    let model_call_id =
        core::str::from_utf8(id_bytes).map_err(|_| Error::new(ErrorKind::Utf8, 0))?;
    let name = core::str::from_utf8(name_bytes).map_err(|_| Error::new(ErrorKind::Utf8, 0))?;
    Ok(PreparedCall {
        model_call_id,
        name,
        arguments: object_arguments(tool.arguments.as_bytes(), tokens, limits)?,
    })
}

fn decoded<'source: 'storage, 'storage>(
    value: model::Text<'source>,
    storage: &'storage mut [u8],
) -> Result<&'storage str, BridgeError> {
    if let Some(text) = value.as_str() {
        return Ok(text);
    }
    let len = value.decode_into(storage)?;
    core::str::from_utf8(&storage[..len]).map_err(|_| Error::new(ErrorKind::Utf8, 0).into())
}

fn object_arguments<'a>(
    input: &'a [u8],
    tokens: &'a mut [Token],
    limits: Limits,
) -> Result<Value<'a>, BridgeError> {
    let value = Document::parse(input, tokens, limits)?.root();
    if value.kind() != Kind::Object {
        return Err(Error::new(ErrorKind::InvalidParams, value.offset()).into());
    }
    Ok(value)
}

/// The binding between one model invocation and one MCP JSON-RPC request.
///
/// Receiving an MCP result does not itself submit a result to a zcall Session.
/// After retaining/appending the actual result, the caller may explicitly call
/// `Session::submit_tool_result(invocation.model_call_id())` when its lifecycle
/// and identity-storage lifetime permit it (see `prepare_ready_detached`).
/// Transport failures and RPC failures are not tool execution errors.
#[derive(Clone, Copy, Debug)]
pub struct Invocation<'a> {
    outgoing: Outgoing,
    model_call_id: &'a str,
    name: &'a str,
}
impl<'identity> Invocation<'identity> {
    /// Request metadata is returned by value so the retained response binding
    /// cannot be accidentally changed by a caller preparing transport headers.
    pub fn outgoing(&self) -> Outgoing {
        self.outgoing
    }
    pub fn model_call_id(&self) -> &'identity str {
        self.model_call_id
    }
    pub fn name(&self) -> &'identity str {
        self.name
    }
    fn correlate(&self, response_id: i64) -> Result<(), BridgeError> {
        if response_id != self.outgoing.id {
            return Err(Error::new(ErrorKind::UnknownId, 0).into());
        }
        Ok(())
    }
    /// Produce a model result preserving every MCP result field. Handle
    /// `Event::Failure` and `Event::InputRequired` separately; neither is accepted
    /// as a successfully completed tool result by this method.
    pub fn content<'response, 'out, const N: usize>(
        &self,
        response_id: i64,
        result: CallResult<'response>,
        tokens: &mut model::TokenStorage<N>,
        limits: model::Limits,
    ) -> Result<model::Content<'out>, BridgeError>
    where
        'identity: 'out,
        'response: 'out,
    {
        self.content_with_encoding(
            response_id,
            result,
            ResultEncoding::Portable,
            tokens,
            limits,
        )
    }
    /// Like `content`, with an explicit native-error-flag policy. The original
    /// MCP JSON envelope is retained in both modes, including `isError`.
    pub fn content_with_encoding<'response, 'out, const N: usize>(
        &self,
        response_id: i64,
        result: CallResult<'response>,
        encoding: ResultEncoding,
        tokens: &mut model::TokenStorage<N>,
        limits: model::Limits,
    ) -> Result<model::Content<'out>, BridgeError>
    where
        'identity: 'out,
        'response: 'out,
    {
        self.correlate(response_id)?;
        let data = result_data(result, tokens, limits)?;
        Ok(model::Content::ToolResult {
            id: model::Presence::Value(model::Text::new(self.model_call_id)),
            name: model::Presence::Value(model::Text::new(self.name)),
            result: data,
            is_error: encoding.flag(result),
        })
    }
    /// Append through zcall's checked history API. Unknown model call IDs,
    /// ambiguous calls and duplicate results leave the request unchanged.
    pub fn append_result<'response, 'out, const N: usize>(
        &self,
        request: &mut model::Request<'out, '_>,
        response_id: i64,
        result: CallResult<'response>,
        tokens: &mut model::TokenStorage<N>,
        limits: model::Limits,
    ) -> Result<(), BridgeError>
    where
        'identity: 'out,
        'response: 'out,
    {
        self.append_result_with_encoding(
            request,
            response_id,
            result,
            ResultEncoding::Portable,
            tokens,
            limits,
        )
    }
    /// Checked append with an explicit native-error-flag policy. Selecting
    /// `NativeErrorFlag` may make later encoding to an incompatible model
    /// protocol fail; the portable envelope never omits the original flag.
    pub fn append_result_with_encoding<'response, 'out, const N: usize>(
        &self,
        request: &mut model::Request<'out, '_>,
        response_id: i64,
        result: CallResult<'response>,
        encoding: ResultEncoding,
        tokens: &mut model::TokenStorage<N>,
        limits: model::Limits,
    ) -> Result<(), BridgeError>
    where
        'identity: 'out,
        'response: 'out,
    {
        self.correlate(response_id)?;
        let data = result_data(result, tokens, limits)?;
        request.append_tool_result(self.model_call_id, data, encoding.flag(result))?;
        Ok(())
    }
}

/// Retain the whole MCP result envelope as a zcall JSON value, without lossy
/// text concatenation, image decoding or provider-specific transformations.
pub fn result_data<'a, const N: usize>(
    result: CallResult<'a>,
    tokens: &mut model::TokenStorage<N>,
    limits: model::Limits,
) -> Result<model::Data<'a>, BridgeError> {
    Ok(model::Data::Json(model::Json::parse(
        result.raw().raw().as_bytes(),
        tokens,
        limits,
    )?))
}
