//! Notification encoders. Applications own capability, role and cancellation policy.
use crate::{
    Error, ErrorKind,
    json::{Kind, Value},
    mcp::validate_progress_token,
    rpc::{self, Id},
    writer::Writer,
};

/// Finite nonnegative progress values retain their original JSON number lexeme.
/// Use Tracker to enforce per-operation monotonic progress across notifications.
pub fn progress(
    token: Value<'_>,
    value: Value<'_>,
    total: Option<Value<'_>>,
    message: Option<&str>,
    output: &mut [u8],
) -> Result<usize, Error> {
    validate_progress_token(token)?;
    number(value)?;
    if let Some(total) = total {
        number(total)?;
    }
    let mut w = Writer::new(output);
    w.raw(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progressToken\":");
    w.raw(token.raw().as_bytes());
    w.raw(b",\"progress\":");
    w.raw(value.raw().as_bytes());
    if let Some(total) = total {
        w.raw(b",\"total\":");
        w.raw(total.raw().as_bytes());
    }
    if let Some(message) = message {
        w.raw(b",\"message\":");
        w.text(message);
    }
    w.raw(b"}}");
    w.finish()
}
fn number(value: Value<'_>) -> Result<(), Error> {
    if value.kind() != Kind::Number {
        return Err(Error::new(ErrorKind::InvalidParams, value.offset()));
    }
    let n: f64 = value
        .raw()
        .parse()
        .map_err(|_| Error::new(ErrorKind::InvalidParams, value.offset()))?;
    if n.is_finite() && n >= 0.0 {
        Ok(())
    } else {
        Err(Error::new(ErrorKind::InvalidParams, value.offset()))
    }
}
/// Encoding does not itself cancel work or grant permission to cancel a peer's request.
pub fn cancelled(id: Id<'_>, reason: Option<&str>, output: &mut [u8]) -> Result<usize, Error> {
    let mut w = Writer::new(output);
    w.raw(
        b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":",
    );
    id.write(&mut w);
    if let Some(reason) = reason {
        w.raw(b",\"reason\":");
        w.text(reason);
    }
    w.raw(b"}}");
    w.finish()
}
/// Legacy handshake notification; not part of modern stateless discovery.
pub fn initialized(output: &mut [u8]) -> Result<usize, Error> {
    rpc::notification("notifications/initialized", None, output)
}
/// Caller must have negotiated the applicable list-change/subscription capability.
pub fn tools_list_changed(output: &mut [u8]) -> Result<usize, Error> {
    rpc::notification("notifications/tools/list_changed", None, output)
}
