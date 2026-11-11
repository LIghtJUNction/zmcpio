use core::convert::Infallible;
use zmcp::{
    json::{Limits, Token},
    mcp::{Action, Server},
    transport::{MessageReader, MessageWriter, TransportError},
};

struct Memory<'a> {
    incoming: &'a [u8],
    outgoing: &'a mut [u8],
    written: usize,
    read: bool,
}
impl MessageReader for Memory<'_> {
    type Error = Infallible;
    fn receive(&mut self, output: &mut [u8]) -> Result<usize, TransportError<Self::Error>> {
        if self.read {
            return Err(TransportError::Closed);
        }
        let dst = output
            .get_mut(..self.incoming.len())
            .ok_or(TransportError::Capacity {
                needed: self.incoming.len(),
            })?;
        dst.copy_from_slice(self.incoming);
        self.read = true;
        Ok(self.incoming.len())
    }
}
impl MessageWriter for Memory<'_> {
    type Error = Infallible;
    fn send(&mut self, message: &[u8]) -> Result<(), TransportError<Self::Error>> {
        let dst = self
            .outgoing
            .get_mut(..message.len())
            .ok_or(TransportError::Capacity {
                needed: message.len(),
            })?;
        dst.copy_from_slice(message);
        self.written = message.len();
        Ok(())
    }
}
fn main() {
    let mut received = [0; 256];
    let mut sent = [0; 256];
    let mut response = [0; 256];
    let mut transport = Memory {
        incoming: br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        outgoing: &mut sent,
        written: 0,
        read: false,
    };
    let n = transport.receive(&mut received).unwrap();
    let mut server = Server::new("memory", "1");
    let mut tokens = [Token::EMPTY; 64];
    let Action::Reply(n) = server
        .handle(
            &received[..n],
            &mut tokens,
            Limits::default(),
            &mut response,
        )
        .unwrap()
    else {
        panic!("expected ping response")
    };
    transport.send(&response[..n]).unwrap();
    assert_eq!(
        &transport.outgoing[..transport.written],
        br#"{"jsonrpc":"2.0","id":1,"result":{}}"#
    );
}
