use zmcpio::{
    http::{
        self, BodyKind, Cancellation, ClientSession, Header, HttpError, Incoming, Method,
        Parameter, ResponseMode, SecurityPolicy, ServerAction, ServerConfig, Version,
    },
    json::{Document, Limits, Token},
    rpc::{self, Message},
};

const MODERN_LIST: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
const LEGACY_LIST: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;

fn message<'a>(bytes: &'a [u8], tokens: &'a mut [Token]) -> Message<'a> {
    rpc::parse(Document::parse(bytes, tokens, Limits::default()).unwrap().root()).unwrap()
}
fn headers(version: Version) -> Vec<Header<'static>> {
    vec![
        Header { name: "Content-Type", value: "application/json; charset=utf-8" },
        Header { name: "Accept", value: "application/json, text/event-stream" },
        Header { name: "MCP-Protocol-Version", value: version.as_str() },
        Header { name: "Mcp-Method", value: "tools/list" },
    ]
}
struct Policy { authorized: bool, origin: bool, session: bool }
const ALLOW: Policy = Policy { authorized: true, origin: true, session: true };
impl SecurityPolicy for Policy {
    fn origin_allowed(&self, _: &str) -> bool { self.origin }
    fn authorized(&self, _: &[Header<'_>]) -> bool { self.authorized }
    fn session_valid(&self, _: &str, _: &[Header<'_>]) -> bool { self.session }
}

#[test]
fn modern_post_and_media_negotiation() {
    let cfg = ServerConfig::stateless(Version::V2026_07_28);
    let mut tokens = [Token::EMPTY; 64];
    let msg = message(MODERN_LIST, &mut tokens);
    let mut hs = headers(cfg.version);
    let mut scratch = [0; 256];
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut scratch), Ok(ServerAction::Message { kind: BodyKind::Request }));
    hs[1].value = "application/json";
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut scratch), Err(HttpError::NotAcceptable));
    hs[1].value = "application/json, text/event-stream;q=0";
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut scratch), Err(HttpError::NotAcceptable));
    hs[1].value = "APPLICATION/JSON;q=1, TEXT/EVENT-STREAM;q=0.5";
    assert!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut scratch).is_ok());
    hs[0].value = "text/plain";
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut scratch), Err(HttpError::UnsupportedMediaType));
}
#[test]
fn modern_disallows_legacy_methods_and_ignores_sessions() {
    let cfg = ServerConfig { version: Version::V2026_07_28, allow_get: true, allow_delete: true, sessions: true };
    let mut scratch = [0; 32];
    for method in [Method::Get, Method::Delete] {
        assert_eq!(cfg.validate(Incoming { method, headers: &[], message: None }, &ALLOW, &mut scratch), Err(HttpError::MethodNotAllowed));
    }
    let mut hs = headers(cfg.version);
    hs.push(Header { name: "Mcp-Session-Id", value: "obsolete" });
    hs.push(Header { name: "Last-Event-ID", value: "obsolete-event" });
    let mut tokens = [Token::EMPTY; 64];
    let msg = message(MODERN_LIST, &mut tokens);
    let policy = Policy { authorized: true, origin: true, session: false };
    assert!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &policy, &mut scratch).is_ok());
    assert_eq!(cfg.version.cancellation(), Cancellation::CloseResponse);
    assert_eq!(Version::V2025_11_25.cancellation(), Cancellation::Notification);
}
#[test]
fn security_checks_precede_processing() {
    let cfg = ServerConfig::stateless(Version::V2026_07_28);
    let mut hs = headers(cfg.version);
    hs.push(Header { name: "Origin", value: "https://evil.invalid" });
    let mut tokens = [Token::EMPTY; 64];
    let msg = message(MODERN_LIST, &mut tokens);
    let mut scratch = [0; 64];
    let req = Incoming { method: Method::Post, headers: &hs, message: Some(msg) };
    let bad_origin = Policy { origin: false, authorized: true, session: true };
    assert_eq!(cfg.validate(req, &bad_origin, &mut scratch), Err(HttpError::Forbidden));
    let denied = Policy { origin: true, authorized: false, session: true };
    assert_eq!(cfg.validate(req, &denied, &mut scratch), Err(HttpError::Unauthorized));
    hs.push(Header { name: "Authorization", value: "Bearer a" });
    hs.push(Header { name: "authorization", value: "Bearer b" });
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut scratch), Err(HttpError::InvalidHeader));
}
#[test]
fn legacy_sessions_and_resume() {
    let cfg = ServerConfig { version: Version::V2025_11_25, allow_get: true, allow_delete: true, sessions: true };
    let mut hs = headers(cfg.version);
    let mut tokens = [Token::EMPTY; 64];
    let msg = message(LEGACY_LIST, &mut tokens);
    let mut scratch = [0; 64];
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut scratch), Err(HttpError::BadRequest));
    hs.push(Header { name: "Mcp-Session-Id", value: "session-one" });
    let bad = Policy { authorized: true, origin: true, session: false };
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &bad, &mut scratch), Err(HttpError::SessionExpired));
    hs.push(Header { name: "Last-Event-ID", value: "stream-4-event-2" });
    assert_eq!(cfg.validate(Incoming { method: Method::Get, headers: &hs, message: None }, &ALLOW, &mut scratch), Ok(ServerAction::Listen { last_event_id: Some("stream-4-event-2") }));
    assert_eq!(cfg.validate(Incoming { method: Method::Delete, headers: &hs, message: None }, &ALLOW, &mut scratch), Ok(ServerAction::Terminate { session_id: "session-one" }));
    hs[4].value = "bad session";
    assert_eq!(cfg.validate(Incoming { method: Method::Get, headers: &hs, message: None }, &ALLOW, &mut scratch), Err(HttpError::InvalidHeader));
}
#[test]
fn legacy_initialize_without_headers_or_session() {
    let cfg = ServerConfig { version: Version::V2025_11_25, allow_get: false, allow_delete: false, sessions: true };
    let hs = [Header { name: "Accept", value: http::ACCEPT }, Header { name: "Content-Type", value: http::JSON }];
    let mut tokens = [Token::EMPTY; 64];
    let msg = message(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#, &mut tokens);
    assert!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut [0; 64]).is_ok());
}
#[test]
fn notification_and_response_rules_are_era_specific() {
    let mut tokens = [Token::EMPTY; 32];
    let msg = message(br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#, &mut tokens);
    let cfg = ServerConfig::stateless(Version::V2026_07_28);
    let hs = headers(cfg.version);
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut []), Err(HttpError::BadRequest));
    let cfg = ServerConfig::stateless(Version::V2025_06_18);
    let hs = headers(cfg.version);
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut []), Ok(ServerAction::Message { kind: BodyKind::Notification }));
    let msg = message(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#, &mut tokens);
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut []), Ok(ServerAction::Message { kind: BodyKind::Response }));
    let cfg = ServerConfig::stateless(Version::V2026_07_28);
    let hs = headers(cfg.version);
    assert_eq!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut []), Err(HttpError::BadRequest));
}
#[test]
fn response_modes_and_error_status() {
    assert_eq!(http::response_mode(200, Some("APPLICATION/JSON; charset=utf-8"), BodyKind::Request), Ok(ResponseMode::Json));
    assert_eq!(http::response_mode(200, Some("text/event-stream"), BodyKind::Request), Ok(ResponseMode::Sse));
    assert_eq!(http::response_mode(202, None, BodyKind::Notification), Ok(ResponseMode::Empty));
    assert!(http::response_mode(202, None, BodyKind::Request).is_err());
    assert!(http::response_mode(200, None, BodyKind::Request).is_err());
    assert_eq!(http::rpc_error_status(Version::V2026_07_28, -32601), 404);
    assert_eq!(http::rpc_error_status(Version::V2026_07_28, -32020), 400);
    assert_eq!(http::rpc_error_status(Version::V2025_11_25, -32601), 200);
}
#[test]
fn session_storage_capacity_cleanup_and_no_replay() {
    let mut storage = [0; 12];
    let mut client = ClientSession::new(Version::V2025_11_25, &mut storage);
    client.set_session_id(Some("session-one")).unwrap();
    assert_eq!(client.set_session_id(Some("too-long-session-id")), Err(HttpError::Capacity { needed: 19 }));
    assert_eq!(client.session_id(), Some("session-one"));
    assert_eq!(client.set_session_id(Some("bad\r\nid")), Err(HttpError::InvalidHeader));
    assert!(!client.observe_status(404, Some("different-session")));
    assert_eq!(client.session_id(), Some("session-one"));
    assert!(client.observe_status(404, Some("session-one")));
    assert_eq!(client.session_id(), None);
    assert_eq!(storage, [0; 12]);
}
#[test]
fn client_header_emission() {
    let mut id = [0; 32];
    let mut client = ClientSession::new(Version::V2025_11_25, &mut id);
    client.set_session_id(Some("session-one")).unwrap();
    let mut out = [0; 512];
    let n = client.post_headers("initialize", None, &mut out).unwrap();
    assert!(!std::str::from_utf8(&out[..n]).unwrap().contains("Session"));
    let n = client.post_headers("tools/list", None, &mut out).unwrap();
    assert!(std::str::from_utf8(&out[..n]).unwrap().contains("Mcp-Session-Id: session-one\r\n"));
    let n = client.get_headers(Some("stream:42"), &mut out).unwrap();
    assert!(std::str::from_utf8(&out[..n]).unwrap().contains("Last-Event-ID: stream:42\r\n"));
    assert!(client.get_headers(Some("bad\r\nid"), &mut out).is_err());
    assert!(client.delete_headers(&mut out).is_ok());
    let mut modern_id = [0; 32];
    let mut modern = ClientSession::new(Version::V2026_07_28, &mut modern_id);
    modern.set_session_id(Some("ignored")).unwrap();
    let n = modern.post_headers("tools/call", Some("Hello, 世界"), &mut out).unwrap();
    let hs = std::str::from_utf8(&out[..n]).unwrap();
    assert!(hs.contains("Mcp-Name: =?base64?SGVsbG8sIOS4lueVjA==?=\r\n"));
    assert!(!hs.contains("Session"));
    assert!(modern.post_headers("tools/call", None, &mut out).is_err());
    assert!(modern.get_headers(None, &mut out).is_err());
    assert!(modern.delete_headers(&mut out).is_err());
    assert!(modern.post_headers("tools/list\r\nInjected: x", None, &mut out).is_err());
}
#[test]
fn sentinel_round_trip_and_canonical_padding() {
    for value in ["simple", "", " padded ", "\ttab", "hello\nworld", "世界", "=?base64?literal?=", "a\tb"] {
        let mut encoded = [0; 256];
        let mut decoded = [0; 256];
        let wire = http::encode_header_value(value, &mut encoded).unwrap();
        assert_eq!(http::decode_header_value(wire, &mut decoded).unwrap(), value);
    }
    for wire in ["=?base64?A?=", "=?base64?AB==?=", "=?base64?AAF=?=", "=?base64?AAAA====?=", "=?base64?AA=A?=", "=?base64?/w==?=", "bad\r\nwire"] {
        assert!(http::decode_header_value(wire, &mut [0; 64]).is_err(), "{wire}");
    }
    assert_eq!(http::encode_header_value("世界", &mut [0; 1]), Err(HttpError::Capacity { needed: 18 }));
}
#[test]
fn standard_headers_match_decoded_unicode_names() {
    let json = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"Hello, \u4e16\u754c","_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#;
    let mut tokens = [Token::EMPTY; 64];
    let Message::Request(request) = message(json, &mut tokens) else { panic!(); };
    let mut hs = headers(Version::V2026_07_28);
    hs[3].value = "tools/call";
    hs.push(Header { name: "mcp-name", value: "=?base64?SGVsbG8sIOS4lueVjA==?=" });
    assert!(http::validate_standard_headers(&hs, request, &mut [0; 128]).is_ok());
    hs[4].value = "wrong";
    assert_eq!(http::validate_standard_headers(&hs, request, &mut [0; 128]), Err(HttpError::HeaderMismatch));
    hs.pop();
    assert_eq!(http::validate_standard_headers(&hs, request, &mut [0; 128]), Err(HttpError::HeaderMismatch));
}
#[test]
fn duplicate_singleton_and_invalid_field_names_rejected() {
    let hs = [Header { name: "MCP-Protocol-Version", value: "2026-07-28" }, Header { name: "mcp-protocol-version", value: "2025-11-25" }];
    assert_eq!(http::header(&hs, "mcp-protocol-version"), Err(HttpError::InvalidHeader));
    let hs = [Header { name: "bad name", value: "x" }];
    assert_eq!(http::header(&hs, "absent"), Err(HttpError::InvalidHeader));
}

