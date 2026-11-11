use zmcpio::{
    ErrorKind,
    batch::{self, Batch, BatchKind, LEGACY_BATCH_VERSION as VERSION},
    client::{Client, Event, Pending},
    json::{Document, Limits, Token, Value},
    mcp::{Action, Era, Notice, Server},
    rpc::{self, Message},
    tools::{Content, ToolResult},
};

fn json<'a>(input: &'a [u8], tokens: &'a mut [Token]) -> Value<'a> {
    Document::parse(input, tokens, Limits::default())
        .unwrap()
        .root()
}

fn ready(client: &mut Client<'_>, server: &mut Server<'_>) {
    let mut request = [0; 2048];
    let mut response = [0; 2048];
    let mut request_tokens = [Token::EMPTY; 128];
    let mut response_tokens = [Token::EMPTY; 128];
    let sent = client
        .initialize_with_version(VERSION, &mut request)
        .unwrap();
    let Action::Reply(len) = server
        .handle(
            &request[..sent.len],
            &mut request_tokens,
            Limits::default(),
            &mut response,
        )
        .unwrap()
    else {
        panic!("initialize");
    };
    client
        .receive(&response[..len], &mut response_tokens, Limits::default())
        .unwrap();
    let len = client.initialized(&mut request).unwrap();
    assert!(matches!(
        server
            .handle(
                &request[..len],
                &mut request_tokens,
                Limits::default(),
                &mut response,
            )
            .unwrap(),
        Action::Notification(Notice::Initialized)
    ));
}

#[test]
fn nonempty_count_and_exact_revision_are_enforced() {
    let mut tokens = [Token::EMPTY; 128];
    let root = json(
        br#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notice"},7]"#,
        &mut tokens,
    );
    let batch = Batch::parse(root, VERSION, 3).unwrap();
    assert_eq!(batch.version(), VERSION);
    assert_eq!(batch.len(), 3);
    assert!(!batch.is_empty());
    assert_eq!(batch.kind(), BatchKind::Requests);
    assert_eq!(batch.raw().raw(), root.raw());
    let entries: Vec<_> = batch.entries().collect();
    assert!(matches!(entries[0].message, Ok(Message::Request(_))));
    assert!(matches!(entries[1].message, Ok(Message::Notification(_))));
    assert_eq!(entries[2].message.unwrap_err().kind, ErrorKind::InvalidRpc);
    let error = Batch::parse(root, VERSION, 2).unwrap_err();
    assert_eq!((error.kind, error.needed), (ErrorKind::Members, 3));
    for version in [
        "2024-11-05",
        "2025-06-18",
        "2025-11-25",
        "2026-07-28",
        "unknown",
    ] {
        assert_eq!(
            Batch::parse(root, version, 3).unwrap_err().kind,
            ErrorKind::UnsupportedVersion
        );
    }
    assert_eq!(rpc::parse(root).unwrap_err().kind, ErrorKind::InvalidRpc);
    for raw in [b"[]".as_slice(), b"{}".as_slice(), b"null".as_slice()] {
        let root = json(raw, &mut tokens);
        assert_eq!(
            Batch::parse(root, VERSION, 10).unwrap_err().kind,
            ErrorKind::InvalidRpc
        );
    }
}

#[test]
fn initialize_and_modern_metadata_are_rejected_before_dispatch() {
    for raw in [
        br#"[{"jsonrpc":"2.0","id":1,"method":"initialize"}]"#.as_slice(),
        br#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"method":"\u0069nitialize"}]"#.as_slice(),
        br#"[{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}]"#.as_slice(),
    ] {
        let mut tokens = [Token::EMPTY; 128];
        let root = json(raw, &mut tokens);
        let mut dispatched = 0;
        if let Ok(batch) = Batch::parse(root, VERSION, 8) {
            for _ in batch.entries() { dispatched += 1; }
        }
        assert_eq!(dispatched, 0);
        assert!(Batch::parse(root, VERSION, 8).is_err());
    }
}

