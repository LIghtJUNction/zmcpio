use zmcp::{
    ErrorKind,
    json::{Document, Limits, Token},
    rpc::{self, Id, Message},
    transport::{FrameStatus, LineDecoder},
};

#[test]
fn json_round_trip_and_unicode() {
    let mut tokens = [Token::EMPTY; 32];
    let d = Document::parse(
        br#"{"a":[null,true,-1.25e+2],"\u0062":"\ud83d\ude80"}"#,
        &mut tokens,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(d.root().get("a").unwrap().elements().count(), 3);
    let s = d.root().get("b").unwrap().string().unwrap();
    assert!(s.matches("🚀"));
    let mut out = [0; 4];
    assert_eq!(s.decode_into(&mut out).unwrap(), "🚀");
    assert!(d.token_bytes() <= 32 * core::mem::size_of::<Token>());
}

#[test]
fn reject_adversarial_json() {
    for input in [
        "",
        "[",
        "[1,]",
        "{\"a\":}",
        "{\"a\":1,}",
        "00",
        "-",
        "1.",
        "1e+",
        "NaN",
        "true false",
        "\"\\ud800\"",
        "\"\\udc00\"",
        "\"a\nb\"",
        "{\"a\":1,\"\\u0061\":2}",
        "{\"a\":{\"b\":1},\"a\":2}",
    ] {
        let mut t = [Token::EMPTY; 64];
        assert!(
            Document::parse(input.as_bytes(), &mut t, Limits::default()).is_err(),
            "accepted {input:?}"
        );
    }
}

#[test]
fn resource_limits_and_integer_overflow() {
    let mut t = [Token::EMPTY; 32];
    assert_eq!(
        Document::parse(b"{}", &mut [], Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Tokens
    );
    assert_eq!(
        Document::parse(
            b"{}",
            &mut t,
            Limits {
                max_bytes: 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::InputLimit
    );
    assert_eq!(
        Document::parse(
            b"[[[]]]",
            &mut t,
            Limits {
                max_depth: 2,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Depth
    );
    assert_eq!(
        Document::parse(
            b"[1,2]",
            &mut t,
            Limits {
                max_members: 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Members
    );
    assert_eq!(
        Document::parse(
            b"\"abc\"",
            &mut t,
            Limits {
                max_string_bytes: 2,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::StringLimit
    );
    let d = Document::parse(b"9223372036854775808", &mut t, Limits::default()).unwrap();
    assert_eq!(d.root().integer().unwrap_err().kind, ErrorKind::Integer);
    assert!(Document::parse(&[0xff], &mut t, Limits::default()).is_err());
}

#[test]
fn rpc_classes_are_disjoint() {
    let cases = [
        (r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, 0),
        (r#"{"jsonrpc":"2.0","method":"ping"}"#, 1),
        (r#"{"jsonrpc":"2.0","id":"a","result":{}}"#, 2),
        (
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad"}}"#,
            3,
        ),
        (
            r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"bad"}}"#,
            3,
        ),
    ];
    for (raw, expected) in cases {
        let mut t = [Token::EMPTY; 64];
        let m = rpc::parse(
            Document::parse(raw.as_bytes(), &mut t, Limits::default())
                .unwrap()
                .root(),
        )
        .unwrap();
        let got = match m {
            Message::Request(_) => 0,
            Message::Notification(_) => 1,
            Message::Success { .. } => 2,
            Message::Failure { .. } => 3,
        };
        assert_eq!(got, expected);
    }
    for raw in [
        r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"ping","result":{}}"#,
        r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","result":{}}"#,
    ] {
        let mut t = [Token::EMPTY; 64];
        let d = Document::parse(raw.as_bytes(), &mut t, Limits::default()).unwrap();
        assert!(rpc::parse(d.root()).is_err(), "accepted {raw}");
    }
}

#[test]
fn response_encoding_capacity_and_escaping() {
    let mut output = [0; 512];
    let n = rpc::failure(
        Some(Id::Number(i64::MIN)),
        -32602,
        "bad\n\"input",
        None,
        &mut output,
    )
    .unwrap();
    let mut t = [Token::EMPTY; 64];
    let d = Document::parse(&output[..n], &mut t, Limits::default()).unwrap();
    assert_eq!(d.root().get("id").unwrap().integer().unwrap(), i64::MIN);
    assert!(matches!(
        rpc::parse(d.root()).unwrap(),
        Message::Failure { .. }
    ));
    let e = rpc::failure(
        Some(Id::Number(i64::MIN)),
        -32602,
        "bad\n\"input",
        None,
        &mut [],
    )
    .unwrap_err();
    assert_eq!(e.needed, n);
}

#[test]
fn framing_every_split_and_backpressure() {
    let raw = b"{\"x\":1}\r\nsecond\n";
    for split in 0..=9 {
        let mut storage = [0; 64];
        let mut f = LineDecoder::new(&mut storage, 64);
        let first = f.feed(&raw[..split]);
        let second = f.feed(&raw[split..]);
        assert_eq!(first.consumed + second.consumed, 9);
        assert_eq!(f.message().unwrap(), b"{\"x\":1}");
        assert_eq!(f.feed(b"later").consumed, 0);
        f.consume();
        assert_eq!(f.feed(&raw[9..]).status, FrameStatus::Ready);
        assert_eq!(f.message().unwrap(), b"second");
    }
}

#[test]
fn framing_capacity_recovery_and_eof() {
    let mut small = [0; 2];
    let mut large = [0; 16];
    let mut f = LineDecoder::new(&mut small, 16);
    let step = f.feed(b"abc\n");
    assert_eq!(step.consumed, 2);
    assert_eq!(step.status, FrameStatus::NeedCapacity { needed: 3 });
    assert!(f.finish().is_err());
    f.replace_storage(&mut large).unwrap();
    assert_eq!(f.feed(b"c\n").status, FrameStatus::Ready);
    assert_eq!(f.message().unwrap(), b"abc");
    f.consume();
    assert!(f.finish().is_ok());
    let mut storage = [0; 8];
    let mut f = LineDecoder::new(&mut storage, 2);
    assert_eq!(f.feed(b"abc\n").status, FrameStatus::TooLarge);
}

#[test]
fn malformed_bytes_never_panic() {
    let mut state = 0x12345678u32;
    for len in 0..128 {
        for _ in 0..32 {
            let mut bytes = [0u8; 128];
            for b in &mut bytes[..len] {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *b = state as u8;
            }
            let mut t = [Token::EMPTY; 64];
            if let Ok(d) = Document::parse(&bytes[..len], &mut t, Limits::default()) {
                let _ = rpc::parse(d.root());
            }
        }
    }
}
