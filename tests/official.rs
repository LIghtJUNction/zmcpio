//! Official MCP 2026-07-28 schema example, not a claim of full conformance-suite coverage.
use zmcp::{
    json::{Document, Limits, Token},
    mcp::{Action, Server},
    rpc::{self, Message},
};
#[test]
fn official_discover_request() {
    let mut server = Server::new("test", "1");
    let mut tokens = [Token::EMPTY; 128];
    let mut output = [0; 1024];
    let Action::Reply(n) = server
        .handle(
            include_bytes!("fixtures/server-discover-request.json"),
            &mut tokens,
            Limits::default(),
            &mut output,
        )
        .unwrap()
    else {
        panic!("expected discover response")
    };
    let d = Document::parse(&output[..n], &mut tokens, Limits::default()).unwrap();
    let Message::Success { id, result } = rpc::parse(d.root()).unwrap() else {
        panic!("expected success")
    };
    assert!(matches!(id, rpc::Id::String(s) if s.matches("discover-1")));
    assert_eq!(
        result.get("supportedVersions").unwrap().elements().count(),
        2
    );
}
