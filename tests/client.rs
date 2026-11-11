use zmcpio::{
    ErrorKind,
    client::{Client, ClientState, Event, Pending},
    json::{Document, Limits, Token, Value},
    mcp::{Action, CLIENT_CAPABILITIES_KEY, Era, PROTOCOL_VERSION, Server, VERSION_KEY},
    tools::{Content, Tool, ToolResult},
};

fn response(id: impl core::fmt::Display, result: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#)
}

fn initialize_result(id: impl core::fmt::Display, version: &str) -> String {
    response(
        id,
        &format!(
            r#"{{"protocolVersion":"{version}","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"fixture","version":"1"}}}}"#
        ),
    )
}

fn parse<'a>(input: &'a [u8], tokens: &'a mut [Token]) -> Value<'a> {
    Document::parse(input, tokens, Limits::default())
        .unwrap()
        .root()
}

fn legacy_ready(client: &mut Client<'_>) {
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let initialize = client.initialize(&mut output).unwrap();
    let reply = initialize_result(initialize.id, "2025-11-25");
    client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    client.initialized(&mut output).unwrap();
}

#[test]
fn modern_requests_include_per_request_metadata() {
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("test-client", "1.2.3", Era::Modern, &mut slots);
    let mut output = [0; 2048];
    let mut tokens = [Token::EMPTY; 128];
    assert_eq!(client.state(), ClientState::Ready);
    let first = client.discover(&mut output).unwrap();
    let request = parse(&output[..first.len], &mut tokens);
    assert_eq!(request.get("id").unwrap().integer().unwrap(), first.id);
    assert!(
        request
            .get("method")
            .unwrap()
            .string()
            .unwrap()
            .matches("server/discover")
    );
    let meta = request.get("params").unwrap().get("_meta").unwrap();
    assert!(
        meta.get(VERSION_KEY)
            .unwrap()
            .string()
            .unwrap()
            .matches(PROTOCOL_VERSION)
    );
    assert!(meta.get(CLIENT_CAPABILITIES_KEY).is_some());
    let info = meta.get("io.modelcontextprotocol/clientInfo").unwrap();
    assert!(
        info.get("name")
            .unwrap()
            .string()
            .unwrap()
            .matches("test-client")
    );
    assert!(
        info.get("version")
            .unwrap()
            .string()
            .unwrap()
            .matches("1.2.3")
    );
    let second = client.list_tools(None, &mut output).unwrap();
    assert!(second.id > first.id);
    let request = parse(&output[..second.len], &mut tokens);
    assert!(
        request
            .get("params")
            .unwrap()
            .get("_meta")
            .unwrap()
            .get(CLIENT_CAPABILITIES_KEY)
            .is_some()
    );
    assert_eq!(client.pending_count(), 2);
}

#[test]
fn legacy_handshake_and_output_failures_are_transactional() {
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
    let mut output = [0; 2048];
    let mut tokens = [Token::EMPTY; 128];
    assert_eq!(client.state(), ClientState::New);
    assert!(client.list_tools(None, &mut output).is_err());
    assert_eq!(
        client.initialize(&mut []).unwrap_err().kind,
        ErrorKind::Buffer
    );
    assert_eq!(client.state(), ClientState::New);
    assert_eq!(client.pending_count(), 0);
    let request = client.initialize(&mut output).unwrap();
    assert_eq!(client.state(), ClientState::Initializing);
    assert_eq!(client.pending_count(), 1);
    assert!(client.initialize(&mut output).is_err());
    let reply = initialize_result(request.id, "2025-11-25");
    assert!(
        matches!(client.receive(reply.as_bytes(), &mut tokens, Limits::default()).unwrap(), Event::Initialized { id, .. } if id == request.id)
    );
    assert_eq!(client.state(), ClientState::AwaitInitialized);
    assert_eq!(client.pending_count(), 0);
    assert!(client.list_tools(None, &mut output).is_err());
    assert_eq!(
        client.initialized(&mut []).unwrap_err().kind,
        ErrorKind::Buffer
    );
    assert_eq!(client.state(), ClientState::AwaitInitialized);
    let n = client.initialized(&mut output).unwrap();
    let notice = parse(&output[..n], &mut tokens);
    assert!(notice.get("id").is_none());
    assert!(
        notice
            .get("method")
            .unwrap()
            .string()
            .unwrap()
            .matches("notifications/initialized")
    );
    assert_eq!(client.state(), ClientState::Ready);
    assert!(client.list_tools(None, &mut output).is_ok());
}

