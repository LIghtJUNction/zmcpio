//! Message transport boundaries. HTTP and WebSocket adapters supply complete
//! payloads directly. LineDecoder is specifically the MCP stdio LF framing.
use core::task::{Context, Poll};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError<E> { Io(E), Capacity { needed: usize }, Closed, Incomplete, FrameTooLarge }

pub trait MessageReader {
    type Error;
    /// One complete message, never an arbitrary network chunk.
    fn receive(&mut self, output: &mut [u8]) -> Result<usize, TransportError<Self::Error>>;
}
pub trait MessageWriter {
    type Error;
    /// Send one whole message or report a transport failure; do not append LF for HTTP.
    fn send(&mut self, message: &[u8]) -> Result<(), TransportError<Self::Error>>;
}
pub trait PollMessageReader {
    type Error;
    fn poll_receive(&mut self, cx: &mut Context<'_>, output: &mut [u8]) -> Poll<Result<usize, TransportError<Self::Error>>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameStatus { NeedInput, Ready, NeedCapacity { needed: usize }, TooLarge }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Step { pub consumed: usize, pub status: FrameStatus }

pub struct LineDecoder<'a> { storage: &'a mut [u8], used: usize, ready: bool, max_frame: usize }
impl<'a> LineDecoder<'a> {
    pub fn new(storage: &'a mut [u8], max_frame: usize) -> Self { Self { storage, used: 0, ready: false, max_frame } }
    pub fn feed(&mut self, input: &[u8]) -> Step {
        if self.ready { return Step { consumed: 0, status: FrameStatus::Ready }; }
        for (at, byte) in input.iter().copied().enumerate() {
            if byte == b'\n' {
                if self.used > 0 && self.storage[self.used - 1] == b'\r' { self.used -= 1; }
                self.ready = true;
                return Step { consumed: at + 1, status: FrameStatus::Ready };
            }
            if self.used >= self.max_frame { return Step { consumed: at, status: FrameStatus::TooLarge }; }
            if self.used == self.storage.len() { return Step { consumed: at, status: FrameStatus::NeedCapacity { needed: self.used.saturating_add(1) } }; }
            self.storage[self.used] = byte;
            self.used += 1;
        }
        Step { consumed: input.len(), status: FrameStatus::NeedInput }
    }
    pub fn message(&self) -> Option<&[u8]> { self.ready.then_some(&self.storage[..self.used]) }
    /// Acknowledge the frame only after the consumer has accepted it.
    pub fn consume(&mut self) { self.ready = false; self.used = 0; }
    /// Preserve partial input when replacing caller storage. Failure leaves it intact.
    pub fn replace_storage(&mut self, storage: &'a mut [u8]) -> Result<(), TransportError<core::convert::Infallible>> {
        if storage.len() < self.used { return Err(TransportError::Capacity { needed: self.used }); }
        storage[..self.used].copy_from_slice(&self.storage[..self.used]);
        self.storage = storage;
        Ok(())
    }
    pub fn finish(&self) -> Result<(), TransportError<core::convert::Infallible>> {
        if !self.ready && self.used != 0 { Err(TransportError::Incomplete) } else { Ok(()) }
    }
    pub fn memory_bytes(&self) -> usize { core::mem::size_of::<Self>() + self.storage.len() }
}
