use zmcpio::{
    ErrorKind,
    client::{Client, Event, Pending},
    extensions::{self, Method, Subscription},
    json::{Document, Limits, Token, Value},
    mcp::{Action, Era, Server},
    rpc::{self, Id, Message, Notification},
};

fn parse<'a>(input: &'a [u8], tokens: &'a mut [Token]) -> Value<'a> {
    Document::parse(input, tokens, Limits::default()).unwrap().root()
}

fn modern(method: &str, fields: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}{fields}}}}}"#
    )
}

fn notification<'a>(input: &'a [u8], tokens: &'a mut [Token]) -> Notification<'a> {
    let Message::Notification(value) = rpc::parse(parse(input, tokens)).unwrap() else {
        panic!("expected notification");
    };
    value
}

#[test]
fn method_names_are_unique_and_roundtrip() {
    for (index, method) in Method::ALL.iter().enumerate() {
        assert!(!method.as_str().is_empty());
        for prior in &Method::ALL[..index] {
            assert_ne!(method.as_str(), prior.as_str());
        }
        let raw = format!("\"{}\"", method.as_str());
        let mut tokens = [Token::EMPTY; 2];
        assert_eq!(Method::parse(parse(raw.as_bytes(), &mut tokens).string().unwrap()), Some(*method));
    }
}

#[test]
fn resources_roundtrip_retains_full_result_and_metadata() {
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut server = Server::new("server", "1").with_extensions(&[Method::ResourcesRead]);
    let mut param_tokens = [Token::EMPTY; 32];
    let params = parse(br#"{"uri":"file:///hello","_meta":{"example.com/trace":"opaque"}}"#, &mut param_tokens);
    let mut request = [0; 2048];
    let outgoing = client.request(Method::ResourcesRead, Some(params), &mut request).unwrap();
    let mut request_tokens = [Token::EMPTY; 128];
    let mut response = [0; 2048];
    let Action::Extension { context, method } = server
        .handle(&request[..outgoing.len], &mut request_tokens, Limits::default(), &mut response)
        .unwrap() else { panic!("expected extension action"); };
    assert_eq!(method, Method::ResourcesRead);
    let meta = context.request.params.unwrap().get("_meta").unwrap();
    assert!(meta.get("example.com/trace").unwrap().string().unwrap().matches("opaque"));
    let mut result_tokens = [Token::EMPTY; 64];
    let result = parse(
        br#"{"contents":[{"uri":"file:///hello","text":"hi"}],"_meta":{"example.com/private":"retained"}}"#,
        &mut result_tokens,
    );
    let n = server.extension_result(context, result, &mut response).unwrap();
    let mut reply_tokens = [Token::EMPTY; 128];
    let Event::ExtensionResult { id, method, result } = client
        .receive(&response[..n], &mut reply_tokens, Limits::default()).unwrap()
        else { panic!("expected extension result"); };
    assert_eq!(id, outgoing.id);
    assert_eq!(method, Method::ResourcesRead);
    assert!(result.get("resultType").unwrap().string().unwrap().matches("complete"));
    assert!(result.get("_meta").unwrap().get("example.com/private").is_some());
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn opt_in_is_required_and_callbacks_are_not_server_methods() {
    let mut server = Server::new("server", "1");
    let mut tokens = [Token::EMPTY; 128];
    let mut output = [0; 1024];
    let raw = modern("resources/list", "");
    let Action::Reply(n) = server.handle(raw.as_bytes(), &mut tokens, Limits::default(), &mut output).unwrap()
        else { panic!("expected error"); };
    assert_eq!(parse(&output[..n], &mut tokens).get("error").unwrap().get("code").unwrap().integer().unwrap(), -32601);

    let mut server = server.with_extensions(&[Method::RootsList]);
    let raw = modern("roots/list", "");
    let Action::Reply(n) = server.handle(raw.as_bytes(), &mut tokens, Limits::default(), &mut output).unwrap()
        else { panic!("expected error"); };
    assert_eq!(parse(&output[..n], &mut tokens).get("error").unwrap().get("code").unwrap().integer().unwrap(), -32601);
}

#[test]
fn extension_buffer_failure_reserves_neither_slot_nor_id() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut short = [0; 8];
    assert_eq!(client.request(Method::ResourcesList, None, &mut short).unwrap_err().kind, ErrorKind::Buffer);
    assert_eq!(client.pending_count(), 0);
    let mut output = [0; 1024];
    assert_eq!(client.request(Method::ResourcesList, None, &mut output).unwrap().id, 1);
}

#[test]
fn reserved_metadata_cannot_be_overridden() {
    for key in [
        "io.modelcontextprotocol/protocolVersion",
        "io.modelcontextprotocol/clientCapabilities",
        "io.modelcontextprotocol/clientInfo",
    ] {
        let raw = format!(r#"{{"_meta":{{"{key}":{{}}}}}}"#);
        let mut tokens = [Token::EMPTY; 16];
        let params = parse(raw.as_bytes(), &mut tokens);
        let mut slots = [Pending::EMPTY; 1];
        let mut client = Client::new("test", "1", Era::Modern, &mut slots);
        let mut output = [0; 1024];
        assert_eq!(client.request(Method::ResourcesList, Some(params), &mut output).unwrap_err().kind, ErrorKind::InvalidParams);
        assert_eq!(client.pending_count(), 0);
    }
}

#[test]
fn only_correct_direction_and_era_can_be_encoded() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    for method in [Method::RootsList, Method::SamplingCreateMessage, Method::ElicitationCreate,
        Method::ResourcesSubscribe, Method::ResourcesUnsubscribe, Method::LoggingSetLevel] {
        assert_eq!(client.request(method, None, &mut output).unwrap_err().kind, ErrorKind::State);
    }
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn input_required_is_rejected_for_list_and_discovery_without_losing_request() {
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    let discover = client.discover(&mut output).unwrap();
    let list = client.request(Method::ResourcesList, None, &mut output).unwrap();
    let mut tokens = [Token::EMPTY; 64];
    for id in [discover.id, list.id] {
        let reply = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"resultType":"input_required","requestState":"x"}}}}"#);
        assert_eq!(client.receive(reply.as_bytes(), &mut tokens, Limits::default()).unwrap_err().kind, ErrorKind::UnsupportedResult);
    }
    assert_eq!(client.pending_count(), 2);
}

