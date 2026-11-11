//! Connect to an explicitly selected stdio MCP server, discover tools, and call
//! one explicitly selected tool. No shell, automatic tool choice, or retries.
//!
//! cargo build --examples --features std
//! cargo run --features std --example mcp_client -- \
//!   target/debug/examples/mcp_stdio echo '{"text":"hello"}'
//!
//! Arguments: SERVER TOOL ARGUMENTS_JSON [SERVER_ARGUMENTS...]
//! The blocking stdio adapter has no response deadline. Applications requiring
//! timed reads should drive Client through their own deadline-aware transport.
use std::{error::Error, process::Command, time::Duration};
use zmcpio::{
    client::{Client, Event, Pending},
    json::{Document, Kind, Limits, Token},
    mcp::Era,
    rpc,
    stdio::StdioTransport,
    transport::{MessageReader, MessageWriter, TransportError},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn io_error(error: TransportError<std::io::Error>) -> Box<dyn Error> {
    format!("MCP transport failure: {error:?}").into()
}

/// Process notifications separately from replies; unsupported callbacks get an
/// explicit JSON-RPC error. The finite message budget prevents notification-only
/// loops, but is deliberately not advertised as a wall-clock timeout.
fn await_reply(
    client: &mut Client<'_>,
    transport: &mut StdioTransport<'_>,
    input: &mut [u8],
    tokens: &mut [Token],
    mut accept: impl FnMut(Event<'_>) -> Result<()>,
) -> Result<()> {
    for _ in 0..32 {
        let len = transport.receive(input).map_err(io_error)?;
        match client.receive(&input[..len], tokens, Limits::default())? {
            Event::Notification(notification) => {
                eprintln!("MCP notification: {:?}", notification.method);
            }
            Event::ServerRequest(request) => {
                let mut output = [0; 1024];
                let len = rpc::failure(
                    Some(request.id),
                    -32601,
                    "Client callback not supported",
                    None,
                    &mut output,
                )?;
                transport.send(&output[..len]).map_err(io_error)?;
            }
            Event::Failure { id, error } => {
                return Err(format!("MCP request {id:?} failed with code {}", error.code).into());
            }
            Event::InputRequired { .. } => {
                return Err("Server requested an unsupported interactive continuation".into());
            }
            event => return accept(event),
        }
    }
    Err("Exceeded the 32-message budget while waiting for a reply".into())
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let program = args
        .next()
        .ok_or("Usage: mcp_client SERVER TOOL ARGUMENTS_JSON [SERVER_ARGUMENTS...]")?;
    let tool = args
        .next()
        .ok_or("Missing tool name")?
        .into_string()
        .map_err(|_| "Tool name is not UTF-8")?;
    let arguments = args
        .next()
        .ok_or("Missing tool arguments JSON")?
        .into_string()
        .map_err(|_| "Arguments are not UTF-8")?;
    let mut argument_tokens = [Token::EMPTY; 1024];
    let arguments = Document::parse(
        arguments.as_bytes(),
        &mut argument_tokens,
        Limits::default(),
    )?;
    if arguments.root().kind() != Kind::Object {
        return Err("Tool arguments must be a JSON object".into());
    }

    let mut command = Command::new(program);
    command.args(args);
    let mut framing = [0; 65536];
    let mut transport = StdioTransport::spawn(&mut command, &mut framing, 65536)?;
    let mut pending = [Pending::EMPTY; 4];
    let mut client = Client::new(
        "zmcpio-client",
        env!("CARGO_PKG_VERSION"),
        Era::Legacy,
        &mut pending,
    );
    let mut output = [0; 65536];
    let mut input = [0; 65536];
    let mut tokens = [Token::EMPTY; 4096];

    let sent = client.initialize(&mut output)?;
    transport.send(&output[..sent.len]).map_err(io_error)?;
    await_reply(
        &mut client,
        &mut transport,
        &mut input,
        &mut tokens,
        |event| {
            if let Event::Initialized { id, .. } = event
                && id == sent.id
            {
                Ok(())
            } else {
                Err("Expected initialization response".into())
            }
        },
    )?;
    let len = client.initialized(&mut output)?;
    transport.send(&output[..len]).map_err(io_error)?;

    let mut cursor_storage = [0; 4096];
    let mut next_storage = [0; 4096];
    let mut cursor_len = None;
    let mut selected_tool_found = false;
    for page_number in 0..128 {
        let cursor = cursor_len
            .map(|len| core::str::from_utf8(&cursor_storage[..len]))
            .transpose()?;
        let sent = client.list_tools(cursor, &mut output)?;
        transport.send(&output[..sent.len]).map_err(io_error)?;
        let mut next_len = None;
        await_reply(
            &mut client,
            &mut transport,
            &mut input,
            &mut tokens,
            |event| {
                let Event::Tools { id, page } = event else {
                    return Err("Expected tools/list response".into());
                };
                if id != sent.id {
                    return Err("Unexpected tools/list response ID".into());
                }
                for discovered in page.tools() {
                    selected_tool_found |= discovered.name().matches(&tool);
                }
                if let Some(next) = page.next_cursor() {
                    next_len = Some(next.decode_into(&mut next_storage)?.len());
                }
                Ok(())
            },
        )?;
        match next_len {
            Some(len) => {
                if cursor_len == Some(len) && cursor_storage[..len] == next_storage[..len] {
                    return Err("Server repeated its pagination cursor".into());
                }
                if page_number == 127 {
                    return Err("Exceeded the 128-page tools/list budget".into());
                }
                cursor_storage[..len].copy_from_slice(&next_storage[..len]);
                cursor_len = Some(len);
            }
            None => break,
        }
    }
    if !selected_tool_found {
        return Err("The explicitly selected tool was not advertised by the server".into());
    }

    let sent = client.call_tool(&tool, Some(arguments.root()), &mut output)?;
    transport.send(&output[..sent.len]).map_err(io_error)?;
    let mut tool_failed = false;
    await_reply(
        &mut client,
        &mut transport,
        &mut input,
        &mut tokens,
        |event| {
            let Event::ToolResult { id, result } = event else {
                return Err("Expected tools/call response".into());
            };
            if id != sent.id {
                return Err("Unexpected tools/call response ID".into());
            }
            tool_failed = result.is_error();
            println!("{}", result.raw().raw());
            Ok(())
        },
    )?;
    client.close();
    let status = transport.close_gracefully(Duration::from_secs(2))?;
    if tool_failed {
        return Err("Remote tool returned isError=true".into());
    }
    if !status.success() {
        return Err("MCP server did not exit successfully".into());
    }
    Ok(())
}
