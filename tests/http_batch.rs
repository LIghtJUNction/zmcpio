use core::cell::Cell;
use zmcpio::{
    batch::{Batch, BatchKind},
    http::{
        self, BodyKind, Header, HttpError, Method, ResponseMode, SecurityPolicy, ServerConfig,
        Version,
    },
    json::{Document, Limits, Token},
    rpc::Message,
};

const HEADERS: [Header<'static>; 3] = [
    Header {
        name: "Content-Type",
        value: "application/json",
    },
    Header {
        name: "Accept",
        value: "application/json, text/event-stream",
    },
    Header {
        name: "Authorization",
        value: "test-token",
    },
];
const REQUESTS: &str =
    r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"method":"tools/list"}]"#;

#[derive(Default)]
struct Policy {
    auth_calls: Cell<usize>,
    session_calls: Cell<usize>,
}
impl SecurityPolicy for Policy {
    fn origin_allowed(&self, origin: &str) -> bool {
        origin == "https://trusted.invalid"
    }
    fn authorized(&self, headers: &[Header<'_>]) -> bool {
        self.auth_calls.set(self.auth_calls.get() + 1);
        http::header(headers, "authorization").ok().flatten() == Some("test-token")
    }
    fn session_valid(&self, session: &str, _: &[Header<'_>]) -> bool {
        self.session_calls.set(self.session_calls.get() + 1);
        session == "session-a"
    }
}

fn validate(
    input: &str,
    config: ServerConfig,
    method: Method,
    headers: &[Header<'_>],
    policy: &Policy,
) -> Result<BodyKind, HttpError> {
    let mut tokens = [Token::EMPTY; 256];
    let document = Document::parse(input.as_bytes(), &mut tokens, Limits::default()).unwrap();
    let batch = Batch::parse(document.root(), "2025-03-26", 16).unwrap();
    config.validate_batch(method, headers, batch, policy)
}

fn config() -> ServerConfig {
    ServerConfig::stateless(Version::V2025_03_26)
}

#[test]
fn request_batch_accepts_json_or_sse_not_empty_202() {
    let kind = validate(
        REQUESTS,
        config(),
        Method::Post,
        &HEADERS,
        &Policy::default(),
    )
    .unwrap();
    assert_eq!(kind, BodyKind::Request);
    assert_eq!(
        http::response_mode(200, Some(http::JSON), kind),
        Ok(ResponseMode::Json)
    );
    assert_eq!(
        http::response_mode(200, Some(http::SSE), kind),
        Ok(ResponseMode::Sse)
    );
    assert!(http::response_mode(202, None, kind).is_err());
}

#[test]
fn notifications_only_require_202_and_no_body() {
    let input = r#"[{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","method":"notifications/roots/list_changed"}]"#;
    let kind = validate(input, config(), Method::Post, &HEADERS, &Policy::default()).unwrap();
    assert_eq!(kind, BodyKind::Notification);
    assert_eq!(
        http::response_mode(202, None, kind),
        Ok(ResponseMode::Empty)
    );
    assert!(http::response_mode(200, Some(http::JSON), kind).is_err());
}

#[test]
fn response_only_batch_requires_202_and_never_a_response_to_response() {
    let input = r#"[{"jsonrpc":"2.0","id":1,"result":{}},{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"unknown"}}]"#;
    let kind = validate(input, config(), Method::Post, &HEADERS, &Policy::default()).unwrap();
    assert_eq!(kind, BodyKind::Response);
    assert_eq!(
        http::response_mode(202, None, kind),
        Ok(ResponseMode::Empty)
    );
    assert!(http::response_mode(200, Some(http::SSE), kind).is_err());
}

#[test]
fn request_and_notification_batch_still_requires_request_results() {
    let input = r#"[{"jsonrpc":"2.0","method":"notifications/roots/list_changed"},{"jsonrpc":"2.0","id":1,"method":"tools/list"}]"#;
    assert_eq!(
        validate(input, config(), Method::Post, &HEADERS, &Policy::default()),
        Ok(BodyKind::Request)
    );
}

#[test]
fn invalid_request_entries_remain_individual_error_responses() {
    let input = br#"[{"jsonrpc":"2.0","id":1,"method":"ping"},7,{"jsonrpc":"2.0","method":"notifications/roots/list_changed"}]"#;
    let mut tokens = [Token::EMPTY; 128];
    let document = Document::parse(input, &mut tokens, Limits::default()).unwrap();
    let batch = Batch::parse(document.root(), "2025-03-26", 16).unwrap();
    assert_eq!(batch.kind(), BatchKind::Requests);
    assert_eq!(batch.len(), 3);
    let mut requests = 0;
    let mut notifications = 0;
    let mut invalid = 0;
    for entry in batch.entries() {
        match entry.message {
            Ok(Message::Request(_)) => requests += 1,
            Ok(Message::Notification(_)) => notifications += 1,
            Err(_) => invalid += 1,
            message => panic!("unexpected entry: {message:?}"),
        }
    }
    assert_eq!((requests, notifications, invalid), (1, 1, 1));
    assert_eq!(
        config().validate_batch(Method::Post, &HEADERS, batch, &Policy::default()),
        Ok(BodyKind::Request)
    );
}

#[test]
fn only_invalid_entries_must_not_be_acknowledged_as_notifications() {
    assert_eq!(
        validate(
            "[1,true,null]",
            config(),
            Method::Post,
            &HEADERS,
            &Policy::default()
        ),
        Ok(BodyKind::Request)
    );
}

#[test]
fn malformed_entries_in_a_response_batch_are_local_errors() {
    let input = r#"[{"jsonrpc":"2.0","id":1,"result":{}},7]"#;
    assert_eq!(
        validate(input, config(), Method::Post, &HEADERS, &Policy::default()),
        Ok(BodyKind::Response)
    );
}

#[test]
fn newer_transport_revisions_reject_batches_even_when_other_headers_are_valid() {
    for version in [
        Version::V2025_06_18,
        Version::V2025_11_25,
        Version::V2026_07_28,
    ] {
        let mut headers = HEADERS.to_vec();
        headers.push(Header {
            name: "MCP-Protocol-Version",
            value: version.as_str(),
        });
        assert_eq!(
            validate(
                REQUESTS,
                ServerConfig::stateless(version),
                Method::Post,
                &headers,
                &Policy::default()
            ),
            Err(HttpError::BadRequest)
        );
    }
}

#[test]
fn batching_is_not_a_get_or_delete_body_feature() {
    let mut cfg = config();
    cfg.allow_get = true;
    cfg.allow_delete = true;
    for method in [Method::Get, Method::Delete] {
        assert_eq!(
            validate(REQUESTS, cfg, method, &HEADERS, &Policy::default()),
            Err(HttpError::BadRequest)
        );
    }
}

#[test]
fn optional_version_header_must_match_the_explicit_legacy_binding() {
    let mut headers = HEADERS.to_vec();
    headers.push(Header {
        name: "MCP-Protocol-Version",
        value: "2025-03-26",
    });
    assert_eq!(
        validate(
            REQUESTS,
            config(),
            Method::Post,
            &headers,
            &Policy::default()
        ),
        Ok(BodyKind::Request)
    );
    headers.last_mut().unwrap().value = "2025-06-18";
    assert_eq!(
        validate(
            REQUESTS,
            config(),
            Method::Post,
            &headers,
            &Policy::default()
        ),
        Err(HttpError::UnsupportedVersion)
    );
    headers.last_mut().unwrap().value = "2025-03-26";
    headers.push(Header {
        name: "mcp-protocol-version",
        value: "2025-03-26",
    });
    assert_eq!(
        validate(
            REQUESTS,
            config(),
            Method::Post,
            &headers,
            &Policy::default()
        ),
        Err(HttpError::InvalidHeader)
    );
}

#[test]
fn authentication_and_origin_are_checked_before_returning_a_dispatchable_kind() {
    let policy = Policy::default();
    assert_eq!(
        validate(REQUESTS, config(), Method::Post, &HEADERS[..2], &policy),
        Err(HttpError::Unauthorized)
    );
    let mut headers = HEADERS.to_vec();
    headers.push(Header {
        name: "Origin",
        value: "https://untrusted.invalid",
    });
    assert_eq!(
        validate(REQUESTS, config(), Method::Post, &headers, &policy),
        Err(HttpError::Forbidden)
    );
    headers.last_mut().unwrap().value = "https://trusted.invalid";
    assert_eq!(
        validate(REQUESTS, config(), Method::Post, &headers, &policy),
        Ok(BodyKind::Request)
    );
    headers.push(Header {
        name: "authorization",
        value: "another-test-token",
    });
    assert_eq!(
        validate(REQUESTS, config(), Method::Post, &headers, &policy),
        Err(HttpError::InvalidHeader)
    );
}

#[test]
fn required_session_is_validated_once_for_the_whole_batch() {
    let mut cfg = config();
    cfg.sessions = true;
    let policy = Policy::default();
    let mut headers = HEADERS.to_vec();
    headers.push(Header {
        name: "Mcp-Session-Id",
        value: "session-a",
    });
    assert_eq!(
        validate(REQUESTS, cfg, Method::Post, &headers, &policy),
        Ok(BodyKind::Request)
    );
    assert_eq!(policy.auth_calls.get(), 1);
    assert_eq!(policy.session_calls.get(), 1);
}

#[test]
fn missing_expired_or_invalid_sessions_cannot_dispatch_batches() {
    let mut cfg = config();
    cfg.sessions = true;
    assert_eq!(
        validate(REQUESTS, cfg, Method::Post, &HEADERS, &Policy::default()),
        Err(HttpError::BadRequest)
    );
    let mut headers = HEADERS.to_vec();
    headers.push(Header {
        name: "Mcp-Session-Id",
        value: "expired",
    });
    assert_eq!(
        validate(REQUESTS, cfg, Method::Post, &headers, &Policy::default()),
        Err(HttpError::SessionExpired)
    );
    assert_eq!(HttpError::SessionExpired.status(), 404);
    headers.last_mut().unwrap().value = "invalid session";
    assert_eq!(
        validate(REQUESTS, cfg, Method::Post, &headers, &Policy::default()),
        Err(HttpError::InvalidHeader)
    );
}

#[test]
fn notification_and_response_batches_do_not_bypass_session_policy() {
    let mut cfg = config();
    cfg.sessions = true;
    for input in [
        r#"[{"jsonrpc":"2.0","method":"notifications/roots/list_changed"}]"#,
        r#"[{"jsonrpc":"2.0","id":1,"result":{}}]"#,
    ] {
        assert_eq!(
            validate(input, cfg, Method::Post, &HEADERS, &Policy::default()),
            Err(HttpError::BadRequest)
        );
    }
}

#[test]
fn both_accept_types_and_json_content_type_are_required() {
    let mut headers = HEADERS;
    headers[0].value = "text/plain";
    assert_eq!(
        validate(
            REQUESTS,
            config(),
            Method::Post,
            &headers,
            &Policy::default()
        ),
        Err(HttpError::UnsupportedMediaType)
    );
    headers[0].value = "Application/JSON; charset=utf-8";
    for accept in [
        "application/json",
        "text/event-stream",
        "*/*",
        "application/json, text/event-stream;q=0",
    ] {
        headers[1].value = accept;
        assert_eq!(
            validate(
                REQUESTS,
                config(),
                Method::Post,
                &headers,
                &Policy::default()
            ),
            Err(HttpError::NotAcceptable),
            "{accept}"
        );
    }
    headers[1].value = http::ACCEPT;
    assert_eq!(
        validate(
            REQUESTS,
            config(),
            Method::Post,
            &headers,
            &Policy::default()
        ),
        Ok(BodyKind::Request)
    );
}

#[test]
fn batch_preflight_rejects_empty_nonarray_mixed_and_initialize_envelopes() {
    for input in [
        "[]",
        "{}",
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"result":{}}]"#,
        r#"[{"jsonrpc":"2.0","method":"notifications/roots/list_changed"},{"jsonrpc":"2.0","id":2,"result":{}}]"#,
        r#"[{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}]"#,
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}]"#,
        r#"[{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}]"#,
    ] {
        let mut tokens = [Token::EMPTY; 128];
        let document = Document::parse(input.as_bytes(), &mut tokens, Limits::default()).unwrap();
        assert!(
            Batch::parse(document.root(), "2025-03-26", 16).is_err(),
            "{input}"
        );
    }
}

#[test]
fn batch_preflight_respects_revision_and_item_limits_before_dispatch() {
    let mut tokens = [Token::EMPTY; 128];
    let document = Document::parse(REQUESTS.as_bytes(), &mut tokens, Limits::default()).unwrap();
    for version in [
        "2024-11-05",
        "2025-06-18",
        "2025-11-25",
        "2026-07-28",
        "unknown",
    ] {
        assert!(
            Batch::parse(document.root(), version, 16).is_err(),
            "{version}"
        );
    }
    assert!(Batch::parse(document.root(), "2025-03-26", 1).is_err());
    assert!(Batch::parse(document.root(), "2025-03-26", 0).is_err());
    let batch = Batch::parse(document.root(), "2025-03-26", 2).unwrap();
    assert_eq!(
        config().validate_batch(Method::Post, &HEADERS, batch, &Policy::default()),
        Ok(BodyKind::Request)
    );
}
