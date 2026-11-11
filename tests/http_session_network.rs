#![cfg(feature = "std")]
//! Host-managed 2025 session/resumption across real loopback TCP connections.
//! Deliberately truncated HTTP/SSE is not a successful tool completion. Only an
//! explicit GET resumes stored events; the side-effecting tool POST is never retried.

use std::{
    io::{self, BufReader, Read},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};
use zmcpio::{
    client::{Client, Event, Pending},
    http::{
        self, ClientSession, Header, HttpError, Incoming, Method, SecurityPolicy, ServerAction,
        ServerConfig, Version,
    },
    http_io::{self, BodyReader},
    json::{Document, Limits, Token},
    mcp::{Action, Era, LegacyState, Server},
    rpc, sse,
    tools::{Content, ToolResult},
};

const SESSION: &str = "network-session-1";
const CHECKPOINT: &str = "checkpoint-1";
const FINAL_EVENT: &str = "checkpoint-2";
const EMPTY: Header<'static> = Header {
    name: "",
    value: "",
};
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const PROGRESS: &[u8] = br#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":2,"progress":1,"total":1}}"#;

struct Policy {
    alive: bool,
}
impl SecurityPolicy for Policy {
    fn origin_allowed(&self, origin: &str) -> bool {
        origin == "http://localhost"
    }
    fn authorized(&self, headers: &[Header<'_>]) -> bool {
        http::header(headers, "x-test-identity").unwrap() == Some("fixture-user")
    }
    fn session_valid(&self, id: &str, headers: &[Header<'_>]) -> bool {
        self.alive && id == SESSION && self.authorized(headers)
    }
}
fn deadlines(socket: &TcpStream) {
    // Accepted sockets do not have identical inheritance rules on every OS.
    socket.set_nonblocking(false).unwrap();
    socket.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
    socket.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
    socket.set_nodelay(true).unwrap();
}
fn accept(listener: &TcpListener) -> TcpStream {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                deadlines(&socket);
                return socket;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < end, "bounded accept deadline");
                thread::yield_now();
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                assert!(Instant::now() < end, "bounded accept deadline");
            }
            Err(error) => panic!("accept failed: {error}"),
        }
    }
}
fn send(address: SocketAddr, method: &str, fields: &[u8], body: &[u8]) -> BufReader<TcpStream> {
    let mut socket = TcpStream::connect_timeout(&address, IO_TIMEOUT).unwrap();
    deadlines(&socket);
    let mut headers = [EMPTY; 16];
    let n = http_io::header_views(core::str::from_utf8(fields).unwrap(), &mut headers).unwrap();
    headers[n] = Header {
        name: "X-Test-Identity",
        value: "fixture-user",
    };
    headers[n + 1] = Header {
        name: "Origin",
        value: "http://localhost",
    };
    http_io::write_request(
        &mut socket,
        method,
        "/mcp",
        &address.to_string(),
        &headers[..n + 2],
        body,
    )
    .unwrap();
    BufReader::with_capacity(17, socket)
}
fn empty_response(mut reader: BufReader<TcpStream>) -> u16 {
    let mut storage = [0; 2048];
    let head = http_io::read_head(&mut reader, &mut storage).unwrap();
    let status = head.status().unwrap();
    let mut body = BodyReader::new(&mut reader, head, 1).unwrap();
    assert_eq!(http_io::read_body(&mut body, &mut [0]).unwrap(), 0);
    status
}

