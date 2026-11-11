use zmcpio::{
    json::{Limits, Token},
    mcp::{Action, Server},
};
fn main() -> Result<(), zmcpio::Error> {
    let mut server = Server::new("example", "1");
    let mut tokens = [Token::EMPTY; 64];
    let mut output = [0; 1024];
    let request = br#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    if let Action::Reply(n) = server.handle(request, &mut tokens, Limits::default(), &mut output)? {
        println!(
            "{}",
            core::str::from_utf8(&output[..n]).expect("encoded UTF-8")
        );
    }
    Ok(())
}
