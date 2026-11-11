//! A complete model-call -> MCP tool -> model-result round trip in memory.
//! Use `mcp_client` for the separate example that connects a real stdio process.
//! No model HTTP request is made here; `Content::ToolCall` stands for a completed
//! model response, or use `bridge::prepare_ready` with a streaming ToolReady.
use zcall::v1 as model;
use zmcpio::{
    bridge::{self, BridgeError},
    client::{Client, Event, Pending},
    json::{Document, Limits, Token},
    mcp::{Action, Era, Server},
    tools::{Content, Tool, ToolResult},
};

fn main() -> Result<(), BridgeError> {
    let mut pending = [Pending::EMPTY; 4];
    let mut client = Client::new("zcall-example", "1", Era::Modern, &mut pending);
    let mut server = Server::new("explicit-echo", "1");
    let mut client_wire = [0; 2048];
    let mut server_tokens = [Token::EMPTY; 128];
    let mut schema_tokens = [Token::EMPTY; 32];
    let schema = Document::parse(
        br#"{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}"#,
        &mut schema_tokens,
        Limits::default(),
    )?;
    let echo = Tool::new("echo", "Echo the supplied message", schema.root())?;

    // Discovery has retained storage because the zcall definition borrows it.
    let mut discovery_wire = [0; 4096];
    let mut discovery_tokens = [Token::EMPTY; 128];
    let outgoing = client.list_tools(None, &mut client_wire)?;
    let Action::ListTools { context, .. } = server.handle(
        &client_wire[..outgoing.len],
        &mut server_tokens,
        Limits::default(),
        &mut discovery_wire,
    )?
    else {
        panic!("expected discovery action")
    };
    let len = server.list_tools(context, &[echo], None, &mut discovery_wire)?;
    let Event::Tools { page, .. } = client.receive(
        &discovery_wire[..len],
        &mut discovery_tokens,
        Limits::default(),
    )?
    else {
        panic!("expected tool definitions")
    };
    let mut model_tokens = model::TokenStorage::<256>::new();
    let definition = bridge::tool_definition(
        page.tools().next().expect("one tool"),
        &mut model_tokens,
        model::Limits::default(),
    )?;
    let arguments = model::Json::parse(
        br#"{"message":"hello from zcall"}"#,
        &mut model_tokens,
        model::Limits::default(),
    )?;
    let model_call = model::Content::ToolCall {
        id: model::Presence::Value(model::Text::new("provider-call-7")),
        name: model::Text::new("echo"),
        arguments: model::Data::Json(arguments),
        legacy: false,
    };
    let mut model_records = model::RecordStorage::<32>::new();
    let mut builder = model::RequestBuilder::new(
        model::Protocol::OpenAiChat.into(),
        model::Presence::Value(model::Text::new("caller-selected-model")),
        &mut model_records,
    )?;
    builder.tool(definition)?;
    builder.message(model::Role::User, &[model::Content::text("Echo hello")])?;
    builder.message(model::Role::Assistant, &[model_call])?;
    let mut model_request = builder.finish();

    let mut identities = [0; 128];
    let mut argument_bytes = [0; 512];
    let mut call_tokens = [Token::EMPTY; 32];
    let prepared = bridge::prepare_call(
        model_call,
        &mut identities,
        &mut argument_bytes,
        &mut call_tokens,
        Limits::default(),
    )?;
    // The application authorizes this named tool before any bytes are sent.
    assert_eq!(prepared.name(), "echo");
    let invocation = prepared.start(&mut client, &mut client_wire)?;
    let mut result_wire = [0; 4096];
    let Action::CallTool(call) = server.handle(
        &client_wire[..invocation.outgoing().len],
        &mut server_tokens,
        Limits::default(),
        &mut result_wire,
    )?
    else {
        panic!("expected tool call")
    };
    assert!(call.name.matches("echo"));
    let content = [Content::Text(
        core::str::from_utf8(call.arguments_bytes()).expect("validated JSON"),
    )];
    let mut result = ToolResult::new(&content);
    result.structured_content = call.arguments;
    let len = server.tool_result(call.context, result, &mut result_wire)?;
    let mut result_tokens = [Token::EMPTY; 128];
    let Event::ToolResult { id, result } =
        client.receive(&result_wire[..len], &mut result_tokens, Limits::default())?
    else {
        panic!("expected completed tool result")
    };
    invocation.append_result(
        &mut model_request,
        id,
        result,
        &mut model_tokens,
        model::Limits::default(),
    )?;
    let mut next_model_wire = [0; 8192];
    let report = model_request.encode(
        model::Protocol::OpenAiChat.into(),
        &mut next_model_wire,
        &mut [],
        &mut model_tokens,
        model::EncodeContext::default(),
        model::Policy::default(),
    )?;
    println!(
        "Next model request, retaining model call ID {}:",
        invocation.model_call_id()
    );
    println!(
        "{}",
        core::str::from_utf8(&next_model_wire[..report.written]).expect("JSON output")
    );
    Ok(())
}