fn server(listener: TcpListener) -> (usize, usize, usize, bool) {
    let mut protocol = Server::new("session-fixture", "1");
    let mut policy = Policy { alive: false };
    let config = ServerConfig {
        version: Version::V2025_11_25,
        allow_get: true,
        allow_delete: true,
        sessions: true,
    };
    let mut replay = [0; 4096];
    let mut replay_len = 0;
    let (mut tool_posts, mut executions, mut resumes) = (0, 0, 0);
    let mut deleted = false;
    for step in 0..6 {
        let mut socket = accept(&listener);
        let mut reader = BufReader::with_capacity(19, socket.try_clone().unwrap());
        let mut head_storage = [0; 2048];
        let head = http_io::read_head(&mut reader, &mut head_storage).unwrap();
        assert_eq!(head.target(), Some("/mcp"));
        let method = match head.method().unwrap() {
            "POST" => Method::Post,
            "GET" => Method::Get,
            "DELETE" => Method::Delete,
            other => panic!("unexpected method {other}"),
        };
        let mut headers = [EMPTY; 16];
        let count = head.copy_headers(&mut headers).unwrap();
        let mut request = [0; 4096];
        let mut body = BodyReader::new(&mut reader, head, request.len()).unwrap();
        let len = http_io::read_body(&mut body, &mut request).unwrap();
        let mut validation_tokens = [Token::EMPTY; 128];
        let message = if method == Method::Post {
            Some(
                rpc::parse(
                    Document::parse(&request[..len], &mut validation_tokens, Limits::default())
                        .unwrap()
                        .root(),
                )
                .unwrap(),
            )
        } else {
            assert_eq!(len, 0);
            None
        };
        let action = config.validate(
            Incoming {
                method,
                headers: &headers[..count],
                message,
            },
            &policy,
            &mut [0; 256],
        );
        if step == 5 {
            assert_eq!(method, Method::Get);
            assert_eq!(action, Err(HttpError::SessionExpired));
            http_io::write_response(&mut socket, 404, &[], b"").unwrap();
            continue;
        }
        let action = action.unwrap();
        if step == 3 {
            assert_eq!(method, Method::Get);
            assert_eq!(
                action,
                ServerAction::Listen {
                    last_event_id: Some(CHECKPOINT)
                }
            );
            assert_eq!((tool_posts, executions), (1, 1));
            resumes += 1;
            let mut encoded = [0; 8192];
            let n = sse::write_event(
                sse::Event {
                    data: Some(&replay[..replay_len]),
                    event: "message",
                    id: Some(FINAL_EVENT),
                    retry_ms: None,
                },
                &mut encoded,
            )
            .unwrap();
            let mut chunks = http_io::start_sse(&mut socket, &[]).unwrap();
            for fragment in encoded[..n].chunks(7) {
                chunks.send(fragment).unwrap();
            }
            chunks.finish().unwrap();
            continue;
        }
        if step == 4 {
            assert_eq!(method, Method::Delete);
            assert_eq!(
                action,
                ServerAction::Terminate {
                    session_id: SESSION
                }
            );
            policy.alive = false;
            deleted = true;
            http_io::write_response(&mut socket, 204, &[], b"").unwrap();
            continue;
        }
        assert_eq!(method, Method::Post);
        assert_eq!(
            head.header("mcp-session-id"),
            if step == 0 { None } else { Some(SESSION) }
        );
        let mut tokens = [Token::EMPTY; 128];
        let mut output = [0; 4096];
        match protocol
            .handle(&request[..len], &mut tokens, Limits::default(), &mut output)
            .unwrap()
        {
            Action::Reply(n) => {
                assert_eq!(step, 0);
                assert_eq!(protocol.legacy_state(), LegacyState::AwaitInitialized);
                policy.alive = true;
                http_io::write_response(
                    &mut socket,
                    200,
                    &[
                        Header {
                            name: "Content-Type",
                            value: http::JSON,
                        },
                        Header {
                            name: "Mcp-Session-Id",
                            value: SESSION,
                        },
                    ],
                    &output[..n],
                )
                .unwrap();
            }
            Action::Notification(_) => {
                assert_eq!(step, 1);
                assert_eq!(protocol.legacy_state(), LegacyState::Ready);
                http_io::write_response(&mut socket, 202, &[], b"").unwrap();
            }
            Action::CallTool(call) => {
                assert_eq!(step, 2);
                assert!(call.name.matches("echo"));
                tool_posts += 1;
                executions += 1; // The fixture's one explicit side effect.
                replay_len = protocol
                    .tool_result(
                        call.context,
                        ToolResult::new(&[Content::Text("ran-once")]),
                        &mut replay,
                    )
                    .unwrap();
                let mut encoded = [0; 1024];
                let n = sse::write_event(
                    sse::Event {
                        data: Some(PROGRESS),
                        event: "message",
                        id: Some(CHECKPOINT),
                        retry_ms: Some(0),
                    },
                    &mut encoded,
                )
                .unwrap();
                {
                    let mut chunks = http_io::start_sse(&mut socket, &[]).unwrap();
                    chunks.send(&encoded[..n]).unwrap();
                    // Not a complete SSE event and not an acknowledged checkpoint.
                    chunks
                        .send(b"id: checkpoint-2\ndata: {\"jsonrpc\":\"2.0\",")
                        .unwrap();
                    // Intentionally omit finish(): simulate network loss before the
                    // final response and before the terminating HTTP zero chunk.
                }
                socket.shutdown(Shutdown::Write).unwrap();
            }
            _ => panic!("unexpected protocol action"),
        }
    }
    (tool_posts, executions, resumes, deleted)
}