#[test]
fn unsupported_legacy_version_never_becomes_ready() {
    for unsupported in ["1900-01-01", "2025-01-01", PROTOCOL_VERSION] {
        let mut slots = [Pending::EMPTY; 1];
        let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
        let mut output = [0; 1024];
        let mut tokens = [Token::EMPTY; 128];
        let request = client.initialize(&mut output).unwrap();
        let reply = initialize_result(request.id, unsupported);
        assert_eq!(
            client
                .receive(reply.as_bytes(), &mut tokens, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::UnsupportedVersion
        );
        assert_ne!(client.state(), ClientState::Ready);
        assert_eq!(client.protocol_version(), None);
        assert_eq!(client.pending_count(), 1);
        assert!(client.initialized(&mut output).is_err());
    }
}

#[test]
fn legacy_june_tools_profile_negotiates_without_replaying_initialize() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    assert_eq!(client.protocol_version(), None);
    let initialize = client.initialize(&mut output).unwrap();
    assert!(
        parse(&output[..initialize.len], &mut tokens)
            .get("params")
            .unwrap()
            .get("protocolVersion")
            .unwrap()
            .string()
            .unwrap()
            .matches("2025-11-25")
    );
    let reply = initialize_result(initialize.id, "2025-06-18");
    client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    assert_eq!(client.protocol_version(), Some("2025-06-18"));
    assert_eq!(client.pending_count(), 0);
    assert_eq!(client.state(), ClientState::AwaitInitialized);
    client.initialized(&mut output).unwrap();
    let call = client.call_tool("echo", None, &mut output).unwrap();
    assert!(call.id > initialize.id);
    let reply = response(call.id, r#"{"content":[],"structuredContent":{"value":7}}"#);
    let Event::ToolResult { result, .. } = client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap()
    else {
        panic!("expected tool result");
    };
    assert!(!result.is_error());
    assert_eq!(client.protocol_version(), Some("2025-06-18"));
}

#[test]
fn pending_slots_and_request_buffers_are_bounded() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    assert_eq!(
        client.list_tools(None, &mut []).unwrap_err().kind,
        ErrorKind::Buffer
    );
    assert_eq!(client.pending_count(), 0);
    let request = client.list_tools(None, &mut output).unwrap();
    assert!(client.discover(&mut output).is_err());
    assert_eq!(client.pending_count(), 1);
    let reply = response(request.id, r#"{"resultType":"complete","tools":[]}"#);
    assert!(
        matches!(client.receive(reply.as_bytes(), &mut tokens, Limits::default()).unwrap(), Event::Tools { id, .. } if id == request.id)
    );
    assert_eq!(client.pending_count(), 0);
    assert!(client.discover(&mut output).unwrap().id > request.id);
}

#[test]
fn numeric_and_string_response_ids_are_not_interchangeable() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let request = client.list_tools(None, &mut output).unwrap();
    for id in [
        format!("{}", request.id + 1000),
        format!("\"{}\"", request.id),
    ] {
        let reply = response(id, r#"{"tools":[]}"#);
        assert!(
            client
                .receive(reply.as_bytes(), &mut tokens, Limits::default())
                .is_err()
        );
        assert_eq!(client.pending_count(), 1);
    }
    let reply = response(request.id, r#"{"tools":[]}"#);
    assert!(matches!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .unwrap(),
        Event::Tools { .. }
    ));
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn malformed_tool_pages_do_not_consume_the_pending_request() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let request = client.list_tools(None, &mut output).unwrap();
    for invalid in [
        r#"{}"#,
        r#"{"tools":{}}"#,
        r#"{"tools":[],"nextCursor":7}"#,
        r#"{"tools":[{"name":"missing_schema"}]}"#,
    ] {
        let reply = response(request.id, invalid);
        assert!(
            client
                .receive(reply.as_bytes(), &mut tokens, Limits::default())
                .is_err(),
            "accepted {invalid}"
        );
        assert_eq!(client.pending_count(), 1);
    }
    let reply = response(request.id, r#"{"tools":[]}"#);
    assert!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .is_ok()
    );
}

#[test]
fn malformed_tool_results_do_not_complete_calls() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let request = client.call_tool("echo", None, &mut output).unwrap();
    for invalid in [
        r#"{"structuredContent":{}}"#,
        r#"{"content":{}}"#,
        r#"{"content":[],"isError":"yes"}"#,
        r#"{"content":[{"type":"text"}]}"#,
    ] {
        let reply = response(request.id, invalid);
        assert!(
            client
                .receive(reply.as_bytes(), &mut tokens, Limits::default())
                .is_err(),
            "accepted {invalid}"
        );
        assert_eq!(client.pending_count(), 1);
    }
    let reply = response(request.id, r#"{"content":[],"structuredContent":null}"#);
    assert!(matches!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .unwrap(),
        Event::ToolResult { .. }
    ));
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn response_parse_capacity_failure_keeps_the_request_pending() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let request = client.list_tools(None, &mut output).unwrap();
    let reply = response(request.id, r#"{"tools":[]}"#);
    assert_eq!(
        client
            .receive(reply.as_bytes(), &mut [], Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Tokens
    );
    assert_eq!(client.pending_count(), 1);
    assert!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .is_ok()
    );
}

#[test]
fn notifications_and_uncorrelated_errors_leave_pending_work_intact() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    client.list_tools(None, &mut output).unwrap();
    let notification = br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;
    assert!(matches!(
        client
            .receive(notification, &mut tokens, Limits::default())
            .unwrap(),
        Event::Notification(_)
    ));
    assert_eq!(client.pending_count(), 1);
    for failure in [
        br#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"bad json"}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad json"}}"#,
    ] {
        assert!(matches!(
            client
                .receive(failure, &mut tokens, Limits::default())
                .unwrap(),
            Event::Failure { id: None, .. }
        ));
        assert_eq!(client.pending_count(), 1);
    }
}

