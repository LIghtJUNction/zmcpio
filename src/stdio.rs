//! Blocking MCP stdio transport, available with the `std` feature.
//!
//! Framing uses caller-owned storage and a fixed-size read chunk. No per-message
//! heap allocation is performed by this module. Process creation itself uses
//! the standard library and may allocate. Blocking reads have no built-in
//! deadline; use an application-owned transport when response timeouts or
//! asynchronous cancellation are required.
use crate::transport::{FrameStatus, LineDecoder, MessageReader, MessageWriter, TransportError};
use std::{
    io::{self, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

/// LF framing over caller-supplied blocking I/O.
///
/// The frame buffer may be replaced after a capacity error. A complete frame is
/// retained when `receive`'s output is too small, so retrying does not skip it.
pub struct StdioIo<'a, R, W> {
    reader: R,
    writer: W,
    decoder: LineDecoder<'a>,
    chunk: [u8; 4096],
    start: usize,
    end: usize,
    max_frame: usize,
    read_closed: bool,
    write_failed: bool,
}

impl<'a, R, W> StdioIo<'a, R, W> {
    pub fn new(reader: R, writer: W, storage: &'a mut [u8], max_frame: usize) -> Self {
        Self {
            reader,
            writer,
            decoder: LineDecoder::new(storage, max_frame),
            chunk: [0; 4096],
            start: 0,
            end: 0,
            max_frame,
            read_closed: false,
            write_failed: false,
        }
    }

    /// Replace the frame buffer without discarding already accepted bytes.
    pub fn replace_storage(
        &mut self,
        storage: &'a mut [u8],
    ) -> Result<(), TransportError<io::Error>> {
        self.decoder
            .replace_storage(storage)
            .map_err(|error| match error {
                TransportError::Capacity { needed } => TransportError::Capacity { needed },
                TransportError::Closed => TransportError::Closed,
                TransportError::Incomplete => TransportError::Incomplete,
                TransportError::FrameTooLarge => TransportError::FrameTooLarge,
                TransportError::Io(never) => match never {},
            })
    }

    /// Recover the underlying I/O. Any unread buffered data is discarded.
    pub fn into_parts(self) -> (R, W) {
        (self.reader, self.writer)
    }
}

impl<R: Read, W> MessageReader for StdioIo<'_, R, W> {
    type Error = io::Error;

    fn receive(&mut self, output: &mut [u8]) -> Result<usize, TransportError<Self::Error>> {
        loop {
            if let Some(message) = self.decoder.message() {
                let len = message.len();
                let dst = output
                    .get_mut(..len)
                    .ok_or(TransportError::Capacity { needed: len })?;
                dst.copy_from_slice(message);
                self.decoder.consume();
                return Ok(len);
            }
            if self.read_closed {
                return Err(TransportError::Closed);
            }
            if self.start != self.end {
                let step = self.decoder.feed(&self.chunk[self.start..self.end]);
                self.start += step.consumed;
                match step.status {
                    FrameStatus::Ready | FrameStatus::NeedInput => continue,
                    FrameStatus::NeedCapacity { needed } => {
                        return Err(TransportError::Capacity { needed });
                    }
                    FrameStatus::TooLarge => {
                        // A rejected suffix must never be mistaken for another message.
                        self.read_closed = true;
                        return Err(TransportError::FrameTooLarge);
                    }
                }
            }
            match self.reader.read(&mut self.chunk) {
                Ok(0) => {
                    self.read_closed = true;
                    return if self.decoder.finish().is_ok() {
                        Err(TransportError::Closed)
                    } else {
                        Err(TransportError::Incomplete)
                    };
                }
                Ok(len) => {
                    self.start = 0;
                    self.end = len;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                // Preserve partial framing so timeout/WouldBlock retries are safe.
                Err(error) => return Err(TransportError::Io(error)),
            }
        }
    }
}

impl<R, W: Write> MessageWriter for StdioIo<'_, R, W> {
    type Error = io::Error;

    fn send(&mut self, message: &[u8]) -> Result<(), TransportError<Self::Error>> {
        if self.write_failed {
            return Err(TransportError::Closed);
        }
        if message.len() > self.max_frame {
            return Err(TransportError::FrameTooLarge);
        }
        if message.is_empty() || message.iter().any(|b| matches!(b, b'\n' | b'\r')) {
            return Err(TransportError::Io(io::ErrorKind::InvalidInput.into()));
        }
        let result = self
            .writer
            .write_all(message)
            .and_then(|()| self.writer.write_all(b"\n"))
            .and_then(|()| self.writer.flush());
        if result.is_err() {
            // A partial write is ambiguous; never silently retry a tool call.
            self.write_failed = true;
        }
        result.map_err(TransportError::Io)
    }
}