/// Returns whether HTTP framing ended cleanly. Completion is counted separately.
fn receive_stream(
    mut reader: BufReader<TcpStream>,
    client: &mut Client<'_>,
    checkpoint: &mut [u8; 64],
    checkpoint_len: &mut usize,
    results: &mut usize,
) -> bool {
    let mut head_storage = [0; 2048];
    let head = http_io::read_head(&mut reader, &mut head_storage).unwrap();
    assert_eq!(head.status(), Some(200));
    assert_eq!(head.header("content-type"), Some(http::SSE));
    let mut body = BodyReader::new(&mut reader, head, 16384).unwrap();
    let (mut line, mut data, mut name, mut id) = ([0; 4096], [0; 4096], [0; 32], [0; 64]);
    let mut decoder = sse::Decoder::new(&mut line, &mut data, &mut name, &mut id, 8192);
    let mut tokens = [Token::EMPTY; 128];
    let mut network = [0; 13];
    let end = Instant::now() + Duration::from_secs(10);
    for _ in 0..4096 {
        assert!(Instant::now() < end, "bounded stream deadline");
        let n = match body.read(&mut network) {
            Ok(n) => n,
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
                decoder.finish().unwrap(); // Discard only the incomplete event.
                return false;
            }
        };
        if n == 0 {
            decoder.finish().unwrap();
            return true;
        }
        let mut at = 0;
        while at < n {
            let step = decoder.feed(&network[at..n]);
            at += step.consumed;
            match step.status {
                sse::Status::Ready => {
                    let event = decoder.event().unwrap();
                    if let Some(data) = event.data {
                        match client
                            .receive(data, &mut tokens, Limits::default())
                            .unwrap()
                        {
                            Event::Notification(_) => assert_eq!(client.pending_count(), 1),
                            Event::ToolResult { id, result } => {
                                assert_eq!(id, 2);
                                assert!(!result.is_error());
                                assert!(result.raw().raw().contains("ran-once"));
                                *results += 1;
                            }
                            _ => panic!("unexpected SSE protocol event"),
                        }
                    }
                    // Commit the cursor only after a complete accepted event.
                    if let Some(id) = event.id {
                        assert!(id.len() <= checkpoint.len());
                        checkpoint[..id.len()].copy_from_slice(id.as_bytes());
                        *checkpoint_len = id.len();
                    }
                    decoder.consume();
                }
                sse::Status::NeedInput => {}
                other => panic!("unexpected bounded SSE status: {other:?}"),
            }
        }
    }
    panic!("stream exceeded finite read budget");
}

#[test]
fn session_disconnect_resume_and_delete_execute_tool_exactly_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || server(listener));
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("session-client", "1", Era::Legacy, &mut slots);
    let mut session_storage = [0; 64];
    let mut session = ClientSession::new(Version::V2025_11_25, &mut session_storage);
    let (mut request, mut fields, mut tokens) = ([0; 4096], [0; 1024], [Token::EMPTY; 128]);

    let outgoing = client.initialize(&mut request).unwrap();
    let n = session
        .post_headers("initialize", None, &mut fields)
        .unwrap();
    let mut reader = send(address, "POST", &fields[..n], &request[..outgoing.len]);
    let mut head_storage = [0; 2048];
    let head = http_io::read_head(&mut reader, &mut head_storage).unwrap();
    assert_eq!(head.status(), Some(200));
    let assigned = head.header("mcp-session-id").unwrap();
    assert_eq!(assigned, SESSION);
    let mut response = [0; 4096];
    let mut body = BodyReader::new(&mut reader, head, response.len()).unwrap();
    let n = http_io::read_body(&mut body, &mut response).unwrap();
    assert!(matches!(
        client
            .receive(&response[..n], &mut tokens, Limits::default())
            .unwrap(),
        Event::Initialized { .. }
    ));
    session.set_session_id(Some(assigned)).unwrap();
    assert_eq!(client.protocol_version(), Some("2025-11-25"));
    let len = client.initialized(&mut request).unwrap();
    let n = session
        .post_headers("notifications/initialized", None, &mut fields)
        .unwrap();
    assert_eq!(
        empty_response(send(address, "POST", &fields[..n], &request[..len])),
        202
    );

    let call = client
        .call_tool_with_progress("echo", None, &mut request)
        .unwrap();
    assert_eq!(call.id, 2);
    let n = session
        .post_headers("tools/call", Some("echo"), &mut fields)
        .unwrap();
    let mut checkpoint = [0; 64];
    let mut checkpoint_len = 0;
    let mut results = 0;
    assert!(!receive_stream(
        send(address, "POST", &fields[..n], &request[..call.len]),
        &mut client,
        &mut checkpoint,
        &mut checkpoint_len,
        &mut results,
    ));
    assert_eq!(results, 0, "transport EOF must not complete a tool");
    assert_eq!(client.pending_count(), 1);
    assert_eq!(
        core::str::from_utf8(&checkpoint[..checkpoint_len]).unwrap(),
        CHECKPOINT
    );

    // Explicit host decision: GET the stored response, never POST the tool again.
    let cursor = core::str::from_utf8(&checkpoint[..checkpoint_len]).unwrap();
    let n = session.get_headers(Some(cursor), &mut fields).unwrap();
    assert!(receive_stream(
        send(address, "GET", &fields[..n], b""),
        &mut client,
        &mut checkpoint,
        &mut checkpoint_len,
        &mut results,
    ));
    assert_eq!(results, 1);
    assert_eq!(client.pending_count(), 0);
    assert_eq!(
        core::str::from_utf8(&checkpoint[..checkpoint_len]).unwrap(),
        FINAL_EVENT
    );

    let n = session.delete_headers(&mut fields).unwrap();
    assert_eq!(
        empty_response(send(address, "DELETE", &fields[..n], b"")),
        204
    );
    // Simulate an in-flight request using the now-expired local session snapshot.
    let n = session.get_headers(Some(FINAL_EVENT), &mut fields).unwrap();
    let status = empty_response(send(address, "GET", &fields[..n], b""));
    assert_eq!(status, 404);
    assert!(session.observe_status(status, Some(SESSION)));
    assert_eq!(session.session_id(), None);
    assert_eq!(worker.join().unwrap(), (1, 1, 1, true));
}