#[test]
fn duplicate_responses_do_not_consume_another_call() {
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let first = client.list_tools(None, &mut output).unwrap();
    let second = client.list_tools(None, &mut output).unwrap();
    let reply = response(first.id, r#"{"tools":[]}"#);
    client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    assert_eq!(client.pending_count(), 1);
    assert!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .is_err()
    );
    assert_eq!(client.pending_count(), 1);
    let reply = response(second.id, r#"{"tools":[]}"#);
    client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn out_of_order_responses_are_decoded_for_their_request_kind() {
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let discover = client.discover(&mut output).unwrap();
    let call = client.call_tool("echo", None, &mut output).unwrap();
    let reply = response(call.id, r#"{"resultType":"complete","content":[]}"#);
    assert!(
        matches!(client.receive(reply.as_bytes(), &mut tokens, Limits::default()).unwrap(), Event::ToolResult { id, .. } if id == call.id)
    );
    assert_eq!(client.pending_count(), 1);
    let reply = response(
        discover.id,
        r#"{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"}"#,
    );
    assert!(
        matches!(client.receive(reply.as_bytes(), &mut tokens, Limits::default()).unwrap(), Event::Discovered { id, .. } if id == discover.id)
    );
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn input_required_is_not_a_final_tool_result_or_an_automatic_retry() {
    let mut cap_tokens = [Token::EMPTY; 16];
    let caps = parse(br#"{"elicitation":{"form":{}}}"#, &mut cap_tokens);
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots)
        .with_client_capabilities(caps).unwrap();
    let mut output = [0; 2048];
    let mut tokens = [Token::EMPTY; 256];
    let call = client.call_tool("confirm", None, &mut output).unwrap();
    let reply = response(
        call.id,
        r#"{"resultType":"input_required","inputRequests":{"confirm":{"method":"elicitation/create","params":{"mode":"form","message":"Confirm?","requestedSchema":{"type":"object"}}}},"requestState":"opaque"}"#,
    );
    assert!(
        matches!(client.receive(reply.as_bytes(), &mut tokens, Limits::default()).unwrap(), Event::InputRequired { id, .. } if id == call.id)
    );
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn unknown_result_type_is_rejected_without_completing_the_call() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let call = client.call_tool("echo", None, &mut output).unwrap();
    let reply = response(call.id, r#"{"resultType":"future-extension","content":[]}"#);
    assert_eq!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedResult
    );
    assert_eq!(client.pending_count(), 1);
    let reply = response(call.id, r#"{"content":[]}"#);
    assert!(matches!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .unwrap(),
        Event::ToolResult { .. }
    ));
}

#[test]
fn rpc_failure_and_tool_execution_error_are_different_events() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let call = client.call_tool("echo", None, &mut output).unwrap();
    let reply = response(
        call.id,
        r#"{"content":[{"type":"text","text":"tool failed"}],"isError":true}"#,
    );
    let Event::ToolResult { result, .. } = client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap()
    else {
        panic!("expected tool execution error result");
    };
    assert!(result.is_error());
    let call = client.call_tool("missing", None, &mut output).unwrap();
    let failure = format!(
        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":-32602,"message":"Unknown tool"}}}}"#,
        call.id
    );
    assert!(
        matches!(client.receive(failure.as_bytes(), &mut tokens, Limits::default()).unwrap(), Event::Failure { id: Some(id), error } if id == call.id && error.code == -32602)
    );
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn pagination_cursor_is_an_opaque_escaped_string() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 2048];
    let mut tokens = [Token::EMPTY; 128];
    let cursor = "page \"二\"\\token\n";
    let request = client.list_tools(Some(cursor), &mut output).unwrap();
    let parsed = parse(&output[..request.len], &mut tokens);
    assert!(
        parsed
            .get("params")
            .unwrap()
            .get("cursor")
            .unwrap()
            .string()
            .unwrap()
            .matches(cursor)
    );
    assert!(!output[..request.len].contains(&b'\n'));
    let reply = response(request.id, r#"{"tools":[],"nextCursor":"\u4e8c\"next"}"#);
    assert!(matches!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .unwrap(),
        Event::Tools { .. }
    ));
}

#[test]
fn cancellation_encoding_is_transactional_and_abandon_is_explicit() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let call = client.call_tool("echo", None, &mut output).unwrap();
    assert_eq!(
        client
            .cancel(call.id, Some("stop"), &mut [])
            .unwrap_err()
            .kind,
        ErrorKind::Buffer
    );
    assert_eq!(client.pending_count(), 1);
    let n = client.cancel(call.id, Some("stop"), &mut output).unwrap();
    let notice = parse(&output[..n], &mut tokens);
    assert!(notice.get("id").is_none());
    assert!(
        notice
            .get("method")
            .unwrap()
            .string()
            .unwrap()
            .matches("notifications/cancelled")
    );
    assert_eq!(
        notice
            .get("params")
            .unwrap()
            .get("requestId")
            .unwrap()
            .integer()
            .unwrap(),
        call.id
    );
    assert_eq!(client.pending_count(), 1);
    client.abandon(call.id).unwrap();
    assert_eq!(client.pending_count(), 0);
    let late = response(call.id, r#"{"content":[]}"#);
    assert!(
        client
            .receive(late.as_bytes(), &mut tokens, Limits::default())
            .is_err()
    );
    assert!(client.cancel(call.id, None, &mut output).is_err());
    assert!(client.call_tool("echo", None, &mut output).unwrap().id > call.id);
}

#[test]
fn legacy_initialize_cannot_be_cancelled() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
    let mut output = [0; 1024];
    let initialize = client.initialize(&mut output).unwrap();
    assert!(client.cancel(initialize.id, None, &mut output).is_err());
    assert_eq!(client.pending_count(), 1);
    assert_eq!(client.state(), ClientState::Initializing);
}

#[test]
fn non_object_arguments_fail_before_allocating_a_request_slot() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 8];
    let argument = parse(b"[1]", &mut tokens);
    assert!(
        client
            .call_tool("echo", Some(argument), &mut output)
            .is_err()
    );
    assert_eq!(client.pending_count(), 0);
    assert!(client.call_tool("", None, &mut output).is_err());
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn progress_validation_is_correlated_monotonic_and_transactional() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 2048];
    let mut tokens = [Token::EMPTY; 128];
    let call = client
        .call_tool_with_progress("work", None, &mut output)
        .unwrap();
    assert_eq!(
        parse(&output[..call.len], &mut tokens)
            .get("params")
            .unwrap()
            .get("_meta")
            .unwrap()
            .get("progressToken")
            .unwrap()
            .integer()
            .unwrap(),
        call.id
    );
    let progress = |token: String, extra: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","method":"notifications/progress","params":{{"progressToken":{token},{extra}}}}}"#
        )
    };
    for value in [0, 1] {
        let notice = progress(call.id.to_string(), &format!(r#""progress":{value}"#));
        assert!(matches!(
            client
                .receive(notice.as_bytes(), &mut tokens, Limits::default())
                .unwrap(),
            Event::Notification(_)
        ));
    }
    for extra in [
        r#""progress":1"#,
        r#""progress":0.5"#,
        r#""progress":-1"#,
        r#""progress":2,"total":-1"#,
        r#""progress":2,"message":false"#,
        r#""progress":1e999"#,
    ] {
        let notice = progress(call.id.to_string(), extra);
        assert!(
            client
                .receive(notice.as_bytes(), &mut tokens, Limits::default())
                .is_err(),
            "accepted {extra}"
        );
        assert_eq!(client.pending_count(), 1);
    }
    // A string token that looks like our numeric token is unrelated. It must
    // not set progress to 100 and make the next legitimate update look stale.
    let unrelated = progress(format!("\"{}\"", call.id), r#""progress":100"#);
    client
        .receive(unrelated.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    let valid = progress(call.id.to_string(), r#""progress":2,"total":10"#);
    client
        .receive(valid.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    assert_eq!(client.pending_count(), 1);
    let done = response(call.id, r#"{"content":[]}"#);
    client
        .receive(done.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn incoming_requests_are_legacy_only_and_do_not_complete_outgoing_requests() {
    for era in [Era::Modern, Era::Legacy] {
        let mut slots = [Pending::EMPTY; 1];
        let mut client = Client::new("test", "1", era, &mut slots);
        if era == Era::Legacy {
            legacy_ready(&mut client);
        }
        let mut output = [0; 1024];
        let mut tokens = [Token::EMPTY; 128];
        let outgoing = client.call_tool("echo", None, &mut output).unwrap();
        // Incoming request IDs are in the server's direction; even a matching
        // numeric ID cannot be mistaken for a response to our tools/call.
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"ping"}}"#,
            outgoing.id
        );
        let event = client.receive(request.as_bytes(), &mut tokens, Limits::default());
        if era == Era::Modern {
            assert_eq!(event.unwrap_err().kind, ErrorKind::InvalidRpc);
        } else {
            assert!(matches!(event.unwrap(), Event::ServerRequest(_)));
        }
        assert_eq!(client.pending_count(), 1);
    }
}

#[test]
fn malformed_discovery_does_not_change_cached_capabilities() {
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let discovery = client.discover(&mut output).unwrap();
    for invalid in [
        r#"{"supportedVersions":["2026-07-28"],"capabilities":{"tools":false}}"#,
        r#"{"supportedVersions":["2026-07-28",17],"capabilities":{}}"#,
        r#"{"supportedVersions":[],"capabilities":{}}"#,
        r#"{"supportedVersions":["2026-07-28"],"capabilities":{},"_meta":false}"#,
    ] {
        let reply = response(discovery.id, invalid);
        assert!(
            client
                .receive(reply.as_bytes(), &mut tokens, Limits::default())
                .is_err()
        );
        assert_eq!(client.pending_count(), 1);
        let probe = client.call_tool("echo", None, &mut output).unwrap();
        client.abandon(probe.id).unwrap();
    }
    let reply = response(
        discovery.id,
        r#"{"supportedVersions":["2026-07-28"],"capabilities":{}}"#,
    );
    client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    assert_eq!(client.pending_count(), 0);
    assert!(client.list_tools(None, &mut output).is_err());
    assert!(client.call_tool("echo", None, &mut output).is_err());
    // Discovery remains available and may update changed server capabilities.
    let discovery = client.discover(&mut output).unwrap();
    let reply = response(
        discovery.id,
        r#"{"supportedVersions":["2026-07-28"],"capabilities":{"tools":{}}}"#,
    );
    client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    assert!(client.list_tools(None, &mut output).is_ok());
}

#[test]
fn malformed_initialize_does_not_mutate_lifecycle_or_capabilities() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let initialize = client.initialize(&mut output).unwrap();
    for invalid in [
        r#"{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":3,"version":"1"}}"#,
        r#"{"protocolVersion":"2025-11-25","capabilities":{"tools":[]},"serverInfo":{"name":"server","version":"1"}}"#,
    ] {
        let reply = response(initialize.id, invalid);
        assert!(
            client
                .receive(reply.as_bytes(), &mut tokens, Limits::default())
                .is_err()
        );
        assert_eq!(client.state(), ClientState::Initializing);
        assert_eq!(client.pending_count(), 1);
    }
    let reply = initialize_result(initialize.id, "2025-11-25");
    client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    client.initialized(&mut output).unwrap();
    assert!(client.call_tool("echo", None, &mut output).is_ok());
}

#[test]
fn legacy_server_without_tools_is_restricted_but_ping_remains_available() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let initialize = client.initialize(&mut output).unwrap();
    let reply = response(
        initialize.id,
        r#"{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"server","version":"1"}}"#,
    );
    client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap();
    client.initialized(&mut output).unwrap();
    assert!(client.list_tools(None, &mut output).is_err());
    assert!(client.call_tool("echo", None, &mut output).is_err());
    let ping = client.ping(&mut output).unwrap();
    let reply = response(ping.id, "{}");
    assert!(matches!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .unwrap(),
        Event::Pong { id } if id == ping.id
    ));
}

#[test]
fn initialize_rpc_failure_can_be_retried_with_a_fresh_id() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let initialize = client.initialize(&mut output).unwrap();
    let failure = format!(
        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":-32603,"message":"Unavailable"}}}}"#,
        initialize.id
    );
    assert!(matches!(
        client
            .receive(failure.as_bytes(), &mut tokens, Limits::default())
            .unwrap(),
        Event::Failure { id: Some(id), .. } if id == initialize.id
    ));
    assert_eq!(client.state(), ClientState::New);
    assert_eq!(client.pending_count(), 0);
    assert!(client.initialize(&mut output).unwrap().id > initialize.id);
}

#[test]
fn close_discards_pending_requests_and_permanently_rejects_io() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let call = client.call_tool("echo", None, &mut output).unwrap();
    client.close();
    assert_eq!(client.state(), ClientState::Closed);
    assert_eq!(client.pending_count(), 0);
    assert!(client.discover(&mut output).is_err());
    assert!(client.list_tools(None, &mut output).is_err());
    assert!(client.call_tool("echo", None, &mut output).is_err());
    assert!(client.cancel(call.id, None, &mut output).is_err());
    assert!(client.abandon(call.id).is_err());
    let reply = response(call.id, r#"{"content":[]}"#);
    assert!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .is_err()
    );
    client.close();
    assert_eq!(client.state(), ClientState::Closed);
}