#[test]
fn malformed_extension_result_does_not_complete_pending_request() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut output = [0; 1024];
    client.request(Method::ResourcesList, None, &mut output).unwrap();
    let mut tokens = [Token::EMPTY; 64];
    assert!(client.receive(br#"{"jsonrpc":"2.0","id":1,"result":{"resources":"bad"}}"#, &mut tokens, Limits::default()).is_err());
    assert_eq!(client.pending_count(), 1);
    assert!(client.receive(br#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","resources":[]}}"#, &mut tokens, Limits::default()).is_ok());
    assert_eq!(client.pending_count(), 0);
}

#[test]
fn explicit_mrtr_roundtrip_preserves_opaque_state_and_uses_new_id() {
    let mut cap_tokens = [Token::EMPTY; 16];
    let caps = parse(br#"{"elicitation":{"form":{}}}"#, &mut cap_tokens);
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots)
        .with_client_capabilities(caps).unwrap();
    let mut server = Server::new("server", "1");
    let mut request = [0; 2048];
    let initial = client.call_tool("confirm", None, &mut request).unwrap();
    let mut request_tokens = [Token::EMPTY; 128];
    let mut response = [0; 2048];
    let Action::CallTool(call) = server.handle(&request[..initial.len], &mut request_tokens, Limits::default(), &mut response).unwrap()
        else { panic!("expected tool call"); };
    let mut input_tokens = [Token::EMPTY; 64];
    let inputs = parse(br#"{"confirm":{"method":"elicitation/create","params":{"message":"Continue?","requestedSchema":{"type":"object"}}}}"#, &mut input_tokens);
    let n = server.input_required(call.context, Some(inputs), Some("opaque\nstate"), &mut response).unwrap();
    let mut reply_tokens = [Token::EMPTY; 128];
    assert!(matches!(client.receive(&response[..n], &mut reply_tokens, Limits::default()).unwrap(), Event::InputRequired { .. }));
    let mut retry_tokens = [Token::EMPTY; 64];
    let retry = parse(br#"{"name":"confirm","requestState":"opaque\nstate","inputResponses":{"confirm":{"action":"accept","content":{}}}}"#, &mut retry_tokens);
    let next = client.request(Method::ToolsCall, Some(retry), &mut request).unwrap();
    assert_ne!(initial.id, next.id);
    let parsed = parse(&request[..next.len], &mut request_tokens);
    assert!(parsed.get("params").unwrap().get("requestState").unwrap().string().unwrap().matches("opaque\nstate"));
}

#[test]
fn server_cannot_request_unadvertised_callbacks_or_mrtr_on_lists() {
    let raw = modern("tools/call", r#","name":"confirm""#);
    let mut server = Server::new("server", "1").with_extensions(&[Method::ResourcesList]);
    let mut tokens = [Token::EMPTY; 128];
    let mut output = [0; 2048];
    let Action::CallTool(call) = server.handle(raw.as_bytes(), &mut tokens, Limits::default(), &mut output).unwrap()
        else { panic!("expected call"); };
    let mut input_tokens = [Token::EMPTY; 64];
    let inputs = parse(br#"{"roots":{"method":"roots/list"}}"#, &mut input_tokens);
    assert_eq!(server.input_required(call.context, Some(inputs), None, &mut output).unwrap_err().kind, ErrorKind::State);
    let raw = modern("resources/list", "");
    let Action::Extension { context, .. } = server.handle(raw.as_bytes(), &mut tokens, Limits::default(), &mut output).unwrap()
        else { panic!("expected list"); };
    assert_eq!(server.input_required(context, None, Some("opaque"), &mut output).unwrap_err().kind, ErrorKind::State);
}

#[test]
fn subscription_acknowledgment_order_filter_and_stream_are_enforced() {
    let mut filter_tokens = [Token::EMPTY; 32];
    let requested = parse(br#"{"toolsListChanged":true,"resourceSubscriptions":["file:///one"]}"#, &mut filter_tokens);
    let mut subscription = Subscription::new(Id::Number(7), requested).unwrap();
    let mut change_tokens = [Token::EMPTY; 64];
    let change = notification(br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{"_meta":{"io.modelcontextprotocol/subscriptionId":7}}}"#, &mut change_tokens);
    assert_eq!(subscription.observe(change).unwrap_err().kind, ErrorKind::State);
    let mut ack_tokens = [Token::EMPTY; 64];
    let ack = notification(br#"{"jsonrpc":"2.0","method":"notifications/subscriptions/acknowledged","params":{"_meta":{"io.modelcontextprotocol/subscriptionId":7},"notifications":{"toolsListChanged":true,"resourceSubscriptions":["file:///one"]}}}"#, &mut ack_tokens);
    subscription.observe(ack).unwrap();
    assert!(subscription.acknowledged());
    subscription.observe(change).unwrap();
    let mut log_tokens = [Token::EMPTY; 64];
    let log = notification(br#"{"jsonrpc":"2.0","method":"notifications/message","params":{"_meta":{"io.modelcontextprotocol/subscriptionId":7},"level":"info","data":"x"}}"#, &mut log_tokens);
    assert_eq!(subscription.observe(log).unwrap_err().kind, ErrorKind::InvalidParams);
    let mut other_tokens = [Token::EMPTY; 64];
    let other = notification(br#"{"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"_meta":{"io.modelcontextprotocol/subscriptionId":7},"uri":"file:///two"}}"#, &mut other_tokens);
    assert!(subscription.observe(other).is_err());
    let mut wrong_tokens = [Token::EMPTY; 64];
    let wrong = notification(br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{"_meta":{"io.modelcontextprotocol/subscriptionId":8}}}"#, &mut wrong_tokens);
    assert_eq!(subscription.observe(wrong).unwrap_err().kind, ErrorKind::UnknownId);
}

#[test]
fn subscription_cannot_acknowledge_more_than_requested() {
    let mut filter_tokens = [Token::EMPTY; 16];
    let requested = parse(br#"{"toolsListChanged":true}"#, &mut filter_tokens);
    let mut subscription = Subscription::new(Id::Number(1), requested).unwrap();
    let mut ack_tokens = [Token::EMPTY; 64];
    let ack = notification(br#"{"jsonrpc":"2.0","method":"notifications/subscriptions/acknowledged","params":{"_meta":{"io.modelcontextprotocol/subscriptionId":1},"notifications":{"promptsListChanged":true}}}"#, &mut ack_tokens);
    assert!(subscription.observe(ack).is_err());
    assert!(!subscription.acknowledged());
}

#[test]
fn subscription_final_response_must_match_its_id() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Modern, &mut slots);
    let mut param_tokens = [Token::EMPTY; 16];
    let params = parse(br#"{"notifications":{"toolsListChanged":true}}"#, &mut param_tokens);
    let mut output = [0; 1024];
    client.request(Method::SubscriptionsListen, Some(params), &mut output).unwrap();
    let mut tokens = [Token::EMPTY; 64];
    assert!(client.receive(br#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","_meta":{"io.modelcontextprotocol/subscriptionId":2}}}"#, &mut tokens, Limits::default()).is_err());
    assert_eq!(client.pending_count(), 1);
    assert!(client.receive(br#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","_meta":{"io.modelcontextprotocol/subscriptionId":1}}}"#, &mut tokens, Limits::default()).is_ok());
}

#[test]
fn legacy_callbacks_require_explicit_capability() {
    let mut empty_tokens = [Token::EMPTY; 8];
    let empty = parse(b"{}", &mut empty_tokens);
    let mut caps_tokens = [Token::EMPTY; 8];
    let caps = parse(br#"{"roots":{}}"#, &mut caps_tokens);
    let mut output = [0; 1024];
    assert!(extensions::callback_request(Id::Number(4), Method::RootsList, None, empty, &mut output).is_err());
    let n = extensions::callback_request(Id::Number(4), Method::RootsList, None, caps, &mut output).unwrap();
    let mut tokens = [Token::EMPTY; 64];
    let Message::Request(request) = rpc::parse(parse(&output[..n], &mut tokens)).unwrap()
        else { panic!("expected callback"); };
    assert!(request.method.matches("roots/list"));
}

#[test]
fn old_sse_peer_versions_negotiate_and_call_without_batches() {
    for version in ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"] {
        let mut slots = [Pending::EMPTY; 1];
        let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
        let mut server = Server::new("server", "1");
        let mut output = [0; 2048];
        let sent = client.initialize(&mut output).unwrap();
        let request = format!(r#"{{"jsonrpc":"2.0","id":{},"method":"initialize","params":{{"protocolVersion":"{version}","capabilities":{{}},"clientInfo":{{"name":"peer","version":"1"}}}}}}"#, sent.id);
        let mut tokens = [Token::EMPTY; 128];
        let Action::Reply(n) = server.handle(request.as_bytes(), &mut tokens, Limits::default(), &mut output).unwrap()
            else { panic!("expected initialize"); };
        assert_eq!(server.legacy_protocol_version(), Some(version));
        let mut reply_tokens = [Token::EMPTY; 128];
        client.receive(&output[..n], &mut reply_tokens, Limits::default()).unwrap();
        assert_eq!(client.protocol_version(), Some(version));
        let n = client.initialized(&mut output).unwrap();
        let mut response = [0; 2048];
        server.handle(&output[..n], &mut tokens, Limits::default(), &mut response).unwrap();
        let sent = client.call_tool("echo", None, &mut output).unwrap();
        assert!(matches!(server.handle(&output[..sent.len], &mut tokens, Limits::default(), &mut response).unwrap(), Action::CallTool(_)));
    }
}

#[test]
fn opt_in_capabilities_are_advertised_without_invented_callbacks() {
    let methods = [Method::ResourcesList, Method::PromptsGet, Method::TasksList];
    let mut server = Server::new("test", "1").with_extensions(&methods);
    let raw = modern("server/discover", "");
    let mut tokens = [Token::EMPTY; 128];
    let mut output = [0; 2048];
    let Action::Reply(n) = server.handle(raw.as_bytes(), &mut tokens, Limits::default(), &mut output).unwrap()
        else { panic!("expected discovery"); };
    let result = parse(&output[..n], &mut tokens).get("result").unwrap();
    let caps = result.get("capabilities").unwrap();
    assert!(caps.get("resources").is_some());
    assert!(caps.get("prompts").is_some());
    assert!(caps.get("sampling").is_none());
    assert!(caps.get("extensions").unwrap().get("io.modelcontextprotocol/tasks").is_some());
    assert_eq!(result.get("supportedVersions").unwrap().elements().count(), 5);
}

#[test]
fn explicit_old_initialize_selection_and_invalid_revision_are_transactional() {
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("test", "1", Era::Legacy, &mut slots);
    let mut output = [0; 1024];
    assert_eq!(client.initialize_with_version("1900-01-01", &mut output).unwrap_err().kind, ErrorKind::UnsupportedVersion);
    assert_eq!(client.pending_count(), 0);
    let sent = client.initialize_with_version("2024-11-05", &mut output).unwrap();
    assert_eq!(sent.id, 1);
    let mut tokens = [Token::EMPTY; 64];
    assert!(parse(&output[..sent.len], &mut tokens).get("params").unwrap().get("protocolVersion").unwrap().string().unwrap().matches("2024-11-05"));
}

#[test]
fn callbacks_do_not_treat_form_capability_as_url_permission() {
    let mut cap_tokens = [Token::EMPTY; 16];
    let caps = parse(br#"{"elicitation":{"form":{}}}"#, &mut cap_tokens);
    let mut param_tokens = [Token::EMPTY; 32];
    let params = parse(br#"{"mode":"url","message":"Open?","url":"https://example.invalid","elicitationId":"request"}"#, &mut param_tokens);
    let mut output = [0; 1024];
    assert_eq!(extensions::callback_request(Id::Number(1), Method::ElicitationCreate, Some(params), caps, &mut output).unwrap_err().kind, ErrorKind::State);
}
