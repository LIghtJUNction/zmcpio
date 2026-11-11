//! Bounded blocking HTTP/1.1 I/O over caller-supplied streams.
//!
//! Wrap sockets in a buffered reader; TLS streams implementing Read + Write work
//! unchanged. This module does not resolve URLs, follow redirects, negotiate TLS,
//! retry requests, or execute tools. Configure deadlines on the supplied stream.
//! Close the connection after any parse/write error; partial sends are ambiguous.
use crate::http::Header;
use std::io::{self, BufRead, Read, Write};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn capacity() -> io::Error {
    invalid("HTTP message exceeds caller capacity or configured limit")
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_\x60|~".contains(&b))
}
fn field_value(value: &str) -> bool {
    value.bytes().all(|b| b == b'\t' || (b >= 32 && b != 127))
}
fn target_valid(value: &str) -> bool {
    value.starts_with('/')
        && !value.starts_with("//")
        && !value.contains('#')
        && value.bytes().all(|b| (33..127).contains(&b))
}

/// Validated, borrowed head. Header names are matched case-insensitively.
#[derive(Clone, Copy, Debug)]
pub struct Head<'a> {
    raw: &'a str,
    first_end: usize,
    status: Option<u16>,
    method: Option<&'a str>,
    target: Option<&'a str>,
}
impl<'a> Head<'a> {
    pub fn status(self) -> Option<u16> {
        self.status
    }
    pub fn method(self) -> Option<&'a str> {
        self.method
    }
    pub fn target(self) -> Option<&'a str> {
        self.target
    }
    pub fn headers(self) -> impl Iterator<Item = Header<'a>> {
        self.raw[self.first_end + 2..]
            .split("\r\n")
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some(Header {
                    name,
                    value: value.trim_matches([' ', '\t']),
                })
            })
    }
    pub fn header(self, name: &str) -> Option<&'a str> {
        self.headers()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value)
    }
    /// Copy only header views, never header values. Returns exact required slots.
    pub fn copy_headers(self, output: &mut [Header<'a>]) -> Result<usize, usize> {
        let needed = self.headers().count();
        if needed > output.len() {
            return Err(needed);
        }
        for (dst, header) in output.iter_mut().zip(self.headers()) {
            *dst = header;
        }
        Ok(needed)
    }
}

/// Convert an already encoded header block into caller-owned borrowed views.
/// The block must end each header with CRLF and must not contain a blank line.
pub fn header_views<'a>(encoded: &'a str, output: &mut [Header<'a>]) -> io::Result<usize> {
    if !encoded.is_empty() && !encoded.ends_with("\r\n") {
        return Err(invalid("incomplete encoded header block"));
    }
    let mut count = 0;
    for line in encoded.split_terminator("\r\n") {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("invalid encoded header block"))?;
        if !token(name) || !field_value(value) {
            return Err(invalid("invalid encoded header"));
        }
        let Some(slot) = output.get_mut(count) else {
            return Err(capacity());
        };
        *slot = Header {
            name,
            value: value.trim_matches([' ', '\t']),
        };
        count += 1;
    }
    Ok(count)
}

fn read_line<R: BufRead>(reader: &mut R, output: &mut [u8]) -> io::Result<usize> {
    let mut used = 0usize;
    loop {
        let input = match reader.fill_buf() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if input.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete HTTP line",
            ));
        }
        let end = input.iter().position(|b| *b == b'\n');
        let take = end.map_or(input.len(), |at| at + 1);
        let next = used.checked_add(take).ok_or_else(capacity)?;
        if next > output.len() {
            return Err(capacity());
        }
        output[used..next].copy_from_slice(&input[..take]);
        reader.consume(take);
        used = next;
        if end.is_some() {
            if used < 2 || output[used - 2] != b'\r' {
                return Err(invalid("HTTP requires CRLF line endings"));
            }
            return Ok(used);
        }
    }
}