#[test]
fn invalid_input_required_does_not_consume_pending_request() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let call = client.call_tool("echo", None, &mut output).unwrap();
    for invalid in [
        r#"{"resultType":"input_required"}"#,
        r#"{"resultType":"input_required","requestState":null}"#,
        r#"{"resultType":"input_required","inputRequests":[]}"#,
        r#"{"resultType":"input_required","inputRequests":{},"requestState":7}"#,
    ] {
        let reply = response(call.id, invalid);
        assert!(
            client
                .receive(reply.as_bytes(), &mut tokens, Limits::default())
                .is_err(),
            "accepted {invalid}"
        );
        assert_eq!(client.pending_count(), 1);
    }
    // State-only input_required is valid load shedding, but never an auto-retry.
    let reply = response(
        call.id,
        r#"{"resultType":"input_required","requestState":"opaque"}"#,
    );
    assert!(matches!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .unwrap(),
        Event::InputRequired { id, .. } if id == call.id
    ));
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn tool_names_are_compared_after_json_unescaping() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let list = client.list_tools(None, &mut output).unwrap();
    let reply = response(
        list.id,
        r#"{"tools":[{"name":"echo","inputSchema":{"type":"object"}},{"name":"\u0065cho","inputSchema":{"type":"object"}}]}"#,
    );
    assert_eq!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::DuplicateKey
    );
    assert_eq!(client.pending_count(), 1);
}

