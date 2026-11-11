use crate::{Error, ErrorKind};

pub(crate) struct Writer<'a> { output: &'a mut [u8], used: usize }
impl<'a> Writer<'a> {
    pub(crate) fn new(output: &'a mut [u8]) -> Self { Self { output, used: 0 } }
    pub(crate) fn raw(&mut self, value: &[u8]) {
        let end = self.used.saturating_add(value.len());
        if let Some(dst) = self.output.get_mut(self.used..end) { dst.copy_from_slice(value); }
        self.used = end;
    }
    pub(crate) fn text(&mut self, value: &str) {
        self.raw(b"\"");
        let bytes = value.as_bytes();
        let mut start = 0;
        for (at, b) in bytes.iter().copied().enumerate() {
            if b == b'"' || b == b'\\' || b < 32 {
                self.raw(&bytes[start..at]);
                match b {
                    b'"' => self.raw(b"\\\""), b'\\' => self.raw(b"\\\\"),
                    b'\n' => self.raw(b"\\n"), b'\r' => self.raw(b"\\r"), b'\t' => self.raw(b"\\t"),
                    _ => { let h = b"0123456789abcdef"; self.raw(&[b'\\', b'u', b'0', b'0', h[(b >> 4) as usize], h[(b & 15) as usize]]); }
                }
                start = at + 1;
            }
        }
        self.raw(&bytes[start..]);
        self.raw(b"\"");
    }
    pub(crate) fn integer(&mut self, n: i64) {
        if n < 0 { self.raw(b"-"); }
        self.unsigned(n.unsigned_abs());
    }
    pub(crate) fn unsigned(&mut self, mut n: u64) {
        let mut buf = [0; 20];
        let mut at = buf.len();
        loop { at -= 1; buf[at] = b'0' + (n % 10) as u8; n /= 10; if n == 0 { break; } }
        self.raw(&buf[at..]);
    }
    pub(crate) fn finish(self) -> Result<usize, Error> {
        if self.used > self.output.len() { Err(Error::capacity(ErrorKind::Buffer, 0, self.used)) } else { Ok(self.used) }
    }
}
