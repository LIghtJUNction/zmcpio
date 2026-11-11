#![cfg(feature = "std")]

use std::{
    io::{self, Cursor, Read, Write},
    process::Command,
    time::Duration,
};
use zmcpio::{
    stdio::{StdioIo, StdioTransport},
    transport::{MessageReader, MessageWriter, TransportError},
};

#[test]
fn preserves_frames_and_output_capacity_retries() {
    let mut storage = [0; 32];
    let mut io = StdioIo::new(
        Cursor::new(b"{\"a\":1}\n{\"b\":2}\r\n"),
        io::sink(),
        &mut storage,
        32,
    );
    assert!(matches!(
        io.receive(&mut [0; 2]),
        Err(TransportError::Capacity { needed: 7 })
    ));
    let mut output = [0; 32];
    let first = io.receive(&mut output).unwrap();
    assert_eq!(&output[..first], br#"{"a":1}"#);
    let second = io.receive(&mut output).unwrap();
    assert_eq!(&output[..second], br#"{"b":2}"#);
    assert!(matches!(
        io.receive(&mut output),
        Err(TransportError::Closed)
    ));
}

#[test]
fn frames_can_cross_internal_read_chunk_boundaries() {
    let mut wire = vec![b'x'; 5000];
    wire.extend_from_slice(b"\n{}\n");
    let mut storage = [0; 8192];
    let mut output = [0; 8192];
    let mut io = StdioIo::new(Cursor::new(wire), io::sink(), &mut storage, 8192);
    let len = io.receive(&mut output).unwrap();
    assert_eq!(len, 5000);
    assert!(output[..len].iter().all(|b| *b == b'x'));
    let len = io.receive(&mut output).unwrap();
    assert_eq!(&output[..len], b"{}");
}

#[test]
fn storage_capacity_can_be_replaced_without_losing_suffix() {
    let mut small = [0; 2];
    let mut large = [0; 32];
    let mut io = StdioIo::new(Cursor::new(b"123456\nnext\n"), io::sink(), &mut small, 32);
    let mut output = [0; 32];
    assert!(matches!(
        io.receive(&mut output),
        Err(TransportError::Capacity { needed: 3 })
    ));
    io.replace_storage(&mut large).unwrap();
    let len = io.receive(&mut output).unwrap();
    assert_eq!(&output[..len], b"123456");
    let len = io.receive(&mut output).unwrap();
    assert_eq!(&output[..len], b"next");
}

#[test]
fn frame_limit_is_terminal_and_partial_eof_is_not_a_message() {
    let mut storage = [0; 64];
    let mut output = [0; 64];
    let mut io = StdioIo::new(Cursor::new(b"12345\nnext\n"), io::sink(), &mut storage, 4);
    assert!(matches!(
        io.receive(&mut output),
        Err(TransportError::FrameTooLarge)
    ));
    assert!(matches!(
        io.receive(&mut output),
        Err(TransportError::Closed)
    ));

    let mut io = StdioIo::new(Cursor::new(b"partial"), io::sink(), &mut storage, 64);
    assert!(matches!(
        io.receive(&mut output),
        Err(TransportError::Incomplete)
    ));
    assert!(matches!(
        io.receive(&mut output),
        Err(TransportError::Closed)
    ));
}

struct Fragmented {
    bytes: &'static [u8],
    interrupted: bool,
    timeout: bool,
}
impl Read for Fragmented {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if !self.timeout && self.bytes.len() == 3 {
            self.timeout = true;
            return Err(io::ErrorKind::TimedOut.into());
        }
        match self.bytes.split_first() {
            Some((byte, rest)) => {
                output[0] = *byte;
                self.bytes = rest;
                Ok(1)
            }
            None => Ok(0),
        }
    }
}

#[test]
fn fragmented_io_survives_interrupt_and_timeout() {
    let mut storage = [0; 8];
    let reader = Fragmented {
        bytes: b"12345\n",
        interrupted: false,
        timeout: false,
    };
    let mut io = StdioIo::new(reader, io::sink(), &mut storage, 8);
    let mut output = [0; 8];
    let error = io.receive(&mut output).unwrap_err();
    assert!(matches!(error, TransportError::Io(e) if e.kind() == io::ErrorKind::TimedOut));
    let len = io.receive(&mut output).unwrap();
    assert_eq!(&output[..len], b"12345");
}

#[derive(Default)]
struct Recorder {
    bytes: Vec<u8>,
    flushes: usize,
}
impl Write for Recorder {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn writer_flushes_and_rejects_frame_injection_before_writing() {
    let mut storage = [0; 64];
    let mut io = StdioIo::new(io::empty(), Recorder::default(), &mut storage, 64);
    for invalid in [b"".as_slice(), b"{}\n{}", b"{}\r"] {
        assert!(
            matches!(io.send(invalid), Err(TransportError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput)
        );
    }
    assert!(matches!(
        io.send(&[b'x'; 65]),
        Err(TransportError::FrameTooLarge)
    ));
    io.send(br#"{"value":"escaped\nnewline"}"#).unwrap();
    let (_, writer) = io.into_parts();
    assert_eq!(writer.bytes, b"{\"value\":\"escaped\\nnewline\"}\n");
    assert_eq!(writer.flushes, 1);
}

struct BrokenWriter(bool);
impl Write for BrokenWriter {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        if !self.0 {
            self.0 = true;
            Ok(1)
        } else {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn partial_write_is_not_retryable() {
    let mut storage = [0; 16];
    let mut io = StdioIo::new(io::empty(), BrokenWriter(false), &mut storage, 16);
    assert!(
        matches!(io.send(b"{}"), Err(TransportError::Io(e)) if e.kind() == io::ErrorKind::BrokenPipe)
    );
    assert!(matches!(io.send(b"{}"), Err(TransportError::Closed)));
}

#[test]
fn failed_spawn_is_reported() {
    let mut storage = [0; 16];
    let mut command = Command::new("zmcpio-test-executable-that-does-not-exist-987342");
    assert!(StdioTransport::spawn(&mut command, &mut storage, 16).is_err());
}

#[test]
#[ignore = "child fixture launched explicitly by shutdown_closes_stdin_and_reaps"]
fn child_waits_for_eof() {
    let mut one = [0];
    while io::stdin().read(&mut one).unwrap() != 0 {}
}

#[test]
#[ignore = "child fixture launched explicitly by shutdown_terminates_uncooperative_child"]
fn child_ignores_eof() {
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn shutdown_terminates_uncooperative_child() {
    let mut storage = [0; 256];
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "child_ignores_eof", "--ignored", "--nocapture"]);
    let mut transport = StdioTransport::spawn(&mut command, &mut storage, 256).unwrap();
    let status = transport
        .close_gracefully(Duration::from_millis(20))
        .unwrap();
    assert!(!status.success());
    assert!(transport.id().is_none());
}

#[test]
fn shutdown_closes_stdin_and_reaps() {
    let mut storage = [0; 256];
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "child_waits_for_eof", "--ignored", "--nocapture"]);
    let mut transport = StdioTransport::spawn(&mut command, &mut storage, 256).unwrap();
    assert!(transport.id().is_some());
    let status = transport.close_gracefully(Duration::from_secs(5)).unwrap();
    assert!(status.success());
    assert_eq!(transport.close().unwrap(), status);
    assert!(transport.id().is_none());
    assert!(matches!(transport.send(b"{}"), Err(TransportError::Closed)));
    assert!(matches!(
        transport.receive(&mut [0; 32]),
        Err(TransportError::Closed)
    ));
}
