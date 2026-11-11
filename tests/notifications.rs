use zmcp::{
    json::{Document, Limits, Token},
    mcp::{Action, Notice, Server},
    notifications,
    rpc::Id,
};
#[test]
fn progress_and_cancellation_round_trip() {
    let mut tokens = [Token::EMPTY; 32];
    let d = Document::parse(
        br#"{"token":"job-1","progress":1.5,"total":9}"#,
        &mut tokens,
        Limits::default(),
    )
    .unwrap();
    let mut output = [0; 512];
    let n = notifications::progress(
        d.root().get("token").unwrap(),
        d.root().get("progress").unwrap(),
        d.root().get("total"),
        Some("step\n1"),
        &mut output,
    )
    .unwrap();
    let mut server = Server::new("s", "1");
    let mut read_tokens = [Token::EMPTY; 64];
    let mut reply = [0; 64];
    let Action::Notification(Notice::Progress {
        progress, total, ..
    }) = server
        .handle(
            &output[..n],
            &mut read_tokens,
            Limits::default(),
            &mut reply,
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(progress, 1.5);
    assert_eq!(total, Some(9.0));
    let n = notifications::cancelled(Id::Number(7), Some("caller stopped"), &mut output).unwrap();
    assert!(matches!(
        server
            .handle(
                &output[..n],
                &mut read_tokens,
                Limits::default(),
                &mut reply
            )
            .unwrap(),
        Action::Notification(Notice::Cancelled { .. })
    ));
    assert!(notifications::cancelled(Id::Number(7), None, &mut []).is_err());
}
#[test]
fn malformed_progress_is_rejected() {
    for raw in [b"-1".as_slice(), b"1e999", b"null", b"true"] {
        let mut t = [Token::EMPTY; 4];
        let mut tt = [Token::EMPTY; 4];
        let value = Document::parse(raw, &mut t, Limits::default()).unwrap();
        let token = Document::parse(b"1", &mut tt, Limits::default()).unwrap();
        assert!(
            notifications::progress(token.root(), value.root(), None, None, &mut [0; 512]).is_err()
        );
    }
}
