//! Safe default cache hints for modern cacheable results, never tool outputs.
use zmcpio::{
    json::{Document, Limits, Token, Value},
    mcp::{Action, Server},
    tools::{Content, ToolResult},
};

fn modern(method: &str, extra: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"{method}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}{extra}}}}}"#
    )
}

fn result<'a>(input: &'a [u8], tokens: &'a mut [Token]) -> Value<'a> {
    Document::parse(input, tokens, Limits::default())
        .unwrap()
        .root()
        .get("result")
        .unwrap()
}

fn assert_private_uncached(value: Value<'_>) {
    assert_eq!(value.get("ttlMs").unwrap().integer().unwrap(), 0);
    assert!(
        value
            .get("cacheScope")
            .unwrap()
            .string()
            .unwrap()
            .matches("private")
    );
}

#[test]
fn modern_discovery_includes_safe_cache_hints() {
    let mut server = Server::new("cache-test", "1");
    let mut tokens = [Token::EMPTY; 128];
    let mut parsed = [Token::EMPTY; 128];
    let mut output = [0; 1024];
    let request = modern("server/discover", "");
    let Action::Reply(n) = server
        .handle(
            request.as_bytes(),
            &mut tokens,
            Limits::default(),
            &mut output,
        )
        .unwrap()
    else {
        panic!("expected discovery result");
    };
    assert_private_uncached(result(&output[..n], &mut parsed));
}

#[test]
fn modern_tool_page_includes_safe_cache_hints_and_cursor() {
    let mut server = Server::new("cache-test", "1");
    let mut tokens = [Token::EMPTY; 128];
    let mut parsed = [Token::EMPTY; 128];
    let mut output = [0; 1024];
    let request = modern("tools/list", "");
    let Action::ListTools { context, .. } = server
        .handle(
            request.as_bytes(),
            &mut tokens,
            Limits::default(),
            &mut output,
        )
        .unwrap()
    else {
        panic!("expected list action");
    };
    let n = server
        .list_tools(context, &[], Some("page-2"), &mut output)
        .unwrap();
    let result = result(&output[..n], &mut parsed);
    assert_private_uncached(result);
    assert!(result.get("tools").unwrap().elements().next().is_none());
    assert!(
        result
            .get("nextCursor")
            .unwrap()
            .string()
            .unwrap()
            .matches("page-2")
    );
}

#[test]
fn legacy_tool_page_remains_unchanged() {
    let mut server = Server::new("cache-test", "1");
    let mut tokens = [Token::EMPTY; 128];
    let mut parsed = [Token::EMPTY; 128];
    let mut output = [0; 1024];
    server
        .handle(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
            &mut tokens,
            Limits::default(),
            &mut output,
        )
        .unwrap();
    server
        .handle(
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &mut tokens,
            Limits::default(),
            &mut output,
        )
        .unwrap();
    let Action::ListTools { context, .. } = server
        .handle(
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            &mut tokens,
            Limits::default(),
            &mut output,
        )
        .unwrap()
    else {
        panic!("expected list action");
    };
    let n = server.list_tools(context, &[], None, &mut output).unwrap();
    let result = result(&output[..n], &mut parsed);
    assert!(result.get("ttlMs").is_none());
    assert!(result.get("cacheScope").is_none());
    assert!(result.get("resultType").is_none());
}

#[test]
fn tool_call_results_do_not_get_cache_hints() {
    let mut server = Server::new("cache-test", "1");
    let mut tokens = [Token::EMPTY; 128];
    let mut parsed = [Token::EMPTY; 128];
    let mut output = [0; 1024];
    let request = modern("tools/call", r#", "name":"echo", "arguments":{}"#);
    let Action::CallTool(call) = server
        .handle(
            request.as_bytes(),
            &mut tokens,
            Limits::default(),
            &mut output,
        )
        .unwrap()
    else {
        panic!("expected call action");
    };
    let n = server
        .tool_result(
            call.context,
            ToolResult::new(&[Content::Text("done")]),
            &mut output,
        )
        .unwrap();
    let result = result(&output[..n], &mut parsed);
    assert!(result.get("ttlMs").is_none());
    assert!(result.get("cacheScope").is_none());
}