const SCHEMA: &[u8] = br#"{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"},"nested":{"type":"object","properties":{"count":{"type":"integer","x-mcp-header":"Count"},"ok":{"type":["boolean","null"],"x-mcp-header":"OK"}}},"nullable":{"type":["string","null"],"x-mcp-header":"Nil"}}}"#;
#[test]
fn custom_headers_nested_primitives_and_null() {
    let mut st = [Token::EMPTY; 128];
    let mut at = [Token::EMPTY; 64];
    let schema = Document::parse(SCHEMA, &mut st, Limits::default()).unwrap().root();
    let args = Document::parse(br#"{"region":" padded ","nested":{"count":42.0,"ok":true},"nullable":null}"#, &mut at, Limits::default()).unwrap().root();
    let mut slots = [Parameter::EMPTY; 8];
    let params = http::parameters(schema, Some(args), &mut slots).unwrap();
    assert_eq!(params.len(), 4);
    let mut out = [0; 512];
    let n = http::write_parameter_headers(params, &mut out, &mut [0; 256]).unwrap();
    let written = std::str::from_utf8(&out[..n]).unwrap();
    assert!(written.contains("Mcp-Param-Region: =?base64?IHBhZGRlZCA=?=\r\n"));
    assert!(written.contains("Mcp-Param-Count: 42\r\n"));
    assert!(written.contains("Mcp-Param-OK: true\r\n"));
    assert!(!written.contains("Nil"));
    let mut hs = vec![
        Header { name: "MCP-PARAM-REGION", value: "=?base64?IHBhZGRlZCA=?=" },
        Header { name: "Mcp-Param-Count", value: "4.2e1" },
        Header { name: "Mcp-Param-OK", value: "true" },
    ];
    assert!(http::validate_parameter_headers(params, &hs, &mut [0; 256]).is_ok());
    hs.push(Header { name: "Mcp-Param-Nil", value: "" });
    assert_eq!(http::validate_parameter_headers(params, &hs, &mut [0; 256]), Err(HttpError::HeaderMismatch));
    hs.pop();
    hs[1].value = "42.000000000000000001";
    assert_eq!(http::validate_parameter_headers(params, &hs, &mut [0; 256]), Err(HttpError::HeaderMismatch));
}
#[test]
fn custom_annotation_definition_rejections() {
    let invalid: &[&[u8]] = &[
        br#"{"type":"object","properties":{"a":{"type":"string","x-mcp-header":"A"},"b":{"type":"string","x-mcp-header":"a"}}}"#,
        br#"{"type":"object","properties":{"a":{"type":"string","x-mcp-header":"bad name"}}}"#,
        br#"{"type":"object","properties":{"a":{"type":"number","x-mcp-header":"A"}}}"#,
        br#"{"type":"object","properties":{"a":{"type":"object","x-mcp-header":"A"}}}"#,
        br#"{"type":"object","properties":{"a":{"type":"string","x-mcp-header":""}}}"#,
        br#"{"type":"object","allOf":[{"properties":{"a":{"type":"string","x-mcp-header":"A"}}}]}"#,
        br#"{"type":"object","properties":{"a":{"type":"array","items":{"type":"string","x-mcp-header":"A"}}}}"#,
        br#"{"type":"object","$defs":{"a":{"type":"string","x-mcp-header":"A"}}}"#,
    ];
    for json in invalid {
        let mut tokens = [Token::EMPTY; 128];
        let schema = Document::parse(json, &mut tokens, Limits::default()).unwrap().root();
        assert!(matches!(http::parameters(schema, None, &mut [Parameter::EMPTY; 16]), Err(HttpError::InvalidSchema)));
    }
}
#[test]
fn custom_safe_integer_is_exact_not_f64_rounded() {
    let mut st = [Token::EMPTY; 64];
    let schema = Document::parse(br#"{"type":"object","properties":{"n":{"type":"integer","x-mcp-header":"N"}}}"#, &mut st, Limits::default()).unwrap().root();
    for number in ["9007199254740991", "-9007199254740991", "42.0", "4.2e1", "100e-2", "-0", "0e1000000"] {
        let json = format!("{{\"n\":{number}}}");
        let mut at = [Token::EMPTY; 32];
        let args = Document::parse(json.as_bytes(), &mut at, Limits::default()).unwrap().root();
        assert!(http::parameters(schema, Some(args), &mut [Parameter::EMPTY; 1]).is_ok(), "{number}");
    }
    for number in ["9007199254740992", "-9007199254740992", "1.00000000000000001", "9007199254740991.1", "1e-100", "1e100", "1.5"] {
        let json = format!("{{\"n\":{number}}}");
        let mut at = [Token::EMPTY; 32];
        let args = Document::parse(json.as_bytes(), &mut at, Limits::default()).unwrap().root();
        assert!(http::parameters(schema, Some(args), &mut [Parameter::EMPTY; 1]).is_err(), "{number}");
    }
}
#[test]
fn custom_parameter_slot_capacity_and_missing_header() {
    let mut st = [Token::EMPTY; 128];
    let schema = Document::parse(SCHEMA, &mut st, Limits::default()).unwrap().root();
    assert!(matches!(http::parameters(schema, None, &mut [Parameter::EMPTY; 1]), Err(HttpError::Capacity { needed: 2 })));
    let mut at = [Token::EMPTY; 32];
    let args = Document::parse(br#"{"region":"us-west1"}"#, &mut at, Limits::default()).unwrap().root();
    let mut slots = [Parameter::EMPTY; 8];
    let params = http::parameters(schema, Some(args), &mut slots).unwrap();
    assert_eq!(http::validate_parameter_headers(params, &[], &mut [0; 128]), Err(HttpError::HeaderMismatch));
}
#[test]
fn error_body_is_valid_rpc_and_lists_supported_version() {
    let mut out = [0; 512];
    let n = HttpError::UnsupportedVersion.write_rpc(None, Version::V2026_07_28, "unknown-version", &mut out).unwrap();
    let mut tokens = [Token::EMPTY; 64];
    let Message::Failure { error, .. } = message(&out[..n], &mut tokens) else { panic!(); };
    assert_eq!(error.code, -32022);
    assert_eq!(error.data.unwrap().get("supported").unwrap().elements().count(), 1);
    assert!(error.data.unwrap().get("requested").unwrap().string().unwrap().matches("unknown-version"));
}

#[test]
fn explicit_2025_03_26_binding_accepts_missing_version_header() {
    let cfg = ServerConfig::stateless(Version::V2025_03_26);
    let hs = [Header { name: "Accept", value: http::ACCEPT }, Header { name: "Content-Type", value: http::JSON }];
    let mut tokens = [Token::EMPTY; 32];
    let msg = message(LEGACY_LIST, &mut tokens);
    assert!(cfg.validate(Incoming { method: Method::Post, headers: &hs, message: Some(msg) }, &ALLOW, &mut []).is_ok());
}
