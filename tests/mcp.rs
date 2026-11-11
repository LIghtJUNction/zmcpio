use zmcpio::{
    ErrorKind,
    json::{Document, Limits, Token},
    mcp::{Action, Era, LegacyState, Notice, Server},
    rpc::Id,
    tools::{Content, Tool, ToolResult},
    tracker::{Reuse, Slot, Tracker},
};

fn modern(method: &str, fields: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"{method}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}{fields}}}}}"#
    )
}

#[test]
fn legacy_initialization_is_transactional() {
    let raw = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;
    let mut server = Server::new("zmcpio", "0.1.0");
    let mut t = [Token::EMPTY; 128];
    assert_eq!(
        server
            .handle(raw, &mut t, Limits::default(), &mut [])
            .unwrap_err()
            .kind,
        ErrorKind::Buffer
    );
    assert_eq!(server.legacy_state(), LegacyState::New);
    let mut out = [0; 1024];
    let Action::Reply(n) = server
        .handle(raw, &mut t, Limits::default(), &mut out)
        .unwrap()
    else {
        panic!()
    };
    let mut rt = [Token::EMPTY; 128];
    let d = Document::parse(&out[..n], &mut rt, Limits::default()).unwrap();
    assert!(
        d.root()
            .get("result")
            .unwrap()
            .get("protocolVersion")
            .unwrap()
            .string()
            .unwrap()
            .matches("2024-11-05")
    );
    let Action::Notification(Notice::Initialized) = server
        .handle(
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &mut t,
            Limits::default(),
            &mut out,
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(server.legacy_state(), LegacyState::Ready);
}

#[test]
fn modern_discovery_and_metadata_required() {
    let mut server = Server::new("server", "1");
    let mut t = [Token::EMPTY; 128];
    let mut out = [0; 1024];
    let raw = modern("server/discover", "");
    let Action::Reply(n) = server
        .handle(raw.as_bytes(), &mut t, Limits::default(), &mut out)
        .unwrap()
    else {
        panic!()
    };
    let mut rt = [Token::EMPTY; 128];
    let d = Document::parse(&out[..n], &mut rt, Limits::default()).unwrap();
    assert!(
        d.root()
            .get("result")
            .unwrap()
            .get("resultType")
            .unwrap()
            .string()
            .unwrap()
            .matches("complete")
    );
    assert_eq!(server.legacy_state(), LegacyState::New);
    let bad = raw
        .replace(r#",\"unused\":0"#, "")
        .replace(r#",\"none\":0"#, "");
    assert_eq!(bad, raw);
    let missing = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#;
    let Action::Reply(n) = server
        .handle(missing.as_bytes(), &mut t, Limits::default(), &mut out)
        .unwrap()
    else {
        panic!()
    };
    let d = Document::parse(&out[..n], &mut rt, Limits::default()).unwrap();
    assert_eq!(
        d.root()
            .get("error")
            .unwrap()
            .get("code")
            .unwrap()
            .integer()
            .unwrap(),
        -32602
    );
}

#[test]
fn calls_are_actions_never_implicit_execution() {
    let mut server = Server::new("s", "1");
    let mut t = [Token::EMPTY; 128];
    let mut out = [0; 2048];
    let raw = modern(
        "tools/call",
        r#", "name":"\u0065cho", "arguments":{"text":"hi"}"#,
    );
    let Action::CallTool(call) = server
        .handle(raw.as_bytes(), &mut t, Limits::default(), &mut out)
        .unwrap()
    else {
        panic!()
    };
    assert!(call.name.matches("echo"));
    assert_eq!(call.arguments_bytes(), br#"{"text":"hi"}"#);
    let content = [Content::Text("tool failed")];
    let n = server
        .tool_result(
            call.context,
            ToolResult {
                content: &content,
                structured_content: None,
                is_error: true,
            },
            &mut out,
        )
        .unwrap();
    let mut rt = [Token::EMPTY; 128];
    let d = Document::parse(&out[..n], &mut rt, Limits::default()).unwrap();
    let r = d.root().get("result").unwrap();
    assert_eq!(r.get("isError").unwrap().boolean(), Some(true));
    assert!(r.get("_meta").is_some());
}

#[test]
fn omitted_arguments_lists_and_structured_results() {
    let mut server = Server::new("s", "1");
    let mut t = [Token::EMPTY; 128];
    let mut st = [Token::EMPTY; 32];
    let mut out = [0; 2048];
    let schema = Document::parse(
        br#"{"type":"object","properties":{}}"#,
        &mut st,
        Limits::default(),
    )
    .unwrap();
    let tool = Tool::new("echo", "Echo", schema.root()).unwrap();
    let raw = modern("tools/list", "");
    let Action::ListTools { context, cursor } = server
        .handle(raw.as_bytes(), &mut t, Limits::default(), &mut out)
        .unwrap()
    else {
        panic!()
    };
    assert!(cursor.is_none());
    let n = server.list_tools(context, &[tool], None, &mut out).unwrap();
    let mut rt = [Token::EMPTY; 128];
    Document::parse(&out[..n], &mut rt, Limits::default()).unwrap();
    let raw = modern("tools/call", r#", "name":"echo""#);
    let Action::CallTool(call) = server
        .handle(raw.as_bytes(), &mut t, Limits::default(), &mut out)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(call.arguments_bytes(), b"{}");
    let blocks = [
        Content::Text("ok"),
        Content::Image {
            data: "aGk=",
            mime_type: "image/png",
        },
        Content::Audio {
            data: "",
            mime_type: "audio/wav",
        },
        Content::ResourceLink {
            uri: "file:///test",
            name: "test",
        },
        Content::ResourceText {
            uri: "file:///test",
            mime_type: "text/plain",
            text: "x",
        },
        Content::ResourceBlob {
            uri: "file:///test",
            mime_type: "application/octet-stream",
            blob: "aGk=",
        },
    ];
    let n = server
        .tool_result(
            call.context,
            ToolResult {
                content: &blocks,
                structured_content: Some(schema.root()),
                is_error: false,
            },
            &mut out,
        )
        .unwrap();
    let d = Document::parse(&out[..n], &mut rt, Limits::default()).unwrap();
    assert_eq!(
        d.root()
            .get("result")
            .unwrap()
            .get("content")
            .unwrap()
            .elements()
            .count(),
        6
    );
    assert_eq!(call.context.era, Era::Modern);
}

#[test]
fn notices_responses_and_bad_json_do_not_become_calls() {
    let mut s = Server::new("s", "1");
    let mut t = [Token::EMPTY; 128];
    let mut out = [0; 1024];
    let Action::Notification(Notice::Cancelled { request_id, .. }) = s
        .handle(
            br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}"#,
            &mut t,
            Limits::default(),
            &mut out,
        )
        .unwrap()
    else {
        panic!()
    };
    assert!(request_id.equals(Id::Number(7)));
    assert!(matches!(
        s.handle(
            br#"{"jsonrpc":"2.0","id":7,"result":{}}"#,
            &mut t,
            Limits::default(),
            &mut out
        )
        .unwrap(),
        Action::IncomingResponse(_)
    ));
    assert!(matches!(
        s.handle(b"{broken", &mut t, Limits::default(), &mut out)
            .unwrap(),
        Action::Reply(_)
    ));
    assert!(matches!(
        s.handle(
            br#"{"jsonrpc":"2.0","method":"unknown"}"#,
            &mut t,
            Limits::default(),
            &mut out
        )
        .unwrap(),
        Action::Notification(_)
    ));
}

#[test]
fn duplicate_ids_cancellation_and_progress_are_bounded() {
    let mut slots = [Slot::EMPTY; 2];
    let mut tracker = Tracker::new(&mut slots, Reuse::AfterCompletion);
    tracker.start(Id::Number(1)).unwrap();
    tracker.start(Id::Number(2)).unwrap();
    assert_eq!(
        tracker.start(Id::Number(1)).unwrap_err().kind,
        ErrorKind::DuplicateId
    );
    assert_eq!(
        tracker.start(Id::Number(3)).unwrap_err().kind,
        ErrorKind::Buffer
    );
    tracker.progress(Id::Number(1), 0.0).unwrap();
    tracker.progress(Id::Number(1), 1.0).unwrap();
    assert!(tracker.progress(Id::Number(1), 1.0).is_err());
    assert!(tracker.cancel(Id::Number(2)));
    assert!(tracker.is_cancelled(Id::Number(2)));
    assert!(!tracker.cancel(Id::Number(3)));
    tracker.finish(Id::Number(2)).unwrap();
    tracker.start(Id::Number(2)).unwrap();
    let mut slots = [Slot::EMPTY; 1];
    let mut legacy = Tracker::new(&mut slots, Reuse::Never);
    legacy.start(Id::Number(1)).unwrap();
    legacy.finish(Id::Number(1)).unwrap();
    assert_eq!(
        legacy.start(Id::Number(1)).unwrap_err().kind,
        ErrorKind::DuplicateId
    );
}
