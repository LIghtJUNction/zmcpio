//! Bounded legacy (2024-11-05) HTTP+SSE endpoint negotiation.
//!
//! The host opens the SSE connection; its first endpoint event selects the POST
//! target. This state machine never opens URLs, forwards credentials, reconnects
//! or replays requests. Only same-origin destinations are accepted. The origin
//! comparison includes scheme, effective port, and parsed IP/case-folded DNS host.
//!
//! Supported references: absolute HTTP(S) URLs, absolute paths, path-relative
//! references with literal dot segments, and query-only references. Percent-
//! encoded paths are preserved. Raw non-ASCII, fragments, userinfo, scheme-relative
//! references, scoped IPv6, escaped controls/backslashes, and percent-encoded dot
//! segments and repeated path slashes are rejected explicitly. The configured base path must already be
//! canonical (no dot segments). This is a conservative transport URL resolver,
//! not a browser navigation or redirect implementation.

use crate::sse::Event;
use core::net::{Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scheme {
    Http,
    Https,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidUri,
    UnsupportedReference,
    CrossOrigin,
    Capacity { needed: usize },
    MissingEndpoint,
    DuplicateEndpoint,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl core::error::Error for Error {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action<'a> {
    EndpointReady,
    /// Pass this complete JSON-RPC message to the MCP protocol decoder.
    Message(&'a [u8]),
    /// Heartbeat/metadata-only or unknown SSE event.
    Ignore,
}

#[derive(Clone, Copy)]
enum Host<'a> {
    Dns(&'a str),
    V4(Ipv4Addr),
    V6(Ipv6Addr),
}

#[derive(Clone, Copy)]
struct Url<'a> {
    scheme: Scheme,
    authority: &'a str,
    host: Host<'a>,
    port: u16,
    path: &'a str,
    query: &'a str,
}

impl Url<'_> {
    fn same_origin(self, other: Url<'_>) -> bool {
        self.scheme == other.scheme
            && self.port == other.port
            && match (self.host, other.host) {
                (Host::Dns(a), Host::Dns(b)) => a.eq_ignore_ascii_case(b),
                (Host::V4(a), Host::V4(b)) => a == b,
                (Host::V6(a), Host::V6(b)) => a == b,
                _ => false,
            }
    }
}

/// One SSE connection and its same-origin POST destination.
pub struct Session<'a> {
    base: Url<'a>,
    target: &'a mut [u8],
    used: usize,
    ready: bool,
}

impl<'a> Session<'a> {
    pub fn new(sse_url: &'a str, target_storage: &'a mut [u8]) -> Result<Self, Error> {
        let base = parse_url(sse_url)?;
        validate_path(base.path, true)?;
        Ok(Self { base, target: target_storage, used: 0, ready: false })
    }

    pub fn scheme(&self) -> Scheme {
        self.base.scheme
    }

    /// Original validated authority for the caller's HTTP Host header.
    pub fn authority(&self) -> &str {
        self.base.authority
    }

    /// Origin-form path and query, never a URL that can change the destination.
    pub fn endpoint_target(&self) -> Option<&str> {
        if self.ready {
            core::str::from_utf8(&self.target[..self.used]).ok()
        } else {
            None
        }
    }

    pub fn on_event<'e>(&mut self, event: Event<'e>) -> Result<Action<'e>, Error> {
        match event.event {
            "endpoint" => {
                if self.ready {
                    return Err(Error::DuplicateEndpoint);
                }
                let data = event.data.ok_or(Error::InvalidUri)?;
                let reference = core::str::from_utf8(data).map_err(|_| Error::InvalidUri)?;
                let used = resolve(self.base, reference, self.target)?.len();
                self.used = used;
                self.ready = true;
                Ok(Action::EndpointReady)
            }
            "message" | "" => {
                let Some(data) = event.data else {
                    return Ok(Action::Ignore);
                };
                if !self.ready {
                    return Err(Error::MissingEndpoint);
                }
                Ok(Action::Message(data))
            }
            _ => Ok(Action::Ignore),
        }
    }

    /// A reconnect requires a new endpoint event. No tool request is replayed.
    pub fn reset(&mut self) {
        self.target[..self.used].fill(0);
        self.used = 0;
        self.ready = false;
    }

    /// Grow storage without losing an accepted target. Failed replacements do
    /// not mutate state; the old used bytes are cleared after a successful copy.
    pub fn replace_storage(&mut self, storage: &'a mut [u8]) -> Result<(), Error> {
        if storage.len() < self.used {
            return Err(Error::Capacity { needed: self.used });
        }
        storage[..self.used].copy_from_slice(&self.target[..self.used]);
        self.target[..self.used].fill(0);
        self.target = storage;
        Ok(())
    }

    pub fn memory_bytes(&self) -> usize {
        core::mem::size_of::<Self>() + self.target.len()
    }
}