#[test]
fn legacy_structured_tool_content_must_be_an_object() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
    legacy_ready(&mut client);
    let mut output = [0; 1024];
    let mut tokens = [Token::EMPTY; 128];
    let call = client.call_tool("echo", None, &mut output).unwrap();
    let reply = response(call.id, r#"{"content":[],"structuredContent":7}"#);
    assert!(
        client
            .receive(reply.as_bytes(), &mut tokens, Limits::default())
            .is_err()
    );
    assert_eq!(client.pending_count(), 1);
    let reply = response(call.id, r#"{"content":[],"structuredContent":{"value":7}}"#);
    let Event::ToolResult { result, .. } = client
        .receive(reply.as_bytes(), &mut tokens, Limits::default())
        .unwrap()
    else {
        panic!("expected tool result");
    };
    assert!(!result.is_error());
    assert_eq!(
        result
            .structured_content()
            .unwrap()
            .get("value")
            .unwrap()
            .integer()
            .unwrap(),
        7
    );
}

#[test]
fn client_and_server_round_trip_tools_in_both_eras() {
    for era in [Era::Modern, Era::Legacy] {
        let mut slots = [Pending::EMPTY; 1];
        let mut client = Client::new("test-client", "1", era, &mut slots);
        let mut server = Server::new("test-server", "2");
        let mut request = [0; 2048];
        let mut reply = [0; 2048];
        let mut client_tokens = [Token::EMPTY; 128];
        let mut server_tokens = [Token::EMPTY; 128];
        let mut schema_tokens = [Token::EMPTY; 8];
        let schema = parse(br#"{"type":"object"}"#, &mut schema_tokens);
        let tool = Tool::new("echo", "Returns a message", schema).unwrap();
        let start = if era == Era::Legacy {
            client.initialize(&mut request).unwrap()
        } else {
            client.discover(&mut request).unwrap()
        };
        let Action::Reply(n) = server
            .handle(
                &request[..start.len],
                &mut server_tokens,
                Limits::default(),
                &mut reply,
            )
            .unwrap()
        else {
            panic!("expected handshake/discovery reply");
        };
        let event = client
            .receive(&reply[..n], &mut client_tokens, Limits::default())
            .unwrap();
        if era == Era::Legacy {
            assert!(matches!(event, Event::Initialized { .. }));
            let n = client.initialized(&mut request).unwrap();
            assert!(matches!(
                server
                    .handle(
                        &request[..n],
                        &mut server_tokens,
                        Limits::default(),
                        &mut reply,
                    )
                    .unwrap(),
                Action::Notification(_)
            ));
        } else {
            assert!(matches!(event, Event::Discovered { .. }));
        }
        let list = client.list_tools(None, &mut request).unwrap();
        let Action::ListTools { context, .. } = server
            .handle(
                &request[..list.len],
                &mut server_tokens,
                Limits::default(),
                &mut reply,
            )
            .unwrap()
        else {
            panic!("expected tool list action");
        };
        let n = server
            .list_tools(context, &[tool], None, &mut reply)
            .unwrap();
        let Event::Tools { page, .. } = client
            .receive(&reply[..n], &mut client_tokens, Limits::default())
            .unwrap()
        else {
            panic!("expected tool page");
        };
        {
            let mut tools = page.tools();
            assert!(tools.next().unwrap().name().matches("echo"));
            assert!(tools.next().is_none());
        }
        let call = client.call_tool("echo", None, &mut request).unwrap();
        let Action::CallTool(action) = server
            .handle(
                &request[..call.len],
                &mut server_tokens,
                Limits::default(),
                &mut reply,
            )
            .unwrap()
        else {
            panic!("expected tool call action");
        };
        assert!(action.name.matches("echo"));
        let n = server
            .tool_result(
                action.context,
                ToolResult::new(&[Content::Text("hello")]),
                &mut reply,
            )
            .unwrap();
        let Event::ToolResult { result, .. } = client
            .receive(&reply[..n], &mut client_tokens, Limits::default())
            .unwrap()
        else {
            panic!("expected tool result");
        };
        assert!(!result.is_error());
        assert!(
            result
                .content()
                .elements()
                .next()
                .unwrap()
                .get("text")
                .unwrap()
                .string()
                .unwrap()
                .matches("hello")
        );
        assert_eq!(client.pending_count(), 0);
    }
}
