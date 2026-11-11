//! A bounded echo tool server. The application, not zmcpio, executes the tool.
use std::io::{self, BufRead, Write};
use zmcpio::{
    json::{Document, Limits, Token},
    mcp::{Action, Server},
    tools::{Content, Tool, ToolResult},
    transport::{FrameStatus, LineDecoder},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut schema_tokens = [Token::EMPTY; 32];
    let schema = Document::parse(
        br#"{"type":"object","properties":{}}"#,
        &mut schema_tokens,
        Limits::default(),
    )?;
    let tools = [Tool::new(
        "echo",
        "Return validated arguments as text",
        schema.root(),
    )?];
    let mut server = Server::new("zmcpio-echo", env!("CARGO_PKG_VERSION"));
    let mut tokens = [Token::EMPTY; 1024];
    let mut frame_storage = [0; 65536];
    let mut decoder = LineDecoder::new(&mut frame_storage, 65536);
    let mut output = [0; 262144];
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut writer = stdout.lock();
    loop {
        let bytes = input.fill_buf()?;
        if bytes.is_empty() {
            decoder
                .finish()
                .map_err(|e| format!("incomplete stdio input: {e:?}"))?;
            break;
        }
        let step = decoder.feed(bytes);
        input.consume(step.consumed);
        match step.status {
            FrameStatus::NeedInput => continue,
            FrameStatus::Ready => {}
            _ => return Err(format!("frame rejected: {:?}", step.status).into()),
        }
        let frame = decoder.message().ok_or("missing frame")?;
        let written = match server.handle(frame, &mut tokens, Limits::default(), &mut output)? {
            Action::Reply(n) => Some(n),
            Action::ListTools { context, cursor } => Some(if cursor.is_some() {
                server.error(context, -32602, "Invalid cursor", &mut output)?
            } else {
                server.list_tools(context, &tools, None, &mut output)?
            }),
            Action::CallTool(call) => {
                // Explicit application allowlist. No shell, filesystem or network tools.
                Some(if call.name.matches("echo") {
                    let text = core::str::from_utf8(call.arguments_bytes())?;
                    server.tool_result(
                        call.context,
                        ToolResult::new(&[Content::Text(text)]),
                        &mut output,
                    )?
                } else {
                    server.error(call.context, -32602, "Unknown tool", &mut output)?
                })
            }
            Action::Notification(_) | Action::IncomingResponse(_) => None,
        };
        if let Some(n) = written {
            writer.write_all(&output[..n])?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        decoder.consume();
    }
    Ok(())
}