/// An explicitly spawned MCP server process with owned stdin/stdout pipes.
///
/// No shell is inserted. Supply executable, arguments, environment and working
/// directory through `Command`. Stderr is inherited separately from protocol
/// stdout; `spawn` overrides any existing stdin/stdout/stderr configuration.
/// Only the direct child is managed, not processes it may create itself.
pub struct StdioTransport<'a> {
    child: Option<Child>,
    io: Option<StdioIo<'a, ChildStdout, ChildStdin>>,
    exit_status: Option<ExitStatus>,
}

impl<'a> StdioTransport<'a> {
    pub fn spawn(
        command: &mut Command,
        storage: &'a mut [u8],
        max_frame: usize,
    ) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let pipes = child.stdout.take().zip(child.stdin.take());
        match pipes {
            Some((stdout, stdin)) => Ok(Self {
                child: Some(child),
                io: Some(StdioIo::new(stdout, stdin, storage, max_frame)),
                exit_status: None,
            }),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                Err(io::Error::other(
                    "spawned MCP server is missing stdio pipes",
                ))
            }
        }
    }

    pub fn id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Observe exit without discarding output still buffered in the pipe.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let status = match self.child.as_mut() {
            Some(child) => child.try_wait()?,
            None => None,
        };
        self.exit_status = status;
        Ok(status)
    }

    pub fn replace_storage(
        &mut self,
        storage: &'a mut [u8],
    ) -> Result<(), TransportError<io::Error>> {
        self.io
            .as_mut()
            .ok_or(TransportError::Closed)?
            .replace_storage(storage)
    }

    /// Close stdin, allow a bounded grace period, then kill and reap the child.
    ///
    /// Pending protocol output is not consumed during shutdown. The grace
    /// period bounds graceful polling, not OS scheduling or the final reap.
    /// Repeated successful calls return the same status.
    pub fn close_gracefully(&mut self, grace: Duration) -> io::Result<ExitStatus> {
        let _stdout = self.io.take().map(|io| {
            let (reader, writer) = io.into_parts();
            drop(writer);
            reader
        });
        if let Some(status) = self.exit_status {
            self.child = None;
            return Ok(status);
        }
        let start = Instant::now();
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "MCP server is closed"))?;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() >= grace {
                if let Err(error) = child.kill() {
                    // The process may have exited between try_wait and kill.
                    if child.try_wait()?.is_none() {
                        return Err(error);
                    }
                }
                break child.wait()?;
            }
            thread::sleep(Duration::from_millis(5).min(grace.saturating_sub(start.elapsed())));
        };
        self.exit_status = Some(status);
        self.child = None;
        Ok(status)
    }

    /// Close pipes, terminate if still running, and reap the direct child.
    pub fn close(&mut self) -> io::Result<ExitStatus> {
        self.close_gracefully(Duration::ZERO)
    }
}

impl MessageReader for StdioTransport<'_> {
    type Error = io::Error;

    fn receive(&mut self, output: &mut [u8]) -> Result<usize, TransportError<Self::Error>> {
        self.io
            .as_mut()
            .ok_or(TransportError::Closed)?
            .receive(output)
    }
}

impl MessageWriter for StdioTransport<'_> {
    type Error = io::Error;

    fn send(&mut self, message: &[u8]) -> Result<(), TransportError<Self::Error>> {
        self.io
            .as_mut()
            .ok_or(TransportError::Closed)?
            .send(message)
    }
}

impl Drop for StdioTransport<'_> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