#[test]
fn response_categories_are_separate_and_invalid_replies_never_request_replies() {
    let mut tokens = [Token::EMPTY; 128];
    let root = json(br#"[{"jsonrpc":"2.0","id":1,"result":{}},7]"#, &mut tokens);
    let batch = Batch::parse(root, VERSION, 2).unwrap();
    assert_eq!(batch.kind(), BatchKind::Responses);
    assert!(batch.entries().nth(1).unwrap().message.is_err());
    let root = json(
        br#"[{"jsonrpc":"2.0","id":1,"result":{}},{"jsonrpc":"2.0","method":"notice"}]"#,
        &mut tokens,
    );
    assert_eq!(
        Batch::parse(root, VERSION, 2).unwrap_err().kind,
        ErrorKind::InvalidRpc
    );
    let root = json(br#"[1,true,null]"#, &mut tokens);
    assert_eq!(
        Batch::parse(root, VERSION, 3).unwrap().kind(),
        BatchKind::Requests
    );
}

#[test]
fn multiple_tool_calls_and_notification_roundtrip_out_of_order() {
    let mut pending = [Pending::EMPTY; 2];
    let mut client = Client::new("client", "1", Era::Legacy, &mut pending);
    let mut server = Server::new("server", "1");
    ready(&mut client, &mut server);
    let mut first_bytes = [0; 2048];
    let mut second_bytes = [0; 2048];
    let first = client.call_tool("first", None, &mut first_bytes).unwrap();
    let second = client.call_tool("second", None, &mut second_bytes).unwrap();
    let mut first_tokens = [Token::EMPTY; 128];
    let mut second_tokens = [Token::EMPTY; 128];
    let mut notice_tokens = [Token::EMPTY; 32];
    let messages = [
        json(&first_bytes[..first.len], &mut first_tokens),
        json(
            br#"{"jsonrpc":"2.0","method":"notifications/example"}"#,
            &mut notice_tokens,
        ),
        json(&second_bytes[..second.len], &mut second_tokens),
    ];
    let mut wire = [0; 8192];
    let len = batch::encode_requests(VERSION, &messages, 3, &mut wire).unwrap();
    let mut batch_tokens = [Token::EMPTY; 512];
    let requests = Batch::parse(json(&wire[..len], &mut batch_tokens), VERSION, 3).unwrap();
    let mut executed = 0;
    let mut noticed = 0;
    let mut replies = Vec::new();
    for entry in requests.entries() {
        let mut tokens = [Token::EMPTY; 128];
        let mut output = [0; 2048];
        match server
            .handle(
                entry.raw.raw().as_bytes(),
                &mut tokens,
                Limits::default(),
                &mut output,
            )
            .unwrap()
        {
            Action::CallTool(call) => {
                // Only the host explicitly chooses to execute, never Batch::parse.
                executed += 1;
                let content = [Content::Text(if call.name.matches("first") {
                    "first"
                } else {
                    "second"
                })];
                let len = server
                    .tool_result(
                        call.context,
                        ToolResult {
                            content: &content,
                            structured_content: None,
                            is_error: false,
                        },
                        &mut output,
                    )
                    .unwrap();
                replies.push(output[..len].to_vec());
            }
            Action::Notification(_) => noticed += 1,
            _ => panic!("unexpected batch action"),
        }
    }
    assert_eq!((executed, noticed), (2, 1));
    replies.reverse();
    let mut reply_tokens = vec![[Token::EMPTY; 128]; replies.len()];
    let responses: Vec<_> = replies
        .iter()
        .zip(reply_tokens.iter_mut())
        .map(|(bytes, tokens)| Some(json(bytes, tokens)))
        .collect();
    let mut reply_wire = [0; 8192];
    let len = batch::encode_responses(VERSION, &responses, 3, &mut reply_wire).unwrap();
    let mut incoming_tokens = [Token::EMPTY; 256];
    let incoming =
        Batch::parse(json(&reply_wire[..len], &mut incoming_tokens), VERSION, 3).unwrap();
    assert_eq!(incoming.kind(), BatchKind::Responses);

    let mut tokens = [Token::EMPTY; 256];
    assert_eq!(
        client
            .receive(&reply_wire[..len], &mut tokens, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRpc
    );
    assert_eq!(client.pending_count(), 2);
    let mut completed = Vec::new();
    for entry in incoming.entries() {
        let Event::ToolResult { id, .. } = client
            .receive(entry.raw.raw().as_bytes(), &mut tokens, Limits::default())
            .unwrap()
        else {
            panic!("tool result");
        };
        completed.push(id);
    }
    assert_eq!(completed, [second.id, first.id]);
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn malformed_request_items_have_individual_invalid_request_responses() {
    let mut tokens = [Token::EMPTY; 128];
    let root = json(br#"[{"jsonrpc":"2.0","id":9,"method":"ping"},17,{"jsonrpc":"2.0","method":"notifications/example"},{"jsonrpc":"2.0","id":4},[]]"#, &mut tokens);
    let batch = Batch::parse(root, VERSION, 5).unwrap();
    let mut server = Server::new("server", "1");
    let mut errors = 0;
    let mut valid = 0;
    let mut silent = 0;
    for entry in batch.entries() {
        let mut item_tokens = [Token::EMPTY; 64];
        let mut output = [0; 512];
        match server
            .handle(
                entry.raw.raw().as_bytes(),
                &mut item_tokens,
                Limits::default(),
                &mut output,
            )
            .unwrap()
        {
            Action::Reply(len) => {
                let mut reply_tokens = [Token::EMPTY; 64];
                let reply = json(&output[..len], &mut reply_tokens);
                if entry.message.is_err() {
                    assert_eq!(reply.get("id").unwrap().raw(), "null");
                    assert_eq!(
                        reply
                            .get("error")
                            .unwrap()
                            .get("code")
                            .unwrap()
                            .integer()
                            .unwrap(),
                        -32600
                    );
                    errors += 1;
                } else {
                    assert!(reply.get("result").is_some());
                    valid += 1;
                }
            }
            Action::Notification(_) => silent += 1,
            _ => panic!("unexpected action"),
        }
    }
    assert_eq!((valid, errors, silent), (1, 3, 1));
}

#[test]
fn all_notifications_emit_zero_bytes_and_no_empty_array() {
    let mut tokens = [Token::EMPTY; 64];
    let root = json(
        br#"[{"jsonrpc":"2.0","method":"one"},{"jsonrpc":"2.0","method":"two"}]"#,
        &mut tokens,
    );
    assert_eq!(
        Batch::parse(root, VERSION, 2).unwrap().kind(),
        BatchKind::Notifications
    );
    let mut output = [0xa5; 128];
    assert_eq!(
        batch::encode_responses(VERSION, &[None, None], 2, &mut output).unwrap(),
        0
    );
    assert_eq!(output, [0xa5; 128]);
    assert_eq!(
        batch::encode_responses(VERSION, &[], 0, &mut []).unwrap(),
        0
    );
    assert_eq!(
        batch::encode_responses(VERSION, &[None, None], 1, &mut output)
            .unwrap_err()
            .kind,
        ErrorKind::Members
    );
}

#[test]
fn encoding_is_atomic_for_capacity_invalid_items_and_unsupported_revisions() {
    let mut response_tokens = [Token::EMPTY; 32];
    let response = json(
        br#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
        &mut response_tokens,
    );
    let expected = format!("[{}]", response.raw());
    let mut short = [0xa5; 8];
    let error = batch::encode_responses(VERSION, &[Some(response)], 1, &mut short).unwrap_err();
    assert_eq!(
        (error.kind, error.needed),
        (ErrorKind::Buffer, expected.len())
    );
    assert_eq!(short, [0xa5; 8]);
    let mut output = [0xa5; 512];
    let len =
        batch::encode_responses(VERSION, &[None, Some(response), None], 3, &mut output).unwrap();
    assert_eq!(&output[..len], expected.as_bytes());
    assert!(output[len..].iter().all(|byte| *byte == 0xa5));

    let mut request_tokens = [Token::EMPTY; 32];
    let request = json(
        br#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        &mut request_tokens,
    );
    for version in ["2024-11-05", "2025-06-18", "2026-07-28"] {
        output.fill(0xa5);
        assert_eq!(
            batch::encode_requests(version, &[request], 1, &mut output)
                .unwrap_err()
                .kind,
            ErrorKind::UnsupportedVersion
        );
        assert_eq!(output, [0xa5; 512]);
        assert_eq!(
            batch::encode_responses(version, &[Some(response)], 1, &mut output)
                .unwrap_err()
                .kind,
            ErrorKind::UnsupportedVersion
        );
        assert_eq!(output, [0xa5; 512]);
    }
    output.fill(0xa5);
    assert_eq!(
        batch::encode_responses(VERSION, &[Some(response), Some(request)], 2, &mut output)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRpc
    );
    assert_eq!(output, [0xa5; 512]);
    assert_eq!(
        batch::encode_requests(VERSION, &[request, response], 2, &mut output)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRpc
    );
    assert_eq!(output, [0xa5; 512]);
    assert_eq!(
        batch::encode_requests(VERSION, &[], 1, &mut output)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRpc
    );
    assert_eq!(output, [0xa5; 512]);
    let error = batch::encode_requests(VERSION, &[request], 1, &mut short).unwrap_err();
    assert_eq!(
        (error.kind, error.needed),
        (ErrorKind::Buffer, request.raw().len() + 2)
    );
    assert_eq!(short, [0xa5; 8]);
}
