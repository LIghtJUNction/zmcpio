#![cfg(feature = "std")]

use std::{
    cell::Cell,
    io::{self, BufRead, Cursor, Read, Write},
    rc::Rc,
};
use zmcpio::http_io::{self, BodyReader, Connection};

#[test]
fn no_body_statuses_omit_content_length_and_preserve_next_response() {
    let mut wire = Vec::new();
    for status in [204, 304] {
        http_io::write_response_with(&mut wire, status, &[], b"", Connection::KeepAlive).unwrap();
    }
    http_io::write_response_with(&mut wire, 200, &[], b"ok", Connection::KeepAlive).unwrap();
    let mut reader = Cursor::new(wire);
    let mut storage = [0; 512];
    for status in [204, 304, 200] {
        let head = http_io::read_head(&mut reader, &mut storage).unwrap();
        assert_eq!(head.status(), Some(status));
        assert_eq!(head.header("connection"), Some("keep-alive"));
        assert_eq!(head.header("transfer-encoding"), None);
        assert_eq!(
            head.header("content-length"),
            if status == 200 { Some("2") } else { None }
        );
        let mut body = BodyReader::new(&mut reader, head, 8).unwrap();
        let mut output = [0; 8];
        let len = http_io::read_body(&mut body, &mut output).unwrap();
        if status == 200 {
            assert_eq!(&output[..len], b"ok");
        } else {
            assert_eq!(len, 0);
        }
    }
    for status in [204, 304] {
        let mut output = Vec::new();
        assert!(http_io::write_response(&mut output, status, &[], b"not permitted").is_err());
        assert!(output.is_empty());
    }
}

#[test]
fn keep_alive_requests_allow_safe_sequential_reuse() {
    let mut wire = Vec::new();
    for (path, body) in [("/first", b"one".as_slice()), ("/second", b"two")] {
        http_io::write_request_with(
            &mut wire,
            "POST",
            path,
            "localhost:8080",
            &[],
            body,
            Connection::KeepAlive,
        )
        .unwrap();
    }
    let mut reader = Cursor::new(wire);
    let mut storage = [0; 512];
    for (path, expected) in [("/first", b"one".as_slice()), ("/second", b"two")] {
        let head = http_io::read_head(&mut reader, &mut storage).unwrap();
        assert_eq!(head.target(), Some(path));
        assert_eq!(head.header("connection"), Some("keep-alive"));
        let mut body = BodyReader::new(&mut reader, head, 3).unwrap();
        let mut output = [0; 3];
        assert_eq!(http_io::read_body(&mut body, &mut output).unwrap(), 3);
        assert_eq!(&output, expected);
    }
}