/// Read exactly one head without consuming body bytes from the buffered reader.
pub fn read_head<'a, R: BufRead>(reader: &mut R, output: &'a mut [u8]) -> io::Result<Head<'a>> {
    let mut used = 0usize;
    loop {
        let n = read_line(reader, &mut output[used..])?;
        used += n;
        if n == 2 {
            break;
        }
    }
    let raw =
        core::str::from_utf8(&output[..used]).map_err(|_| invalid("HTTP head is not UTF-8"))?;
    parse_head(raw)
}

fn parse_head(raw: &str) -> io::Result<Head<'_>> {
    let used = raw.len();
    let first_end = raw
        .find("\r\n")
        .ok_or_else(|| invalid("missing HTTP start"))?;
    let mut parts = raw[..first_end].splitn(3, ' ');
    let first = parts.next().unwrap_or("");
    let second = parts.next().unwrap_or("");
    let third = parts.next().unwrap_or("");
    let (status, method, target) = if first == "HTTP/1.1" || first == "HTTP/1.0" {
        if second.len() != 3 || !second.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid("invalid HTTP status"));
        }
        let code: u16 = second.parse().map_err(|_| invalid("invalid HTTP status"))?;
        if !(100..600).contains(&code) || !field_value(third) {
            return Err(invalid("invalid HTTP status line"));
        }
        (Some(code), None, None)
    } else {
        if !token(first) || !target_valid(second) || !matches!(third, "HTTP/1.1" | "HTTP/1.0") {
            return Err(invalid("invalid HTTP request line"));
        }
        (None, Some(first), Some(second))
    };
    let head = Head {
        raw,
        first_end,
        status,
        method,
        target,
    };
    for line in raw[first_end + 2..used - 2].split("\r\n") {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("invalid HTTP field"))?;
        if !token(name) || !field_value(value) {
            return Err(invalid("invalid HTTP header"));
        }
    }
    for name in [
        "content-length",
        "transfer-encoding",
        "content-type",
        "content-encoding",
        "host",
        "authorization",
        "origin",
        "mcp-protocol-version",
        "mcp-session-id",
        "mcp-method",
        "mcp-name",
        "last-event-id",
    ] {
        if head
            .headers()
            .filter(|h| h.name.eq_ignore_ascii_case(name))
            .count()
            > 1
        {
            return Err(invalid("duplicate security or framing header"));
        }
    }
    if (first == "HTTP/1.0" || third == "HTTP/1.0") && head.header("transfer-encoding").is_some() {
        return Err(invalid("HTTP/1.0 does not support Transfer-Encoding"));
    }
    if head.header("content-length").is_some() && head.header("transfer-encoding").is_some() {
        return Err(invalid("ambiguous HTTP body framing"));
    }
    if method.is_some() && first != "HTTP/1.0" && third == "HTTP/1.1" {
        let host = head.header("host").ok_or_else(|| invalid("missing Host"))?;
        if host.is_empty()
            || !host.bytes().all(|b| (33..127).contains(&b))
            || host.contains(['/', '\\', '@', '#', '?'])
        {
            return Err(invalid("empty Host"));
        }
    }
    Ok(head)
}

/// Read a final response while skipping at most `max_informational` 1xx heads.
/// Protocol upgrades are unsupported. Configure a read deadline on the stream;
/// this count bounds interim-message work but does not replace a time limit.
pub fn read_response_head<'a, R: BufRead>(
    reader: &mut R,
    output: &'a mut [u8],
    max_informational: usize,
) -> io::Result<Head<'a>> {
    let mut informational = 0usize;
    let used = loop {
        let head = read_head(reader, output)?;
        let status = head
            .status()
            .ok_or_else(|| invalid("expected HTTP response"))?;
        if status >= 200 {
            break head.raw.len();
        }
        if status == 101 {
            return Err(invalid("HTTP protocol upgrades are unsupported"));
        }
        if informational >= max_informational {
            return Err(invalid("too many informational HTTP responses"));
        }
        informational += 1;
    };
    // Reborrow only after the loop, so no previous iteration retains the buffer.
    let raw =
        core::str::from_utf8(&output[..used]).map_err(|_| invalid("HTTP head is not UTF-8"))?;
    parse_head(raw)
}

