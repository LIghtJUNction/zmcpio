#![cfg(feature = "std")]

use std::{
    io::{BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};
use zmcpio::{
    client::{Client, ClientState, Event, Pending},
    http::Header,
    http_io::{self, BodyReader, ChunkWriter, Connection},
    json::{Limits, Token},
    legacy_sse::{self, Session},
    mcp::{Action, Era, LegacyState, Notice, Server},
    sse,
    tools::{Content, ToolResult},
    ErrorKind,
};

fn socket_pair(listener: &TcpListener) -> (TcpStream, TcpStream) {
    let client = TcpStream::connect_timeout(
        &listener.local_addr().unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    // connect_timeout succeeded before accept: there is already a queued peer.
    let (server, _) = listener.accept().unwrap();
    for socket in [&client, &server] {
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        socket.set_nodelay(true).unwrap();
    }
    (client, server)
}

fn event<W: Write>(writer: &mut ChunkWriter<'_, W>, name: &str, data: &[u8]) {
    let mut encoded = [0; 8192];
    let len = sse::write_event(
        sse::Event {
            data: Some(data),
            event: name,
            id: None,
            retry_ms: None,
        },
        &mut encoded,
    )
    .unwrap();
    // Neither HTTP chunks nor TCP reads need coincide with SSE fields/events.
    for fragment in encoded[..len].chunks(7) {
        writer.send(fragment).unwrap();
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Received {
    Endpoint,
    Message(usize),
    End,
}

fn next_event<R: Read>(
    body: &mut R,
    decoder: &mut sse::Decoder<'_>,
    session: &mut Session<'_>,
    message: &mut [u8],
) -> Received {
    // One byte deliberately exercises split CRLF/fields and leaves no unread
    // suffix to lose when control returns to send the next POST.
    let mut byte = [0; 1];
    loop {
        if body.read(&mut byte).unwrap() == 0 {
            decoder.finish().unwrap();
            return Received::End;
        }
        let step = decoder.feed(&byte);
        assert_eq!(step.consumed, 1);
        match step.status {
            sse::Status::NeedInput => {}
            sse::Status::Ready => {
                let received = match session.on_event(decoder.event().unwrap()).unwrap() {
                    legacy_sse::Action::EndpointReady => Some(Received::Endpoint),
                    legacy_sse::Action::Message(data) => {
                        assert!(data.len() <= message.len());
                        message[..data.len()].copy_from_slice(data);
                        Some(Received::Message(data.len()))
                    }
                    legacy_sse::Action::Ignore => None,
                };
                decoder.consume();
                if let Some(received) = received {
                    return received;
                }
            }
            status => panic!("unexpected bounded SSE decoder status: {status:?}"),
        }
    }
}

fn post(
    socket: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    session: &Session<'_>,
    body: &[u8],
    last: bool,
) {
    http_io::write_request_with(
        socket,
        "POST",
        session.endpoint_target().unwrap(),
        session.authority(),
        &[Header {
            name: "Content-Type",
            value: "application/json",
        }],
        body,
        if last { Connection::Close } else { Connection::KeepAlive },
    )
    .unwrap();
    let mut storage = [0; 2048];
    let head = http_io::read_head(reader, &mut storage).unwrap();
    assert_eq!(head.status(), Some(202));
    assert_eq!(head.header("content-length"), Some("0"));
    assert_eq!(head.header("content-type"), None);
    let mut response = BodyReader::new(reader, head, 16).unwrap();
    assert_eq!(http_io::read_body(&mut response, &mut [0; 16]).unwrap(), 0);
}

fn legacy_roundtrip(duplicate_tool_reply: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let authority = listener.local_addr().unwrap().to_string();
    let base_url = format!("http://{authority}/sse");
    let endpoint_url = format!("http://{authority}/messages?session=a");
    // Only two connected sockets per peer. Closing the listener also ensures
    // this fixture cannot silently open another connection and replay a call.
    let (mut client_sse, mut server_sse) = socket_pair(&listener);
    let (mut client_post, mut server_post) = socket_pair(&listener);
    drop(listener);

    let server = thread::spawn(move || {
        let mut sse_reader = BufReader::with_capacity(13, server_sse.try_clone().unwrap());
        let mut storage = [0; 2048];
        let head = http_io::read_head(&mut sse_reader, &mut storage).unwrap();
        assert_eq!(head.method(), Some("GET"));
        assert_eq!(head.target(), Some("/sse"));
        assert_eq!(head.header("accept"), Some("text/event-stream"));
        let mut body = BodyReader::new(&mut sse_reader, head, 16).unwrap();
        assert_eq!(http_io::read_body(&mut body, &mut [0; 16]).unwrap(), 0);
        let mut stream = http_io::start_sse(&mut server_sse, &[]).unwrap();
        stream.send(b": legacy heartbeat\r\n\r\n").unwrap();
        event(&mut stream, "endpoint", endpoint_url.as_bytes());

        let mut posts = BufReader::with_capacity(19, server_post.try_clone().unwrap());
        // Initialization state belongs to the MCP connection, not each POST.
        let mut protocol = Server::new("legacy-network-test", "1");
        let mut tokens = [Token::EMPTY; 256];
        let mut raw = [0; 4096];
        let mut output = [0; 4096];
        let mut post_count = 0;
        let mut tool_executions = 0;
        for index in 0..3 {
            let head = http_io::read_head(&mut posts, &mut storage).unwrap();
            assert_eq!(head.method(), Some("POST"));
            assert_eq!(head.target(), Some("/messages?session=a"));
            assert_eq!(head.header("content-type"), Some("application/json"));
            assert_eq!(head.header("mcp-protocol-version"), None);
            assert_eq!(head.header("mcp-session-id"), None);
            assert_eq!(head.header("mcp-method"), None);
            let mut body = BodyReader::new(&mut posts, head, raw.len()).unwrap();
            let len = http_io::read_body(&mut body, &mut raw).unwrap();
            post_count += 1;
            let reply = match protocol
                .handle(&raw[..len], &mut tokens, Limits::default(), &mut output)
                .unwrap()
            {
                Action::Reply(len) => {
                    assert_eq!(index, 0);
                    assert_eq!(protocol.legacy_protocol_version(), Some("2024-11-05"));
                    assert_eq!(protocol.legacy_state(), LegacyState::AwaitInitialized);
                    Some(len)
                }
                Action::Notification(Notice::Initialized) => {
                    assert_eq!(index, 1);
                    assert_eq!(protocol.legacy_state(), LegacyState::Ready);
                    None
                }
                Action::CallTool(call) => {
                    assert_eq!(index, 2);
                    assert_eq!(protocol.legacy_state(), LegacyState::Ready);
                    assert!(call.name.matches("echo"));
                    tool_executions += 1;
                    Some(
                        protocol
                            .tool_result(
                                call.context,
                                ToolResult::new(&[Content::Text("legacy-wire-ok")]),
                                &mut output,
                            )
                            .unwrap(),
                    )
                }
                action => panic!("unexpected legacy action: {action:?}"),
            };
            // The POST never carries the JSON-RPC result. Even initialize is
            // answered only on the already-open GET event stream.
            http_io::write_response_with(
                &mut server_post,
                202,
                &[],
                b"",
                if index == 2 { Connection::Close } else { Connection::KeepAlive },
            )
            .unwrap();
            if let Some(len) = reply {
                event(&mut stream, "message", &output[..len]);
                if duplicate_tool_reply && index == 2 {
                    event(&mut stream, "message", &output[..len]);
                }
            }
        }
        stream.finish().unwrap();
        (post_count, tool_executions)
    });

    http_io::write_request(
        &mut client_sse,
        "GET",
        "/sse",
        &authority,
        &[Header {
            name: "Accept",
            value: "text/event-stream",
        }],
        b"",
    )
    .unwrap();
    let mut reader = BufReader::with_capacity(11, client_sse);
    let mut head_storage = [0; 2048];
    let head = http_io::read_head(&mut reader, &mut head_storage).unwrap();
    assert_eq!(head.status(), Some(200));
    assert_eq!(head.header("content-type"), Some("text/event-stream"));
    assert_eq!(head.header("transfer-encoding"), Some("chunked"));
    let mut body = BodyReader::new(&mut reader, head, 32768).unwrap();
    let mut line = [0; 4096];
    let mut data = [0; 4096];
    let mut event_name = [0; 64];
    let mut event_id = [0; 64];
    let mut decoder =
        sse::Decoder::new(&mut line, &mut data, &mut event_name, &mut event_id, 8192);
    let mut target = [0; 512];
    let mut session = Session::new(&base_url, &mut target).unwrap();
    let mut message = [0; 4096];
    assert_eq!(
        next_event(&mut body, &mut decoder, &mut session, &mut message),
        Received::Endpoint
    );
    assert_eq!(session.endpoint_target(), Some("/messages?session=a"));

    let mut post_reader = BufReader::with_capacity(17, client_post.try_clone().unwrap());
    let mut pending = [Pending::EMPTY; 2];
    let mut client = Client::new("legacy-network-client", "1", Era::Legacy, &mut pending);
    let mut outgoing = [0; 4096];
    let mut tokens = [Token::EMPTY; 256];
    let initialize = client.initialize_with_version("2024-11-05", &mut outgoing).unwrap();
    post(&mut client_post, &mut post_reader, &session, &outgoing[..initialize.len], false);
    assert_eq!(client.state(), ClientState::Initializing, "HTTP 202 is not an MCP reply");
    assert_eq!(client.pending_count(), 1);
    let Received::Message(len) =
        next_event(&mut body, &mut decoder, &mut session, &mut message)
    else {
        panic!("missing legacy initialize reply on GET stream");
    };
    let Event::Initialized { id, .. } =
        client.receive(&message[..len], &mut tokens, Limits::default()).unwrap()
    else {
        panic!("expected negotiated legacy initialization");
    };
    assert_eq!(id, initialize.id);
    assert_eq!(client.protocol_version(), Some("2024-11-05"));
    assert_eq!(client.state(), ClientState::AwaitInitialized);

    let len = client.initialized(&mut outgoing).unwrap();
    post(&mut client_post, &mut post_reader, &session, &outgoing[..len], false);
    assert_eq!(client.state(), ClientState::Ready);
    assert_eq!(client.pending_count(), 0);

    let call = client.call_tool("echo", None, &mut outgoing).unwrap();
    post(&mut client_post, &mut post_reader, &session, &outgoing[..call.len], true);
    assert_eq!(client.pending_count(), 1, "POST acceptance cannot complete the call");
    let Received::Message(len) =
        next_event(&mut body, &mut decoder, &mut session, &mut message)
    else {
        panic!("missing tool result on GET stream");
    };
    let Event::ToolResult { id, result } =
        client.receive(&message[..len], &mut tokens, Limits::default()).unwrap()
    else {
        panic!("expected legacy tool result");
    };
    assert_eq!(id, call.id);
    assert!(!result.is_error());
    assert!(core::str::from_utf8(&message[..len]).unwrap().contains("legacy-wire-ok"));
    assert_eq!(client.pending_count(), 0);

    if duplicate_tool_reply {
        let Received::Message(len) =
            next_event(&mut body, &mut decoder, &mut session, &mut message)
        else {
            panic!("missing deliberately duplicated SSE reply");
        };
        let error = client.receive(&message[..len], &mut tokens, Limits::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnknownId);
        assert_eq!(client.pending_count(), 0);
    }
    assert_eq!(
        next_event(&mut body, &mut decoder, &mut session, &mut message),
        Received::End
    );
    assert_eq!(server.join().unwrap(), (3, 1), "three POSTs, exactly one tool execution");
}

#[test]
fn actual_tcp_2024_http_plus_sse_roundtrip() {
    legacy_roundtrip(false);
}

#[test]
fn duplicate_legacy_sse_reply_never_replays_a_tool() {
    legacy_roundtrip(true);
}
