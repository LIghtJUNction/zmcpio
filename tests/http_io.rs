#![cfg(feature = "std")]

use std::io::{BufReader, Cursor, Read};
use zmcpio::http::Header;
use zmcpio::http_io::{self, BodyReader};

#[test]
fn fixed_body_does_not_consume_next_response() {
    let bytes = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Type: application/json\r\n\r\nabcNEXT";
    let mut reader = BufReader::with_capacity(1, Cursor::new(bytes));
    let mut storage = [0; 512];
    let head = http_io::read_head(&mut reader, &mut storage).unwrap();
    assert_eq!(head.status(), Some(200));
    assert_eq!(head.header("CONTENT-type"), Some("application/json"));
    let mut body = BodyReader::new(&mut reader, head, 3).unwrap();
    let mut output = [0; 3];
    assert_eq!(http_io::read_body(&mut body, &mut output).unwrap(), 3);
    assert_eq!(&output, b"abc");
    let mut next = [0; 4];
    reader.read_exact(&mut next).unwrap();
    assert_eq!(&next, b"NEXT");
}

#[test]
fn chunked_body_and_trailers_are_bounded() {
    let bytes = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2;foo=bar\r\nab\r\n1\r\nc\r\n0\r\nX-Extra: yes\r\n\r\nNEXT";
    for capacity in 1..16 {
        let mut reader = BufReader::with_capacity(capacity, Cursor::new(bytes));
        let mut storage = [0; 512];
        let head = http_io::read_head(&mut reader, &mut storage).unwrap();
        let mut body = BodyReader::new(&mut reader, head, 3).unwrap();
        let mut output = [0; 3];
        assert_eq!(http_io::read_body(&mut body, &mut output).unwrap(), 3);
        assert_eq!(&output, b"abc");
        let mut next = [0; 4];
        reader.read_exact(&mut next).unwrap();
        assert_eq!(&next, b"NEXT");
    }
}

#[test]
fn reject_smuggling_and_header_injection() {
    for bytes in [
        "POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n",
        "POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: 1\r\ncontent-length: 1\r\n\r\n",
        "POST /mcp HTTP/1.1\r\nHost: x\r\n Bad: folded\r\n\r\n",
        "POST /mcp HTTP/1.1\nHost: x\n\n",
        "POST //evil/x HTTP/1.1\r\nHost: x\r\n\r\n",
        "POST /mcp HTTP/1.1\r\nHost: x\r\nOrigin: x\r\norigin: y\r\n\r\n",
    ] {
        let mut reader = Cursor::new(bytes.as_bytes());
        let mut head = [0; 512];
        assert!(http_io::read_head(&mut reader, &mut head).is_err(), "{bytes}");
    }
    for (host, target, headers) in [
        ("x\r\nBad: x", "/mcp", vec![]),
        ("x", "/a b", vec![]),
        ("x", "/mcp", vec![Header { name: "X-Test", value: "x\r\nBad: x" }]),
        ("x", "/mcp", vec![Header { name: "Content-Length", value: "9" }]),
    ] {
        let mut output = Vec::new();
        assert!(http_io::write_request(&mut output, "POST", target, host, &headers, b"{}").is_err());
        assert!(output.is_empty(), "validation must precede writes");
    }
}

#[test]
fn short_or_oversized_body_is_an_error() {
    for bytes in [
        "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nab",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\na",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nabcd\r\n0\r\n\r\n",
        "HTTP/1.1 200 OK\r\n\r\nabcd",
    ] {
        let mut reader = Cursor::new(bytes.as_bytes());
        let mut head = [0; 256];
        let head = http_io::read_head(&mut reader, &mut head).unwrap();
        match BodyReader::new(&mut reader, head, 3) {
            Err(_) => {}
            Ok(mut body) => {
                let mut output = [0; 3];
                assert!(http_io::read_body(&mut body, &mut output).is_err());
                assert!(body.read(&mut [0]).is_err());
            }
        }
    }
}

#[test]
fn request_and_response_roundtrip() {
    let mut output = Vec::new();
    let headers = [Header { name: "Content-Type", value: "application/json" }];
    http_io::write_request(&mut output, "POST", "/mcp?q=x", "localhost:8080", &headers, b"{}").unwrap();
    let mut reader = Cursor::new(output);
    let mut storage = [0; 512];
    let head = http_io::read_head(&mut reader, &mut storage).unwrap();
    assert_eq!(head.method(), Some("POST"));
    assert_eq!(head.target(), Some("/mcp?q=x"));
    assert_eq!(head.header("host"), Some("localhost:8080"));
    let mut views = [Header { name: "", value: "" }; 8];
    assert_eq!(head.copy_headers(&mut views).unwrap(), 4);
    assert_eq!(head.copy_headers(&mut []), Err(4));
    let mut body = BodyReader::new(&mut reader, head, 8).unwrap();
    let mut bytes = [0; 8];
    assert_eq!(http_io::read_body(&mut body, &mut bytes).unwrap(), 2);
    let mut output = Vec::new();
    http_io::write_response(&mut output, 202, &[], b"").unwrap();
    let mut reader = Cursor::new(output);
    let head = http_io::read_head(&mut reader, &mut storage).unwrap();
    assert_eq!(head.status(), Some(202));
}

#[test]
fn chunk_writer_is_readable_and_flushes_events() {
    let mut output = Vec::new();
    {
        let mut sse = http_io::start_sse(&mut output, &[]).unwrap();
        sse.send(b"data: a\n\n").unwrap();
        sse.send(b"").unwrap();
        sse.send(b"data: b\n\n").unwrap();
        sse.finish().unwrap();
    }
    let mut reader = Cursor::new(output);
    let mut storage = [0; 512];
    let head = http_io::read_head(&mut reader, &mut storage).unwrap();
    assert_eq!(head.header("content-type"), Some("text/event-stream"));
    let mut body = BodyReader::new(&mut reader, head, 128).unwrap();
    let mut bytes = [0; 128];
    let n = http_io::read_body(&mut body, &mut bytes).unwrap();
    assert_eq!(&bytes[..n], b"data: a\n\ndata: b\n\n");
}
