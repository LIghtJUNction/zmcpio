use zmcpio::legacy_sse::{self, Action, Error, Scheme, Session};
use zmcpio::sse::Event;

fn event<'a>(kind: &'a str, data: Option<&'a [u8]>) -> Event<'a> {
    Event {
        data,
        event: kind,
        id: None,
        retry_ms: None,
    }
}

#[test]
fn relative_absolute_and_query_endpoint_references() {
    let base = "https://EXAMPLE.com:443/a/sse?old=1";
    for (reference, expected) in [
        ("/messages?sid=2", "/messages?sid=2"),
        ("messages?sid=2", "/a/messages?sid=2"),
        ("./messages", "/a/messages"),
        ("../messages", "/messages"),
        ("?sid=2", "/a/sse?sid=2"),
        ("HTTPS://example.COM/messages", "/messages"),
        ("https://example.com:0443/messages", "/messages"),
        ("https://example.com?sid=2", "/?sid=2"),
        ("/%E4%BD%A0?x=%20", "/%E4%BD%A0?x=%20"),
        (
            "messages?next=https://elsewhere.test/",
            "/a/messages?next=https://elsewhere.test/",
        ),
    ] {
        let mut target = [0; 256];
        assert_eq!(
            legacy_sse::resolve_endpoint(base, reference, &mut target).unwrap(),
            expected
        );
    }
}

#[test]
fn literal_dot_segments_use_rfc_resolution_and_preserve_query() {
    for (reference, expected) in [
        ("/a/b/c/./../../g", "/a/g"),
        ("/a/../../g", "/g"),
        ("/./g", "/g"),
        ("/g/.", "/g/"),
        ("/g/..", "/"),
        ("g/../h", "/a/h"),
        ("../..", "/"),
        (".", "/a/"),
        ("..", "/"),
        ("/a?x=/../", "/a?x=/../"),
    ] {
        let mut target = [0; 128];
        assert_eq!(
            legacy_sse::resolve_endpoint("https://example.com/a/sse", reference, &mut target)
                .unwrap(),
            expected,
            "{reference}"
        );
    }
}

#[test]
fn ip_origins_compare_address_and_effective_port() {
    for (base, reference) in [
        ("http://127.0.0.1/sse", "http://127.0.0.1:80/post"),
        (
            "https://[2001:db8::1]/sse",
            "https://[2001:0db8:0:0:0:0:0:1]:443/post",
        ),
        (
            "http://[::1]:8080/sse",
            "http://[0:0:0:0:0:0:0:1]:8080/post",
        ),
    ] {
        let mut target = [0; 128];
        assert_eq!(
            legacy_sse::resolve_endpoint(base, reference, &mut target).unwrap(),
            "/post"
        );
    }
}

#[test]
fn cross_origin_and_ambiguous_references_never_mutate_output() {
    for reference in [
        "https://evil.test/post",
        "http://example.com/post",
        "https://example.com:444/post",
        "//example.com/post",
        "/a/..//evil.test/post",
        "/a//b",
        "https://example.com@evil.test/post",
        "https://evil.test@example.com/post",
        "https://example.com%40evil.test/post",
        "/post\r\nAuthorization: stolen",
        "/post\\evil",
        "/post%0d%0aX:bad",
        "/post%00",
        "/post%5c",
        "/post#fragment",
        "/post%",
        "/post%z1",
        "/post%1",
        "/%2e/post",
        "/a/.%2E/post",
        "/a/%2e%2e/post",
        "mailto:x@example.com",
        "https:evil.test",
        "你好",
        "",
    ] {
        let mut output = [0xa5; 256];
        assert!(
            legacy_sse::resolve_endpoint("https://example.com/sse", reference, &mut output)
                .is_err(),
            "{reference}"
        );
        assert_eq!(output, [0xa5; 256]);
    }
}

#[test]
fn invalid_or_noncanonical_base_is_rejected() {
    for base in [
        "ftp://example.com/sse",
        "https://example.com/a/../sse",
        "https://example.com/%2e/sse",
        "https://user:secret@example.com/sse",
        "https://example.com/sse#fragment",
        "https://example..com/sse",
        "https://-example.com/sse",
        "https://example.com:/sse",
        "https://example.com:65536/sse",
        "https://[not-an-ip]/sse",
        "http://[fe80::1%25eth0]/sse",
        "http://127.1/sse",
        "http://256.0.0.1/sse",
    ] {
        let mut target = [0; 128];
        assert!(Session::new(base, &mut target).is_err(), "{base}");
    }
}

