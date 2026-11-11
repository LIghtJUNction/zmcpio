//! JSON-RPC 2.0 message classification under the MCP ID profile.
//! Requests use string or signed-integer IDs. Null IDs occur only in errors.
use crate::{Error, ErrorKind, json::{JsonString, Kind, Value}, writer::Writer};

#[derive(Clone, Copy, Debug)]
pub enum Id<'a> { String(JsonString<'a>), Number(i64) }
impl<'a> Id<'a> {
    pub fn from_value(value: Value<'a>) -> Result<Self, Error> {
        if let Some(s) = value.string() { return Ok(Self::String(s)); }
        value.integer().map(Self::Number).map_err(|_| Error::new(ErrorKind::InvalidRpc, value.offset()))
    }
    pub fn equals(self, other: Self) -> bool {
        match (self, other) {
            (Self::String(a), Self::String(b)) => a.equals(b),
            (Self::Number(a), Self::Number(b)) => a == b,
            _ => false,
        }
    }
    pub(crate) fn write(self, w: &mut Writer<'_>) {
        match self {
            Self::Number(n) => w.integer(n),
            Self::String(s) => {
                w.raw(b"\"");
                // Encode characters, not a reinterpreted escaped slice.
                for c in s.chars() {
                    let mut b = [0; 4];
                    let t = c.encode_utf8(&mut b);
                    match c {
                        '"' => w.raw(b"\\\""), '\\' => w.raw(b"\\\\"),
                        '\n' => w.raw(b"\\n"), '\r' => w.raw(b"\\r"), '\t' => w.raw(b"\\t"),
                        c if c < ' ' => {
                            let h = b"0123456789abcdef";
                            w.raw(&[b'\\', b'u', b'0', b'0', h[(c as usize) >> 4], h[(c as usize) & 15]]);
                        }
                        _ => w.raw(t.as_bytes()),
                    }
                }
                w.raw(b"\"");
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Request<'a> { pub id: Id<'a>, pub method: JsonString<'a>, pub params: Option<Value<'a>> }
#[derive(Clone, Copy, Debug)]
pub struct Notification<'a> { pub method: JsonString<'a>, pub params: Option<Value<'a>> }
#[derive(Clone, Copy, Debug)]
pub struct RpcError<'a> { pub code: i32, pub message: JsonString<'a>, pub data: Option<Value<'a>> }
#[derive(Clone, Copy, Debug)]
pub enum Message<'a> {
    Request(Request<'a>),
    Notification(Notification<'a>),
    Success { id: Id<'a>, result: Value<'a> },
    Failure { id: Option<Id<'a>>, error: RpcError<'a> },
}

pub fn parse(value: Value<'_>) -> Result<Message<'_>, Error> {
    let bad = || Error::new(ErrorKind::InvalidRpc, value.offset());
    if value.kind() != Kind::Object || !value.get("jsonrpc").and_then(Value::string).is_some_and(|s| s.matches("2.0")) { return Err(bad()); }
    let method = value.get("method");
    let result = value.get("result");
    let error = value.get("error");
    let id = value.get("id");
    let params = value.get("params");
    if let Some(method) = method {
        if result.is_some() || error.is_some() { return Err(bad()); }
        let method = method.string().ok_or_else(bad)?;
        if params.is_some_and(|p| !matches!(p.kind(), Kind::Object | Kind::Array)) { return Err(bad()); }
        return match id {
            Some(id) => Ok(Message::Request(Request { id: Id::from_value(id)?, method, params })),
            None => Ok(Message::Notification(Notification { method, params })),
        };
    }
    if params.is_some() { return Err(bad()); }
    let id = id.ok_or_else(bad)?;
    match (result, error) {
        (Some(result), None) => Ok(Message::Success { id: Id::from_value(id)?, result }),
        (None, Some(error)) if error.kind() == Kind::Object => {
            let code = error.get("code").ok_or_else(bad)?.integer().map_err(|_| bad())?;
            let message = error.get("message").and_then(Value::string).ok_or_else(bad)?;
            Ok(Message::Failure {
                id: if id.kind() == Kind::Null { None } else { Some(Id::from_value(id)?) },
                error: RpcError { code: i32::try_from(code).map_err(|_| bad())?, message, data: error.get("data") },
            })
        }
        _ => Err(bad()),
    }
}

pub(crate) fn prefix(w: &mut Writer<'_>, id: Option<Id<'_>>) {
    w.raw(b"{\"jsonrpc\":\"2.0\",\"id\":");
    if let Some(id) = id { id.write(w); } else { w.raw(b"null"); }
}
/// Partial output must be discarded on error. `needed` gives the full wire size.
pub fn success(id: Id<'_>, result: Value<'_>, output: &mut [u8]) -> Result<usize, Error> {
    let mut w = Writer::new(output);
    prefix(&mut w, Some(id)); w.raw(b",\"result\":"); w.raw(result.raw().as_bytes()); w.raw(b"}"); w.finish()
}
pub fn failure(id: Option<Id<'_>>, code: i32, message: &str, data: Option<Value<'_>>, output: &mut [u8]) -> Result<usize, Error> {
    let mut w = Writer::new(output);
    prefix(&mut w, id); w.raw(b",\"error\":{\"code\":"); w.integer(i64::from(code));
    w.raw(b",\"message\":"); w.text(message);
    if let Some(data) = data { w.raw(b",\"data\":"); w.raw(data.raw().as_bytes()); }
    w.raw(b"}}"); w.finish()
}
pub fn notification(method: &str, params: Option<Value<'_>>, output: &mut [u8]) -> Result<usize, Error> {
    if params.is_some_and(|v| !matches!(v.kind(), Kind::Object | Kind::Array)) { return Err(Error::new(ErrorKind::Type, 0)); }
    let mut w = Writer::new(output);
    w.raw(b"{\"jsonrpc\":\"2.0\",\"method\":"); w.text(method);
    if let Some(p) = params { w.raw(b",\"params\":"); w.raw(p.raw().as_bytes()); }
    w.raw(b"}"); w.finish()
}