#[derive(Clone, Copy, Debug)]
enum Framing {
    Length(usize),
    Chunked {
        remaining: usize,
        crlf: bool,
        done: bool,
    },
    Eof,
}

/// Decode Content-Length, chunked, or close-delimited bodies with a hard byte cap.
/// This decoder never reads beyond a framed body into the next HTTP message.
/// A limit/framing failure is terminal for this instance and its connection.
pub struct BodyReader<'a, R> {
    reader: &'a mut R,
    framing: Framing,
    read: usize,
    limit: usize,
    failed: bool,
}
impl<'a, R: BufRead> BodyReader<'a, R> {
    pub fn new(reader: &'a mut R, head: Head<'_>, limit: usize) -> io::Result<Self> {
        let framing = if head.status.is_some_and(|s| s < 200 || s == 204 || s == 304) {
            Framing::Length(0)
        } else if let Some(encoding) = head.header("transfer-encoding") {
            if !encoding.eq_ignore_ascii_case("chunked") {
                return Err(invalid("unsupported HTTP transfer coding"));
            }
            Framing::Chunked {
                remaining: 0,
                crlf: false,
                done: false,
            }
        } else if let Some(length) = head.header("content-length") {
            if length.is_empty() || !length.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid("invalid Content-Length"));
            }
            let length = length
                .parse()
                .map_err(|_| invalid("Content-Length overflow"))?;
            if length > limit {
                return Err(capacity());
            }
            Framing::Length(length)
        } else if head.method.is_some() {
            Framing::Length(0)
        } else {
            Framing::Eof
        };
        if head
            .header("content-encoding")
            .is_some_and(|s| !s.eq_ignore_ascii_case("identity"))
        {
            return Err(invalid("compressed HTTP bodies are not enabled"));
        }
        Ok(Self {
            reader,
            framing,
            read: 0,
            limit,
            failed: false,
        })
    }
    fn next_chunk(&mut self) -> io::Result<()> {
        let Framing::Chunked {
            remaining,
            crlf,
            done,
        } = self.framing
        else {
            return Ok(());
        };
        if remaining != 0 || done {
            return Ok(());
        }
        if crlf {
            let mut end = [0; 2];
            self.reader.read_exact(&mut end)?;
            if end != *b"\r\n" {
                return Err(invalid("invalid chunk terminator"));
            }
        }
        let mut line = [0; 1024];
        let n = read_line(self.reader, &mut line)?;
        let chunk_line = &line[..n - 2];
        if !chunk_line
            .iter()
            .all(|b| *b == b'\t' || (32..127).contains(b))
        {
            return Err(invalid("invalid chunk extension characters"));
        }
        let size = chunk_line.split(|b| *b == b';').next().unwrap_or(b"");
        if size.is_empty() || !size.iter().all(u8::is_ascii_hexdigit) {
            return Err(invalid("invalid chunk size"));
        }
        let mut length = 0usize;
        for digit in size {
            let digit = match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                _ => digit - b'A' + 10,
            };
            length = length
                .checked_mul(16)
                .and_then(|n| n.checked_add(usize::from(digit)))
                .ok_or_else(capacity)?;
        }
        if length > self.limit.saturating_sub(self.read) {
            return Err(capacity());
        }
        if length == 0 {
            // Trailers are never used as authorization, session, or framing data.
            let mut total = 0usize;
            loop {
                let n = read_line(self.reader, &mut line)?;
                total = total.checked_add(n).ok_or_else(capacity)?;
                if total > 8192 {
                    return Err(capacity());
                }
                if n == 2 {
                    break;
                }
                let raw = core::str::from_utf8(&line[..n - 2])
                    .map_err(|_| invalid("invalid HTTP trailer"))?;
                let (name, value) = raw
                    .split_once(':')
                    .ok_or_else(|| invalid("invalid HTTP trailer"))?;
                if !token(name) || !field_value(value) {
                    return Err(invalid("invalid HTTP trailer"));
                }
            }
        }
        self.framing = Framing::Chunked {
            remaining: length,
            crlf: length != 0,
            done: length == 0,
        };
        Ok(())
    }
    fn read_body(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        self.next_chunk()?;
        let remaining = match self.framing {
            Framing::Length(n) | Framing::Chunked { remaining: n, .. } => n,
            Framing::Eof => {
                let eof = loop {
                    match self.reader.fill_buf() {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        result => break result?.is_empty(),
                    }
                };
                if eof {
                    return Ok(0);
                }
                self.limit.saturating_sub(self.read)
            }
        };
        if remaining == 0 {
            if matches!(self.framing, Framing::Eof) {
                return Err(capacity());
            }
            return Ok(0);
        }
        let take = output
            .len()
            .min(remaining)
            .min(self.limit.saturating_sub(self.read));
        if take == 0 {
            return Err(capacity());
        }
        let n = loop {
            match self.reader.read(&mut output[..take]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if n == 0 {
            if matches!(self.framing, Framing::Eof) {
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated HTTP body",
            ));
        }
        self.read += n;
        match &mut self.framing {
            Framing::Length(left)
            | Framing::Chunked {
                remaining: left, ..
            } => *left -= n,
            Framing::Eof => {}
        }
        Ok(n)
    }
}
impl<R: BufRead> Read for BodyReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.failed {
            return Err(invalid("HTTP body reader has failed; close connection"));
        }
        let result = self.read_body(output);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