struct FaultyWriter {
    bytes: Vec<u8>,
    budget: Rc<Cell<usize>>,
    fail_flush: Rc<Cell<bool>>,
    calls: Rc<Cell<usize>>,
}
impl Write for FaultyWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls.set(self.calls.get() + 1);
        if self.budget.get() == 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let len = bytes.len().min(self.budget.get());
        self.bytes.extend_from_slice(&bytes[..len]);
        self.budget.set(self.budget.get() - len);
        Ok(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.calls.set(self.calls.get() + 1);
        if self.fail_flush.get() {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn chunk_writer_is_poisoned_after_partial_write_or_failed_flush() {
    for fail_on_flush in [false, true] {
        let budget = Rc::new(Cell::new(usize::MAX));
        let fail_flush = Rc::new(Cell::new(false));
        let calls = Rc::new(Cell::new(0));
        let mut writer = FaultyWriter {
            bytes: Vec::new(),
            budget: budget.clone(),
            fail_flush: fail_flush.clone(),
            calls: calls.clone(),
        };
        let mut chunks = http_io::start_sse(&mut writer, &[]).unwrap();
        if fail_on_flush {
            fail_flush.set(true);
        } else {
            budget.set(2); // Only a prefix of the chunk header reaches the peer.
        }
        assert!(chunks.send(b"data: hello\n\n").is_err());
        let failed_calls = calls.get();
        budget.set(usize::MAX);
        fail_flush.set(false);
        assert!(chunks.send(b"data: do-not-send\n\n").is_err());
        assert!(chunks.send(b"").is_err());
        assert_eq!(calls.get(), failed_calls);
        assert!(chunks.finish().is_err());
        assert_eq!(calls.get(), failed_calls);
    }
}

#[test]
fn sse_keepalive_finishes_before_next_response() {
    let mut wire = Vec::new();
    {
        let mut chunks = http_io::start_sse_with(&mut wire, &[], Connection::KeepAlive).unwrap();
        chunks.send(b"data: {}\n\n").unwrap();
        chunks.finish().unwrap();
    }
    http_io::write_response_with(&mut wire, 202, &[], b"", Connection::KeepAlive).unwrap();
    let mut reader = Cursor::new(wire);
    let mut storage = [0; 512];
    let head = http_io::read_head(&mut reader, &mut storage).unwrap();
    assert_eq!(head.header("connection"), Some("keep-alive"));
    let mut body = BodyReader::new(&mut reader, head, 32).unwrap();
    let mut output = [0; 32];
    let len = http_io::read_body(&mut body, &mut output).unwrap();
    assert_eq!(&output[..len], b"data: {}\n\n");
    let head = http_io::read_head(&mut reader, &mut storage).unwrap();
    assert_eq!(head.status(), Some(202));
}

#[test]
fn forbidden_chunk_extension_controls_are_terminal() {
    for byte in [0_u8, 1, b'\r', 0x1f, 0x7f, 0xff] {
        let mut wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1;name=".to_vec();
        wire.push(byte);
        wire.extend_from_slice(b"\r\nx\r\n0\r\n\r\n");
        let mut reader = Cursor::new(wire);
        let mut storage = [0; 256];
        let head = http_io::read_head(&mut reader, &mut storage).unwrap();
        let mut body = BodyReader::new(&mut reader, head, 16).unwrap();
        assert!(body.read(&mut [0; 16]).is_err(), "{byte}");
        assert!(
            body.read(&mut [0; 16]).is_err(),
            "failed body must stay failed"
        );
    }
}

/// Inject a signal interruption before every successful read/fill operation.
struct Interrupting<'a> {
    bytes: &'a [u8],
    at: usize,
    read_interrupt: bool,
    fill_interrupt: bool,
}
impl<'a> Interrupting<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            read_interrupt: true,
            fill_interrupt: true,
        }
    }
}
impl Read for Interrupting<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.read_interrupt {
            self.read_interrupt = false;
            return Err(io::ErrorKind::Interrupted.into());
        }
        let len = output.len().min(3).min(self.bytes.len() - self.at);
        output[..len].copy_from_slice(&self.bytes[self.at..self.at + len]);
        self.at += len;
        self.read_interrupt = true;
        self.fill_interrupt = true;
        Ok(len)
    }
}
impl BufRead for Interrupting<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.fill_interrupt {
            self.fill_interrupt = false;
            return Err(io::ErrorKind::Interrupted.into());
        }
        let end = (self.at + 3).min(self.bytes.len());
        Ok(&self.bytes[self.at..end])
    }
    fn consume(&mut self, amount: usize) {
        self.at += amount;
        self.fill_interrupt = true;
    }
}

#[test]
fn interrupted_heads_and_all_body_framings_are_retried() {
    for wire in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".as_slice(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n1\r\nc\r\n0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n\r\nabc",
    ] {
        let mut reader = Interrupting::new(wire);
        let mut storage = [0; 256];
        let head = http_io::read_head(&mut reader, &mut storage).unwrap();
        let mut body = BodyReader::new(&mut reader, head, 3).unwrap();
        let mut output = [0; 3];
        assert_eq!(http_io::read_body(&mut body, &mut output).unwrap(), 3);
        assert_eq!(&output, b"abc");
    }
}

#[test]
fn generic_body_helper_retries_interrupted_reads_including_extra_byte_probe() {
    for capacity in [3, 8] {
        let mut reader = Interrupting::new(b"abc");
        let mut output = [0; 8];
        assert_eq!(
            http_io::read_body(&mut reader, &mut output[..capacity]).unwrap(),
            3
        );
        assert_eq!(&output[..3], b"abc");
    }
    let mut reader = Interrupting::new(b"abcd");
    assert!(http_io::read_body(&mut reader, &mut [0; 3]).is_err());
}
