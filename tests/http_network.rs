#![cfg(feature = "std")]

use std::{
    io::{BufReader, Read},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};
use zmcpio::{
    client::{Client, Event, Pending},
    http::{self, BodyKind, ClientSession, Header, Incoming, Method, SecurityPolicy, ServerConfig, Version},
    http_io::{self, BodyReader},
    json::{Document, Limits, Token},
    mcp::{Action, Era, Server},
    rpc,
    sse,
    tools::{Content, ToolResult},
};

struct TestPolicy;
impl SecurityPolicy for TestPolicy {
    fn origin_allowed(&self, origin: &str) -> bool { origin == "http://localhost" }
    fn authorized(&self, headers: &[Header<'_>]) -> bool {
        http::header(headers, "x-test-auth").unwrap() == Some("yes")
    }
    fn session_valid(&self, _: &str, _: &[Header<'_>]) -> bool { false }
}

fn deadlines(stream: &TcpStream) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
}

fn roundtrip(streamed: bool, authorize: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        deadlines(&socket);
        let mut reader = BufReader::with_capacity(19, socket.try_clone().unwrap());
        let mut head_storage = [0; 4096];
        let head = http_io::read_head(&mut reader, &mut head_storage).unwrap();
        assert_eq!(head.target(), Some("/mcp"));
        let mut headers = [Header { name: "", value: "" }; 32];
        let count = head.copy_headers(&mut headers).unwrap();
        let mut raw = [0; 4096];
        let mut body = BodyReader::new(&mut reader, head, raw.len()).unwrap();
        let len = http_io::read_body(&mut body, &mut raw).unwrap();
        let mut tokens = [Token::EMPTY; 256];
        let document = Document::parse(&raw[..len], &mut tokens, Limits::default()).unwrap();
        let message = rpc::parse(document.root()).unwrap();
        let incoming = Incoming { method: Method::Post, headers: &headers[..count], message: Some(message) };
        let validated = ServerConfig::stateless(Version::V2026_07_28)
            .validate(incoming, &TestPolicy, &mut [0; 512]);
        if let Err(error) = validated {
            assert!(!authorize);
            http_io::write_response(&mut socket, error.status(), &[], b"").unwrap();
            return;
        }
        assert!(authorize);
        let mut protocol = Server::new("network-test", "1");
        let mut output = [0; 4096];
        let Action::CallTool(call) = protocol.handle(&raw[..len], &mut tokens, Limits::default(), &mut output).unwrap() else {
            panic!("expected explicit tool action");
        };
        assert!(call.name.matches("echo"));
        let result_len = protocol.tool_result(call.context, ToolResult::new(&[Content::Text("wire-ok")]), &mut output).unwrap();
        if streamed {
            let mut writer = http_io::start_sse(&mut socket, &[Header { name: "X-Accel-Buffering", value: "no" }]).unwrap();
            writer.send(b": heartbeat\r\n\r\n").unwrap();
            let mut event = [0; 8192];
            let n = sse::write_event(sse::Event { data: Some(&output[..result_len]), event: "message", id: None, retry_ms: None }, &mut event).unwrap();
            // Split SSE across HTTP chunks, including within UTF-8/field boundaries.
            for chunk in event[..n].chunks(7) {
                writer.send(chunk).unwrap();
            }
            writer.finish().unwrap();
        } else {
            http_io::write_response(&mut socket, 200, &[Header { name: "Content-Type", value: http::JSON }], &output[..result_len]).unwrap();
        }
    });
    let mut socket = TcpStream::connect(address).unwrap();
    deadlines(&socket);
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("network-client", "1", Era::Modern, &mut slots);
    let mut output = [0; 4096];
    let call = client.call_tool("echo", None, &mut output).unwrap();
    let session = ClientSession::new(Version::V2026_07_28, &mut []);
    let mut header_text = [0; 1024];
    let n = session.post_headers("tools/call", Some("echo"), &mut header_text).unwrap();
    let mut headers = [Header { name: "", value: "" }; 32];
    let mut count = http_io::header_views(core::str::from_utf8(&header_text[..n]).unwrap(), &mut headers).unwrap();
    if authorize {
        headers[count] = Header { name: "X-Test-Auth", value: "yes" };
        count += 1;
    }
    http_io::write_request(&mut socket, "POST", "/mcp", &address.to_string(), &headers[..count], &output[..call.len]).unwrap();
    let mut reader = BufReader::with_capacity(13, socket);
    let mut head_storage = [0; 4096];
    let head = http_io::read_head(&mut reader, &mut head_storage).unwrap();
    if !authorize {
        assert_eq!(head.status(), Some(401));
        assert_eq!(client.pending_count(), 1, "HTTP errors never complete a tool");
        client.abandon(call.id).unwrap();
        server.join().unwrap();
        return;
    }
    let mode = http::response_mode(head.status().unwrap(), head.header("content-type"), BodyKind::Request).unwrap();
    let mut body = BodyReader::new(&mut reader, head, 16384).unwrap();
    let mut tokens = [Token::EMPTY; 256];
    if mode == http::ResponseMode::Json {
        let n = http_io::read_body(&mut body, &mut output).unwrap();
        let Event::ToolResult { id, result } = client.receive(&output[..n], &mut tokens, Limits::default()).unwrap() else {
            panic!("expected JSON tool result");
        };
        assert_eq!(id, call.id);
        assert!(!result.is_error());
    } else {
        assert_eq!(mode, http::ResponseMode::Sse);
        let mut line = [0; 4096];
        let mut data = [0; 4096];
        let mut event = [0; 64];
        let mut id = [0; 64];
        let mut decoder = sse::Decoder::new(&mut line, &mut data, &mut event, &mut id, 8192);
        let mut network = [0; 11];
        let mut results = 0;
        loop {
            let n = body.read(&mut network).unwrap();
            if n == 0 { break; }
            let mut at = 0;
            while at < n {
                let step = decoder.feed(&network[at..n]);
                at += step.consumed;
                match step.status {
                    sse::Status::Ready => {
                        let event = decoder.event().unwrap();
                        if let Some(data) = event.data {
                            let Event::ToolResult { id, result } = client.receive(data, &mut tokens, Limits::default()).unwrap() else {
                                panic!("expected SSE tool result");
                            };
                            assert_eq!(id, call.id);
                            assert!(!result.is_error());
                            results += 1;
                        }
                        decoder.consume();
                    }
                    sse::Status::NeedInput => {}
                    status => panic!("unexpected SSE status: {status:?}"),
                }
            }
        }
        decoder.finish().unwrap();
        assert_eq!(results, 1);
    }
    assert_eq!(client.pending_count(), 0);
    server.join().unwrap();
}

#[test]
fn actual_tcp_json_response() { roundtrip(false, true); }

#[test]
fn actual_tcp_chunked_sse_response() { roundtrip(true, true); }

#[test]
fn authentication_failure_precedes_tool_dispatch() { roundtrip(false, false); }