/// Resolve a validated same-origin endpoint to an origin-form POST target.
/// Failures leave output unchanged. Capacity includes temporary unnormalized
/// path bytes; the final returned target may be shorter after removing dots.
pub fn resolve_endpoint<'o>(
    sse_url: &str,
    reference: &str,
    output: &'o mut [u8],
) -> Result<&'o str, Error> {
    let base = parse_url(sse_url)?;
    validate_path(base.path, true)?;
    resolve(base, reference, output)
}

fn resolve<'o>(
    base: Url<'_>,
    reference: &str,
    output: &'o mut [u8],
) -> Result<&'o str, Error> {
    validate_uri(reference)?;
    if reference.is_empty() {
        return Err(Error::InvalidUri);
    }
    if reference.starts_with("//") {
        return Err(Error::UnsupportedReference);
    }
    let first_segment = reference.split(['/', '?']).next().unwrap_or("");
    let (prefix, path, query) = if first_segment.contains(':') {
        let endpoint = parse_url(reference)?;
        if !base.same_origin(endpoint) {
            return Err(Error::CrossOrigin);
        }
        ("", endpoint.path, endpoint.query)
    } else {
        let (path, query) = path_query(reference);
        if path.is_empty() {
            ("", base.path, query)
        } else if path.starts_with('/') {
            ("", path, query)
        } else {
            let directory = base.path.rfind('/').map_or("/", |at| &base.path[..at + 1]);
            (directory, path, query)
        }
    };
    validate_path(path, false)?;
    let path_len = prefix.len().checked_add(path.len()).ok_or(Error::InvalidUri)?;
    let needed = path_len.checked_add(query.len()).ok_or(Error::InvalidUri)?;
    if output.len() < needed {
        return Err(Error::Capacity { needed });
    }
    output[..prefix.len()].copy_from_slice(prefix.as_bytes());
    output[prefix.len()..path_len].copy_from_slice(path.as_bytes());
    let path_len = remove_dots(output, path_len);
    output[path_len..path_len + query.len()].copy_from_slice(query.as_bytes());
    core::str::from_utf8(&output[..path_len + query.len()]).map_err(|_| Error::InvalidUri)
}

fn path_query(value: &str) -> (&str, &str) {
    match value.find('?') {
        Some(at) => (&value[..at], &value[at..]),
        None => (value, ""),
    }
}

fn parse_url(input: &str) -> Result<Url<'_>, Error> {
    validate_uri(input)?;
    let (scheme, rest) = input.split_once("://").ok_or(Error::UnsupportedReference)?;
    let scheme = if scheme.eq_ignore_ascii_case("http") {
        Scheme::Http
    } else if scheme.eq_ignore_ascii_case("https") {
        Scheme::Https
    } else {
        return Err(Error::UnsupportedReference);
    };
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let (host, port) = parse_authority(
        authority,
        if scheme == Scheme::Https { 443 } else { 80 },
    )?;
    let (path, query) = path_query(&rest[end..]);
    Ok(Url {
        scheme,
        authority,
        host,
        port,
        path: if path.is_empty() { "/" } else { path },
        query,
    })
}

