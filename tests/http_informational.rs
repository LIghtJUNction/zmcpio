#![cfg(feature = "std")]

use std::io::{BufReader, Cursor, ErrorKind, Read};
use zmcpio::http_io::{self, BodyReader};

#[test]
fn final_response_needs_no_informational_allowance_and_preserves_body() {
    let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcNEXT";
    for capacity in 1..16 {
        let mut reader = BufReader::with_capacity(capacity, Cursor::new(wire));
        let mut storage = [0; 256];
        let head = http_io::read_response_head(&mut reader, &mut storage, 0).unwrap();
        assert_eq!(head.status(), Some(200));
        assert_eq!(head.header("content-length"), Some("3"));
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
fn mixed_informational_heads_are_skipped_without_reading_final_body() {
    let wire = concat!(
        "HTTP/1.1 100 Continue\r\n\r\n",
        "HTTP/1.1 102 Processing\r\nX-Progress: pending\r\n\r\n",
        "HTTP/1.1 103 Early Hints\r\nLink: </mcp>; rel=preload\r\n\r\n",
        "HTTP/1.1 199 Extension\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nX-Final: yes\r\n\r\n",
        "abcNEXT",
    );
    for capacity in 1..16 {
        let mut reader = BufReader::with_capacity(capacity, Cursor::new(wire.as_bytes()));
        let mut storage = [0; 256];
        let head = http_io::read_response_head(&mut reader, &mut storage, 8).unwrap();
        assert_eq!(head.status(), Some(200));
        assert_eq!(head.header("x-final"), Some("yes"));
        assert_eq!(head.header("x-progress"), None);
        assert_eq!(head.header("link"), None);
        // Directly inspect the caller's reader: no body byte was consumed.
        let mut untouched = [0; 7];
        reader.read_exact(&mut untouched).unwrap();
        assert_eq!(&untouched, b"abcNEXT");
    }
}

#[test]
fn exactly_eight_informational_heads_are_allowed() {
    let mut wire = "HTTP/1.1 100 Continue\r\n\r\n".repeat(8);
    wire.push_str("HTTP/1.1 204 No Content\r\n\r\nNEXT");
    let mut reader = BufReader::with_capacity(3, Cursor::new(wire.as_bytes()));
    let mut storage = [0; 256];
    let head = http_io::read_response_head(&mut reader, &mut storage, 8).unwrap();
    assert_eq!(head.status(), Some(204));
    let mut next = [0; 4];
    reader.read_exact(&mut next).unwrap();
    assert_eq!(&next, b"NEXT");
}

#[test]
fn ninth_informational_head_exceeds_the_explicit_limit() {
    let mut wire = "HTTP/1.1 100 Continue\r\n\r\n".repeat(9);
    wire.push_str("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    let mut reader = BufReader::with_capacity(7, Cursor::new(wire.as_bytes()));
    let mut storage = [0; 256];
    let error = http_io::read_response_head(&mut reader, &mut storage, 8).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn zero_limit_rejects_any_informational_head() {
    for status in [
        "100 Continue",
        "102 Processing",
        "103 Early Hints",
        "199 Extension",
    ] {
        let wire = format!("HTTP/1.1 {status}\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let mut reader = Cursor::new(wire.as_bytes());
        let mut storage = [0; 256];
        let error = http_io::read_response_head(&mut reader, &mut storage, 0).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData, "{status}");
    }
}

#[test]
fn switching_protocols_is_never_skipped() {
    let wire = concat!(
        "HTTP/1.1 101 Switching Protocols\r\n",
        "Connection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
    );
    for limit in [0, 1, 8] {
        let mut reader = BufReader::with_capacity(5, Cursor::new(wire.as_bytes()));
        let mut storage = [0; 256];
        let error = http_io::read_response_head(&mut reader, &mut storage, limit).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }
}

#[test]
fn request_head_is_not_accepted_as_a_response() {
    let wire = b"POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n";
    let mut reader = Cursor::new(wire);
    let mut storage = [0; 256];
    let error = http_io::read_response_head(&mut reader, &mut storage, 8).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn missing_or_truncated_final_response_is_not_success() {
    for wire in [
        "HTTP/1.1 100 Continue\r\n\r\n",
        "HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 200",
    ] {
        let mut reader = BufReader::with_capacity(2, Cursor::new(wire.as_bytes()));
        let mut storage = [0; 256];
        let error = http_io::read_response_head(&mut reader, &mut storage, 8).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }
}

#[test]
fn every_head_is_subject_to_caller_capacity() {
    let wire = format!(
        "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nX-Fill: {}\r\nContent-Length: 0\r\n\r\n",
        "x".repeat(128)
    );
    let mut reader = BufReader::with_capacity(7, Cursor::new(wire.as_bytes()));
    let mut storage = [0; 64];
    let error = http_io::read_response_head(&mut reader, &mut storage, 8).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn final_http_error_is_returned_with_its_body() {
    let wire = concat!(
        "HTTP/1.1 100 Continue\r\n\r\n",
        "HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{}",
    );
    let mut reader = BufReader::with_capacity(5, Cursor::new(wire.as_bytes()));
    let mut storage = [0; 256];
    let head = http_io::read_response_head(&mut reader, &mut storage, 8).unwrap();
    assert_eq!(head.status(), Some(404));
    let mut body = BodyReader::new(&mut reader, head, 2).unwrap();
    let mut output = [0; 2];
    assert_eq!(http_io::read_body(&mut body, &mut output).unwrap(), 2);
    assert_eq!(&output, b"{}");
}