/// Read a bounded body completely; reject overflow, including one extra byte.
/// For SSE use incremental reads instead, so long-lived streams are not buffered.
fn read_retry<R: Read>(reader: &mut R, output: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(output) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

pub fn read_body<R: Read>(reader: &mut R, output: &mut [u8]) -> io::Result<usize> {
    let mut used = 0usize;
    while used < output.len() {
        let n = read_retry(reader, &mut output[used..])?;
        if n == 0 {
            return Ok(used);
        }
        used += n;
    }
    let mut extra = [0];
    if read_retry(reader, &mut extra)? != 0 {
        return Err(capacity());
    }
    Ok(used)
}

fn validate_headers(headers: &[Header<'_>], request: bool, sse: bool) -> io::Result<()> {
    for (at, header) in headers.iter().enumerate() {
        if !token(header.name) || !field_value(header.value) {
            return Err(invalid("invalid outgoing HTTP header"));
        }
        if ["content-length", "transfer-encoding", "connection"]
            .iter()
            .any(|name| header.name.eq_ignore_ascii_case(name))
            || (request && header.name.eq_ignore_ascii_case("host"))
            || (sse && header.name.eq_ignore_ascii_case("content-type"))
        {
            return Err(invalid("HTTP framing headers are owned by the writer"));
        }
        if headers[..at]
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case(header.name))
        {
            return Err(invalid("duplicate outgoing HTTP header"));
        }
    }
    Ok(())
}
fn write_headers<W: Write>(writer: &mut W, headers: &[Header<'_>]) -> io::Result<()> {
    for header in headers {
        write!(writer, "{}: {}\r\n", header.name, header.value)?;
    }
    Ok(())
}

/// Explicit connection reuse policy. Reuse only after consuming the complete body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Connection {
    Close,
    KeepAlive,
}
impl Connection {
    fn header(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::KeepAlive => "keep-alive",
        }
    }
}