#[test]
fn endpoint_must_precede_messages_and_cannot_be_rebound() {
    let mut target = [0; 128];
    let mut session = Session::new("https://example.com/a/sse", &mut target).unwrap();
    assert_eq!(session.scheme(), Scheme::Https);
    assert_eq!(session.authority(), "example.com");
    assert_eq!(session.endpoint_target(), None);
    assert_eq!(
        session.on_event(event("message", Some(b"{}"))),
        Err(Error::MissingEndpoint)
    );
    assert_eq!(session.on_event(event("message", None)), Ok(Action::Ignore));
    assert_eq!(
        session.on_event(event("unknown", Some(b"ignored"))),
        Ok(Action::Ignore)
    );
    assert_eq!(
        session.on_event(event("endpoint", Some(b"post?sid=1"))),
        Ok(Action::EndpointReady)
    );
    assert_eq!(session.endpoint_target(), Some("/a/post?sid=1"));
    let message = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
    assert_eq!(
        session.on_event(event("message", Some(message))),
        Ok(Action::Message(message))
    );
    assert_eq!(
        session.on_event(event("endpoint", Some(b"/other"))),
        Err(Error::DuplicateEndpoint)
    );
    assert_eq!(session.endpoint_target(), Some("/a/post?sid=1"));
    session.reset();
    assert_eq!(session.endpoint_target(), None);
    assert_eq!(
        session.on_event(event("message", Some(b"{}"))),
        Err(Error::MissingEndpoint)
    );
    assert_eq!(
        session.on_event(event("endpoint", Some(b"/new"))),
        Ok(Action::EndpointReady)
    );
    assert_eq!(session.endpoint_target(), Some("/new"));
}

#[test]
fn endpoint_capacity_can_be_retried_without_accepting_partial_state() {
    let mut small = [0; 2];
    let mut large = [0; 128];
    let mut too_small = [0; 1];
    let mut session = Session::new("http://localhost:8080/sse", &mut small).unwrap();
    assert_eq!(
        session.on_event(event("endpoint", Some(b"/messages"))),
        Err(Error::Capacity { needed: 9 })
    );
    assert_eq!(session.endpoint_target(), None);
    session.replace_storage(&mut large).unwrap();
    assert_eq!(
        session.on_event(event("endpoint", Some(b"/messages"))),
        Ok(Action::EndpointReady)
    );
    assert_eq!(
        session.replace_storage(&mut too_small),
        Err(Error::Capacity { needed: 9 })
    );
    assert_eq!(session.endpoint_target(), Some("/messages"));
}

#[test]
fn failed_capacity_and_endpoint_syntax_do_not_modify_output() {
    let mut output = [0xa5; 2];
    assert_eq!(
        legacy_sse::resolve_endpoint("https://example.com/sse", "/messages", &mut output),
        Err(Error::Capacity { needed: 9 })
    );
    assert_eq!(output, [0xa5; 2]);
    let mut target = [0; 32];
    let mut session = Session::new("https://example.com/sse", &mut target).unwrap();
    assert_eq!(
        session.on_event(event("endpoint", None)),
        Err(Error::InvalidUri)
    );
    assert_eq!(
        session.on_event(event("endpoint", Some(&[0xff]))),
        Err(Error::InvalidUri)
    );
    assert_eq!(session.endpoint_target(), None);
}

#[test]
fn real_sse_wire_drives_legacy_endpoint_and_message_state() {
    let mut target = [0; 128];
    let mut session = Session::new("https://example.com/sse", &mut target).unwrap();
    let mut line = [0; 128];
    let mut data = [0; 128];
    let mut name = [0; 32];
    let mut id = [0; 32];
    let mut decoder = zmcpio::sse::Decoder::new(&mut line, &mut data, &mut name, &mut id, 1024);
    let wire = b"event: endpoint\ndata: /messages?sid=7\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
    let mut messages = 0;
    for byte in wire {
        let step = decoder.feed(core::slice::from_ref(byte));
        if step.status == zmcpio::sse::Status::Ready {
            match session.on_event(decoder.event().unwrap()).unwrap() {
                Action::EndpointReady => {
                    assert_eq!(session.endpoint_target(), Some("/messages?sid=7"))
                }
                Action::Message(bytes) => {
                    assert_eq!(bytes, br#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
                    messages += 1;
                }
                Action::Ignore => {}
            }
            decoder.consume();
        } else {
            assert_eq!(step.status, zmcpio::sse::Status::NeedInput);
        }
    }
    assert_eq!(messages, 1);
    decoder.finish().unwrap();
}
