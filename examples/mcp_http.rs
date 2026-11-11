//! Local Streamable HTTP echo server/client with real bounded socket I/O.
//! cargo run --features std --example mcp_http -- server 8080 sse
//! cargo run --features std --example mcp_http -- client 127.0.0.1:8080
//! The demo is loopback-only, has no privileged tools, and is not a TLS server.
use std::{
    io::{BufReader, Read},
    net::{SocketAddr, TcpListener, TcpStream},
    time::Duration,
};
use zmcpio::{
    client::{Client, Event, Pending},
    http::{self, BodyKind, ClientSession, Header, Incoming, Method, SecurityPolicy, ServerConfig, Version},
    http_io::{self, BodyReader},
    json::{Document, Limits, Token},
    mcp::{Action, Era, Server},
    rpc::{self, Message},
    sse,
    tools::{Content, Tool, ToolResult},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct LocalPolicy<'a> { host: &'a str, origin: &'a str }
impl SecurityPolicy for LocalPolicy<'_> {
    fn origin_allowed(&self, origin: &str) -> bool { origin == self.origin }
    fn authorized(&self, headers: &[Header<'_>]) -> bool {
        // Explicit local-demo policy; a real service verifies authentication here.
        http::header(headers, "host").ok().flatten() == Some(self.host)
    }
    fn session_valid(&self, _: &str, _: &[Header<'_>]) -> bool { false }
}
fn deadline(socket: &TcpStream) -> Result<()> {
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    socket.set_write_timeout(Some(Duration::from_secs(10)))?;
    socket.set_nodelay(true)?;
    Ok(())
}
fn serve(mut socket: TcpStream, streamed: bool, host: &str) -> Result<()> {
    deadline(&socket)?;
    let mut reader = BufReader::with_capacity(8192, socket.try_clone()?);
    let mut head_bytes = [0; 8192];
    let head = http_io::read_head(&mut reader, &mut head_bytes)?;
    let mut headers = [Header { name: "", value: "" }; 64];
    let count = head.copy_headers(&mut headers).map_err(|_| "too many headers")?;
    let headers = &headers[..count];
    let policy_origin = format!("http://{host}");
    let policy = LocalPolicy { host, origin: &policy_origin };
    let config = ServerConfig::stateless(Version::V2026_07_28);
    let mut scratch = [0; 1024];
    let mut output = [0; 65536];
    if head.target() != Some("/mcp") {
        http_io::write_response(&mut socket, 404, &[], b"")?;
        return Ok(());
    }
    let method = match head.method() {
        Some("POST") => Method::Post,
        Some("GET") => Method::Get,
        Some("DELETE") => Method::Delete,
        _ => {
            http_io::write_response(&mut socket, 405, &[Header { name: "Allow", value: "POST" }], b"")?;
            return Ok(());
        }
    };
    let mut request = [0; 16384];
    let mut body = BodyReader::new(&mut reader, head, request.len())?;
    let len = http_io::read_body(&mut body, &mut request)?;
    let mut tokens = [Token::EMPTY; 1024];
    let message = if method == Method::Post {
        let document = match Document::parse(&request[..len], &mut tokens, Limits::default()) {
            Ok(document) => document,
            Err(_) => {
                let n = rpc::failure(None, -32700, "Parse error", None, &mut output)?;
                http_io::write_response(&mut socket, 400, &[Header { name: "Content-Type", value: http::JSON }], &output[..n])?;
                return Ok(());
            }
        };
        Some(rpc::parse(document.root())?)
    } else { None };
    let incoming = Incoming { method, headers, message };
    if let Err(error) = config.validate(incoming, &policy, &mut scratch) {
        let id = match message { Some(Message::Request(r)) => Some(r.id), _ => None };
        let requested = http::header(headers, "mcp-protocol-version")?.unwrap_or("");
        let n = error.write_rpc(id, config.version, requested, &mut output)?;
        http_io::write_response(&mut socket, error.status(), &[Header { name: "Content-Type", value: http::JSON }], &output[..n])?;
        return Ok(());
    }
    let mut protocol = Server::new("zmcpio-http-echo", env!("CARGO_PKG_VERSION"));
    let action = protocol.handle(&request[..len], &mut tokens, Limits::default(), &mut output)?;
    let n = match action {
        Action::Reply(n) => n,
        Action::ListTools { context, cursor } => {
            if cursor.is_some() {
                protocol.error(context, -32602, "Invalid cursor", &mut output)?
            } else {
                let mut schema_tokens = [Token::EMPTY; 32];
                let schema = Document::parse(br#"{"type":"object","properties":{}}"#, &mut schema_tokens, Limits::default())?;
                let tool = Tool::new("echo", "Echo arguments without executing commands", schema.root())?;
                protocol.list_tools(context, &[tool], None, &mut output)?
            }
        }
        Action::CallTool(call) if call.name.matches("echo") => {
            let text = core::str::from_utf8(call.arguments_bytes())?;
            protocol.tool_result(call.context, ToolResult::new(&[Content::Text(text)]), &mut output)?
        }
        Action::CallTool(call) => protocol.error(call.context, -32602, "Unknown tool", &mut output)?,
        Action::Extension { context, .. } => protocol.error(context, -32601, "Method not found", &mut output)?,
        Action::Notification(_) => {
            http_io::write_response(&mut socket, 202, &[], b"")?;
            return Ok(());
        }
        Action::IncomingResponse(_) => return Err("unexpected client response".into()),
    };
    let mut response_tokens = [Token::EMPTY; 1024];
    let document = Document::parse(&output[..n], &mut response_tokens, Limits::default())?;
    let status = match rpc::parse(document.root())? {
        Message::Failure { error, .. } => http::rpc_error_status(config.version, error.code),
        _ => 200,
    };
    if streamed && status == 200 {
        let mut writer = http_io::start_sse(&mut socket, &[
            Header { name: "Cache-Control", value: "no-cache" },
            Header { name: "X-Accel-Buffering", value: "no" },
        ])?;
        writer.send(b": connected\n\n")?;
        let mut event = [0; 131072];
        let len = sse::write_event(sse::Event { data: Some(&output[..n]), event: "message", id: None, retry_ms: None }, &mut event)?;
        writer.send(&event[..len])?;
        writer.finish()?;
    } else {
        http_io::write_response(&mut socket, status, &[Header { name: "Content-Type", value: http::JSON }], &output[..n])?;
    }
    Ok(())
}
fn call(address: SocketAddr) -> Result<()> {
    if !address.ip().is_loopback() { return Err("demo plaintext client is loopback-only; supply verified TLS for remote hosts".into()); }
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    deadline(&socket)?;
    let mut slots = [Pending::EMPTY; 4];
    let mut client = Client::new("zmcpio-http-client", "1", Era::Modern, &mut slots);
    let mut output = [0; 16384];
    let mut argument_tokens = [Token::EMPTY; 32];
    let arguments = Document::parse(br#"{"text":"hello over HTTP"}"#, &mut argument_tokens, Limits::default())?;
    let call = client.call_tool("echo", Some(arguments.root()), &mut output)?;
    let session = ClientSession::new(Version::V2026_07_28, &mut []);
    let mut text = [0; 2048];
    let n = session.post_headers("tools/call", Some("echo"), &mut text)?;
    let mut headers = [Header { name: "", value: "" }; 32];
    let count = http_io::header_views(core::str::from_utf8(&text[..n])?, &mut headers)?;
    http_io::write_request(&mut socket, "POST", "/mcp", &address.to_string(), &headers[..count], &output[..call.len])?;
    let mut reader = BufReader::with_capacity(8192, socket);
    let mut head_bytes = [0; 8192];
    let head = http_io::read_head(&mut reader, &mut head_bytes)?;
    let mode = http::response_mode(head.status().ok_or("missing response status")?, head.header("content-type"), BodyKind::Request)?;
    let mut body = BodyReader::new(&mut reader, head, 1 << 20)?;
    let mut tokens = [Token::EMPTY; 1024];
    match mode {
        http::ResponseMode::Json => {
            let len = http_io::read_body(&mut body, &mut output)?;
            print_result(client.receive(&output[..len], &mut tokens, Limits::default())?)?;
        }
        http::ResponseMode::Sse => {
            let mut line = [0; 16384];
            let mut data = [0; 16384];
            let mut event_name = [0; 128];
            let mut id = [0; 1024];
            let mut decoder = sse::Decoder::new(&mut line, &mut data, &mut event_name, &mut id, 65536);
            let mut bytes = [0; 4096];
            loop {
                let n = body.read(&mut bytes)?;
                if n == 0 {
                    decoder.finish()?;
                    break;
                }
                let mut at = 0;
                while at < n {
                    let step = decoder.feed(&bytes[at..n]);
                    at += step.consumed;
                    match step.status {
                        sse::Status::NeedInput => {}
                        sse::Status::Ready => {
                            let event = decoder.event().ok_or("missing event")?;
                            if event.event == "message"
                                && let Some(data) = event.data.filter(|data| !data.is_empty())
                            {
                                print_result(client.receive(data, &mut tokens, Limits::default())?)?;
                            }
                            decoder.consume();
                        }
                        status => return Err(format!("SSE capacity/framing failure: {status:?}").into()),
                    }
                }
            }
        }
        http::ResponseMode::Empty => return Err("tool request received an empty response".into()),
    }
    if client.pending_count() != 0 { return Err("stream closed without a final response; tool NOT retried".into()); }
    Ok(())
}
fn print_result(event: Event<'_>) -> Result<()> {
    match event {
        Event::ToolResult { result, .. } => println!("{}", result.raw().raw()),
        Event::Notification(_) => {}
        Event::Failure { error, .. } => return Err(format!("JSON-RPC error {}", error.code).into()),
        _ => return Err("unexpected protocol event".into()),
    }
    Ok(())
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("server") => {
            let port: u16 = args.next().unwrap_or_else(|| "8080".into()).parse()?;
            let streamed = args.next().as_deref() != Some("json");
            let once = args.next().as_deref() == Some("--once");
            let listener = TcpListener::bind(("127.0.0.1", port))?;
            let address = listener.local_addr()?.to_string();
            println!("http://{address}/mcp");
            for socket in listener.incoming() {
                if let Err(error) = serve(socket?, streamed, &address) { eprintln!("{error}"); }
                if once { break; }
            }
            Ok(())
        }
        Some("client") => call(args.next().ok_or("provide 127.0.0.1:PORT")?.parse()?),
        _ => Err("usage: mcp_http server [PORT] [json|sse] [--once] | client 127.0.0.1:PORT".into()),
    }
}