/// No redirects/retries. The caller must not send bearer credentials over plaintext.
pub fn write_request<W: Write>(
    writer: &mut W,
    method: &str,
    target: &str,
    host: &str,
    headers: &[Header<'_>],
    body: &[u8],
) -> io::Result<()> {
    write_request_with(
        writer,
        method,
        target,
        host,
        headers,
        body,
        Connection::Close,
    )
}

pub fn write_request_with<W: Write>(
    writer: &mut W,
    method: &str,
    target: &str,
    host: &str,
    headers: &[Header<'_>],
    body: &[u8],
    connection: Connection,
) -> io::Result<()> {
    if !token(method)
        || !target_valid(target)
        || host.is_empty()
        || !host.bytes().all(|b| (33..127).contains(&b))
        || host.contains(['/', '\\', '@', '#', '?'])
    {
        return Err(invalid("invalid outgoing HTTP request target or host"));
    }
    validate_headers(headers, true, false)?;
    write!(writer, "{method} {target} HTTP/1.1\r\nHost: {host}\r\n")?;
    write_headers(writer, headers)?;
    write!(
        writer,
        "Content-Length: {}\r\nConnection: {}\r\n\r\n",
        body.len(),
        connection.header()
    )?;
    writer.write_all(body)?;
    writer.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Response",
    }
}
pub fn write_response<W: Write>(
    writer: &mut W,
    status: u16,
    headers: &[Header<'_>],
    body: &[u8],
) -> io::Result<()> {
    write_response_with(writer, status, headers, body, Connection::Close)
}

pub fn write_response_with<W: Write>(
    writer: &mut W,
    status: u16,
    headers: &[Header<'_>],
    body: &[u8],
    connection: Connection,
) -> io::Result<()> {
    if !(200..600).contains(&status) || ((status == 204 || status == 304) && !body.is_empty()) {
        return Err(invalid("invalid outgoing response"));
    }
    validate_headers(headers, false, false)?;
    write!(writer, "HTTP/1.1 {status} {}\r\n", reason(status))?;
    write_headers(writer, headers)?;
    if status != 204 && status != 304 {
        write!(writer, "Content-Length: {}\r\n", body.len())?;
    }
    write!(writer, "Connection: {}\r\n\r\n", connection.header())?;
    writer.write_all(body)?;
    writer.flush()
}

/// Start a chunked SSE response. Flush each event to preserve streaming latency.
pub fn start_sse<'a, W: Write>(
    writer: &'a mut W,
    headers: &[Header<'_>],
) -> io::Result<ChunkWriter<'a, W>> {
    start_sse_with(writer, headers, Connection::Close)
}

pub fn start_sse_with<'a, W: Write>(
    writer: &'a mut W,
    headers: &[Header<'_>],
    connection: Connection,
) -> io::Result<ChunkWriter<'a, W>> {
    validate_headers(headers, false, true)?;
    writer.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n")?;
    write_headers(writer, headers)?;
    write!(
        writer,
        "Transfer-Encoding: chunked\r\nConnection: {}\r\n\r\n",
        connection.header()
    )?;
    writer.flush()?;
    Ok(ChunkWriter {
        writer,
        failed: false,
    })
}
pub struct ChunkWriter<'a, W> {
    writer: &'a mut W,
    failed: bool,
}
impl<W: Write> ChunkWriter<'_, W> {
    /// Each supplied fragment may contain one or more complete SSE events.
    pub fn send(&mut self, data: &[u8]) -> io::Result<()> {
        if self.failed {
            return Err(invalid("chunk writer failed; close connection"));
        }
        let result = (|| {
            if !data.is_empty() {
                write!(self.writer, "{:x}\r\n", data.len())?;
                self.writer.write_all(data)?;
                self.writer.write_all(b"\r\n")?;
                self.writer.flush()?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub fn finish(self) -> io::Result<()> {
        if self.failed {
            return Err(invalid("chunk writer failed; close connection"));
        }
        self.writer.write_all(b"0\r\n\r\n")?;
        self.writer.flush()
    }
}