fn parse_authority(authority: &str, default_port: u16) -> Result<(Host<'_>, u16), Error> {
    if authority.is_empty() || authority.contains(['@', '%', '\\']) {
        return Err(Error::InvalidUri);
    }
    if authority.starts_with('[') {
        let end = authority.find(']').ok_or(Error::InvalidUri)?;
        let address = authority[1..end].parse::<Ipv6Addr>().map_err(|_| Error::InvalidUri)?;
        let suffix = &authority[end + 1..];
        let port = if suffix.is_empty() {
            default_port
        } else {
            parse_port(suffix.strip_prefix(':').ok_or(Error::InvalidUri)?)?
        };
        return Ok((Host::V6(address), port));
    }
    let (name, port) = match authority.rsplit_once(':') {
        Some((name, port)) => (name, parse_port(port)?),
        None => (authority, default_port),
    };
    if let Ok(address) = name.parse::<Ipv4Addr>() {
        return Ok((Host::V4(address), port));
    }
    // Do not treat malformed or noncanonical numeric IPs as DNS names.
    if name.bytes().all(|b| b.is_ascii_digit() || b == b'.') || !dns_name(name) {
        return Err(Error::InvalidUri);
    }
    Ok((Host::Dns(name), port))
}

fn parse_port(port: &str) -> Result<u16, Error> {
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::InvalidUri);
    }
    port.parse().map_err(|_| Error::InvalidUri)
}

fn dns_name(name: &str) -> bool {
    let labels = name.strip_suffix('.').unwrap_or(name);
    !labels.is_empty()
        && labels.len() <= 253
        && labels.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn validate_uri(value: &str) -> Result<(), Error> {
    let bytes = value.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if !(33..127).contains(&byte) || matches!(byte, b'#' | b'\\' | b'"' | b'<' | b'>' | b'^' | 0x60 | b'{' | b'|' | b'}') {
            return Err(Error::InvalidUri);
        }
        if byte == b'%' {
            let high = bytes.get(at + 1).and_then(|b| hex(*b)).ok_or(Error::InvalidUri)?;
            let low = bytes.get(at + 2).and_then(|b| hex(*b)).ok_or(Error::InvalidUri)?;
            let decoded = high * 16 + low;
            if decoded < 32 || decoded == 127 || decoded == b'\\' {
                return Err(Error::InvalidUri);
            }
            at += 3;
        } else {
            at += 1;
        }
    }
    Ok(())
}

fn validate_path(path: &str, canonical: bool) -> Result<(), Error> {
    // Removing /segment/.. must not expose a leading // authority reference.
    // Reject repeated slashes rather than silently changing resource identity.
    if path.contains("//") {
        return Err(Error::UnsupportedReference);
    }
    for segment in path.split('/') {
        let bytes = segment.as_bytes();
        let mut at = 0;
        let mut dots = 0;
        let mut encoded = false;
        while at < bytes.len() {
            if bytes[at] == b'.' {
                dots += 1;
                at += 1;
            } else if bytes[at] == b'%' && at + 2 < bytes.len()
                && hex(bytes[at + 1]) == Some(2) && hex(bytes[at + 2]) == Some(14)
            {
                dots += 1;
                at += 3;
                encoded = true;
            } else {
                break;
            }
        }
        if at == bytes.len() && matches!(dots, 1 | 2) && (encoded || canonical) {
            return Err(Error::UnsupportedReference);
        }
    }
    Ok(())
}

/// RFC 3986 section 5.2.4, performed in-place; writes only move toward the start.
fn remove_dots(output: &mut [u8], len: usize) -> usize {
    let mut read = 0;
    let mut written = 0;
    while read < len {
        let tail = &output[read..len];
        if tail.starts_with(b"../") {
            read += 3;
        } else if tail.starts_with(b"./") {
            read += 2;
        } else if tail.starts_with(b"/./") {
            read += 2;
        } else if tail == b"/." {
            output[written] = b'/';
            written += 1;
            read = len;
        } else if tail.starts_with(b"/../") {
            read += 3;
            written = output[..written].iter().rposition(|b| *b == b'/').unwrap_or(0);
        } else if tail == b"/.." {
            written = output[..written].iter().rposition(|b| *b == b'/').unwrap_or(0);
            output[written] = b'/';
            written += 1;
            read = len;
        } else if tail == b"." || tail == b".." {
            read = len;
        } else {
            let initial = usize::from(tail[0] == b'/');
            let take = tail[initial..].iter().position(|b| *b == b'/')
                .map_or(tail.len(), |at| initial + at);
            output.copy_within(read..read + take, written);
            written += take;
            read += take;
        }
    }
    written
}
