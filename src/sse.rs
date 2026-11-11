//! Allocation-free Server-Sent Events framing.
//!
//! Feed arbitrary network chunks, retain the unconsumed suffix, and acknowledge
//! each Ready block with consume. A block may carry only id/retry metadata: in
//! that case Event::data is None and no application message should be dispatched.
//! Persistent last-event IDs and retry policy belong to the HTTP adapter, which
//! can commit them after accepting a complete block. Never use metadata from an
//! incomplete block as a resume checkpoint.
//!
//! Implements WHATWG line endings, one leading BOM, UTF-8 replacement decoding,
//! multiline data, empty data, comments, field-name case sensitivity and ignored
//! unknown fields. EOF discards incomplete blocks. Capacity failures preserve
//! state and can be retried after replacing the indicated caller-owned buffer.
//! The hard block limit includes comments and ignored fields; CRLF counts as one
//! line ending. A failed limit is terminal until reset. No I/O or allocation.

use core::fmt;

/// Caller-owned buffer which needs additional space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Buffer {
    Line,
    Data,
    Event,
    Id,
    Output,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Capacity { buffer: Buffer, needed: usize },
    BlockTooLarge { limit: usize },
    InvalidUtf8,
    InvalidField,
    UnconsumedEvent,
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl core::error::Error for Error {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    NeedInput,
    Ready,
    NeedCapacity { buffer: Buffer, needed: usize },
    Error(Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Step {
    pub consumed: usize,
    pub status: Status,
}

/// A complete SSE block, borrowing decoder buffers.
///
/// Data is None for metadata-only blocks, and Some(empty) for an explicitly
/// empty data event. Event defaults to "message". Id is only the field present
/// in this block, not an inherited last-event ID; Some("") clears a checkpoint.
/// Retry is the last valid decimal value in the block, saturated to u64::MAX.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event<'a> {
    pub data: Option<&'a [u8]>,
    pub event: &'a str,
    pub id: Option<&'a str>,
    pub retry_ms: Option<u64>,
}

pub struct Decoder<'a> {
    line: &'a mut [u8],
    data: &'a mut [u8],
    event_name: &'a mut [u8],
    event_id: &'a mut [u8],
    line_len: usize,
    data_len: usize,
    event_len: usize,
    id_len: usize,
    block_bytes: usize,
    max_block_bytes: usize,
    first_line: bool,
    skip_lf: bool,
    has_data: bool,
    has_id: bool,
    has_fields: bool,
    retry_ms: Option<u64>,
    ready: bool,
    closed: bool,
    failed: Option<Error>,
}

impl<'a> Decoder<'a> {
    /// Buffers bound individual lines, concatenated data, event names and IDs.
    /// max_event_bytes is a hard bound on a complete normalized wire block,
    /// including field names, comments, unknown fields and line terminators.
    pub fn new(
        line: &'a mut [u8],
        data: &'a mut [u8],
        event_name: &'a mut [u8],
        event_id: &'a mut [u8],
        max_event_bytes: usize,
    ) -> Self {
        Self {
            line,
            data,
            event_name,
            event_id,
            line_len: 0,
            data_len: 0,
            event_len: 0,
            id_len: 0,
            block_bytes: 0,
            max_block_bytes: max_event_bytes,
            first_line: true,
            skip_lf: false,
            has_data: false,
            has_id: false,
            has_fields: false,
            retry_ms: None,
            ready: false,
            closed: false,
            failed: None,
        }
    }

    /// The consumed prefix must not be fed again, even on a capacity failure.
    /// Feed the remaining suffix after replacing the requested buffer. Ready
    /// never consumes bytes from the following block.
    pub fn feed(&mut self, input: &[u8]) -> Step {
        if let Some(error) = self.failed {
            return Step { consumed: 0, status: Status::Error(error) };
        }
        if self.closed {
            return Step { consumed: 0, status: Status::Error(Error::Closed) };
        }
        if self.ready {
            return Step { consumed: 0, status: Status::Ready };
        }
        for (at, byte) in input.iter().copied().enumerate() {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if self.block_bytes >= self.max_block_bytes {
                let error = Error::BlockTooLarge { limit: self.max_block_bytes };
                self.failed = Some(error);
                return Step { consumed: at, status: Status::Error(error) };
            }
            if byte == b'\r' || byte == b'\n' {
                let blank_line = self.line_len == 0
                    || (self.first_line
                        && self.line_len == 3
                        && self.line[..3] == [0xef, 0xbb, 0xbf]);
                match self.process_line() {
                    Ok(()) => {
                        self.block_bytes += 1;
                        self.line_len = 0;
                        self.first_line = false;
                        self.skip_lf = byte == b'\r';
                        if self.ready {
                            self.block_bytes = 0;
                            return Step { consumed: at + 1, status: Status::Ready };
                        }
                        // Empty/comment-only blocks still establish boundaries.
                        if blank_line {
                            self.block_bytes = 0;
                        }
                    }
                    Err(status) => {
                        return Step { consumed: at, status };
                    }
                }
            } else {
                if self.line_len == self.line.len() {
                    return Step {
                        consumed: at,
                        status: Status::NeedCapacity {
                            buffer: Buffer::Line,
                            needed: self.line_len.saturating_add(1),
                        },
                    };
                }
                self.line[self.line_len] = byte;
                self.line_len += 1;
                self.block_bytes += 1;
            }
        }
        Step { consumed: input.len(), status: Status::NeedInput }
    }

    fn process_line(&mut self) -> Result<(), Status> {
        let raw = &self.line[..self.line_len];
        let line = if self.first_line && raw.starts_with(&[0xef, 0xbb, 0xbf]) {
            &raw[3..]
        } else {
            raw
        };
        if line.is_empty() {
            self.ready = self.has_fields;
            return Ok(());
        }
        if line[0] == b':' {
            return Ok(());
        }
        let (field, value) = match line.iter().position(|b| *b == b':') {
            Some(at) => {
                let value = &line[at + 1..];
                (&line[..at], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (line, &[][..]),
        };
        match field {
            b"data" => {
                let separator = usize::from(self.has_data);
                let needed = self.data_len
                    .saturating_add(separator)
                    .saturating_add(replaced_len(value));
                need_capacity(Buffer::Data, needed, self.data.len())?;
                if self.has_data {
                    self.data[self.data_len] = b'\n';
                    self.data_len += 1;
                }
                self.data_len += replace_utf8(value, &mut self.data[self.data_len..]);
                self.has_data = true;
                self.has_fields = true;
            }
            b"event" => {
                let needed = replaced_len(value);
                need_capacity(Buffer::Event, needed, self.event_name.len())?;
                self.event_len = replace_utf8(value, self.event_name);
                self.has_fields = true;
            }
            b"id" if !value.contains(&0) => {
                let needed = replaced_len(value);
                need_capacity(Buffer::Id, needed, self.event_id.len())?;
                self.id_len = replace_utf8(value, self.event_id);
                self.has_id = true;
                self.has_fields = true;
            }
            b"retry" if !value.is_empty() && value.iter().all(u8::is_ascii_digit) => {
                self.retry_ms = Some(value.iter().fold(0_u64, |n, byte| {
                    n.saturating_mul(10).saturating_add(u64::from(*byte - b'0'))
                }));
                self.has_fields = true;
            }
            _ => {}
        }
        Ok(())
    }

    /// Latest complete retry field in the current block, even before its blank
    /// line. HTTP adapters can apply this hint before finish discards partial
    /// event data. No timer or reconnect is started by this decoder.
    pub fn retry_hint(&self) -> Option<u64> {
        self.retry_ms
    }

    pub fn event(&self) -> Option<Event<'_>> {
        if !self.ready {
            return None;
        }
        // replace_utf8 only writes valid UTF-8.
        let event = core::str::from_utf8(&self.event_name[..self.event_len])
            .unwrap_or_default();
        let id = core::str::from_utf8(&self.event_id[..self.id_len])
            .unwrap_or_default();
        Some(Event {
            data: self.has_data.then_some(&self.data[..self.data_len]),
            event: if event.is_empty() { "message" } else { event },
            id: self.has_id.then_some(id),
            retry_ms: self.retry_ms,
        })
    }

    /// Release a complete block only after its consumer has accepted it.
    /// This is a no-op while accumulating an incomplete block.
    pub fn consume(&mut self) {
        if self.ready {
            self.clear_block();
        }
    }

    fn clear_block(&mut self) {
        self.data_len = 0;
        self.event_len = 0;
        self.id_len = 0;
        self.has_data = false;
        self.has_id = false;
        self.has_fields = false;
        self.retry_ms = None;
        self.ready = false;
    }

    /// Preserve buffered bytes while growing one buffer. Failure is atomic.
    pub fn replace_buffer(&mut self, buffer: Buffer, storage: &'a mut [u8]) -> Result<(), Error> {
        let (old, used) = match buffer {
            Buffer::Line => (&mut self.line, self.line_len),
            Buffer::Data => (&mut self.data, self.data_len),
            Buffer::Event => (&mut self.event_name, self.event_len),
            Buffer::Id => (&mut self.event_id, self.id_len),
            Buffer::Output => return Err(Error::InvalidField),
        };
        if storage.len() < used {
            return Err(Error::Capacity { buffer, needed: used });
        }
        storage[..used].copy_from_slice(&old[..used]);
        *old = storage;
        Ok(())
    }

    /// End of stream never dispatches an incomplete event. A complete unread
    /// block must be consumed first. Subsequent feed calls report Closed.
    pub fn finish(&mut self) -> Result<(), Error> {
        if self.ready {
            return Err(Error::UnconsumedEvent);
        }
        if let Some(error) = self.failed {
            return Err(error);
        }
        self.clear_block();
        self.line_len = 0;
        self.block_bytes = 0;
        self.closed = true;
        Ok(())
    }

    /// Start a new stream with the same caller buffers. Resume state is external.
    pub fn reset(&mut self) {
        self.clear_block();
        self.line_len = 0;
        self.block_bytes = 0;
        self.first_line = true;
        self.skip_lf = false;
        self.closed = false;
        self.failed = None;
    }

    pub fn memory_bytes(&self) -> usize {
        core::mem::size_of::<Self>() + self.line.len() + self.data.len()
            + self.event_name.len() + self.event_id.len()
    }
}

fn need_capacity(buffer: Buffer, needed: usize, available: usize) -> Result<(), Status> {
    if needed > available {
        Err(Status::NeedCapacity { buffer, needed })
    } else {
        Ok(())
    }
}

/// UTF-8 error spans are replaced exactly as by String::from_utf8_lossy.
fn replaced_len(mut input: &[u8]) -> usize {
    let mut len = 0_usize;
    loop {
        match core::str::from_utf8(input) {
            Ok(_) => return len.saturating_add(input.len()),
            Err(error) => {
                len = len.saturating_add(error.valid_up_to()).saturating_add(3);
                let tail = &input[error.valid_up_to()..];
                let bad = error.error_len().unwrap_or(tail.len());
                input = &tail[bad..];
            }
        }
    }
}

/// Caller preflights replaced_len, so all destination slices are in bounds.
fn replace_utf8(mut input: &[u8], output: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        match core::str::from_utf8(input) {
            Ok(_) => {
                output[len..len + input.len()].copy_from_slice(input);
                return len + input.len();
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output[len..len + valid].copy_from_slice(&input[..valid]);
                len += valid;
                output[len..len + 3].copy_from_slice(&[0xef, 0xbf, 0xbd]);
                len += 3;
                let tail = &input[valid..];
                let bad = error.error_len().unwrap_or(tail.len());
                input = &tail[bad..];
            }
        }
    }
}

fn valid_field(value: &str, reject_nul: bool) -> bool {
    !value.bytes().any(|byte| byte == b'\r' || byte == b'\n' || (reject_nul && byte == 0))
}

fn decimal(mut number: u64, storage: &mut [u8; 20]) -> &[u8] {
    let mut at = storage.len();
    loop {
        at -= 1;
        storage[at] = b'0' + (number % 10) as u8;
        number /= 10;
        if number == 0 {
            return &storage[at..];
        }
    }
}

fn add_size(size: &mut usize, extra: usize) -> Result<(), Error> {
    *size = size.checked_add(extra).ok_or(Error::BlockTooLarge { limit: usize::MAX })?;
    Ok(())
}

/// Encode one SSE block. All validation and capacity checks precede output
/// mutation. Data must be valid UTF-8 and contain no CR; LF is safely encoded
/// as separate data fields. This prevents field/event injection while retaining
/// multiline JSON, leading spaces, empty data and trailing LF exactly.
pub fn write_event(event: Event<'_>, output: &mut [u8]) -> Result<usize, Error> {
    if !valid_field(event.event, false) || event.id.is_some_and(|id| !valid_field(id, true)) {
        return Err(Error::InvalidField);
    }
    if let Some(data) = event.data {
        if core::str::from_utf8(data).is_err() {
            return Err(Error::InvalidUtf8);
        }
        if data.contains(&b'\r') {
            return Err(Error::InvalidField);
        }
    }
    let mut digits = [0; 20];
    let retry = match event.retry_ms {
        Some(number) => Some(decimal(number, &mut digits)),
        None => None,
    };
    let has_name = !event.event.is_empty() && event.event != "message";
    let mut needed = 1_usize;
    if let Some(id) = event.id {
        add_size(&mut needed, 5)?;
        add_size(&mut needed, id.len())?;
    }
    if has_name {
        add_size(&mut needed, 8)?;
        add_size(&mut needed, event.event.len())?;
    }
    if let Some(retry) = retry {
        add_size(&mut needed, 8)?;
        add_size(&mut needed, retry.len())?;
    }
    if let Some(data) = event.data {
        for line in data.split(|byte| *byte == b'\n') {
            add_size(&mut needed, 7)?;
            add_size(&mut needed, line.len())?;
        }
    }
    if output.len() < needed {
        return Err(Error::Capacity { buffer: Buffer::Output, needed });
    }
    let mut at = 0;
    if let Some(id) = event.id {
        put_field(output, &mut at, b"id: ", id.as_bytes());
    }
    if has_name {
        put_field(output, &mut at, b"event: ", event.event.as_bytes());
    }
    if let Some(retry) = retry {
        put_field(output, &mut at, b"retry: ", retry);
    }
    if let Some(data) = event.data {
        for line in data.split(|byte| *byte == b'\n') {
            put_field(output, &mut at, b"data: ", line);
        }
    }
    output[at] = b'\n';
    Ok(at + 1)
}

fn put_field(output: &mut [u8], at: &mut usize, prefix: &[u8], value: &[u8]) {
    output[*at..*at + prefix.len()].copy_from_slice(prefix);
    *at += prefix.len();
    output[*at..*at + value.len()].copy_from_slice(value);
    *at += value.len();
    output[*at] = b'\n';
    *at += 1;
}

/// Encode a heartbeat comment block. LF-delimited comment lines are supported;
/// CR is rejected. Capacity errors leave output unchanged.
pub fn write_comment(comment: &str, output: &mut [u8]) -> Result<usize, Error> {
    if comment.contains('\r') {
        return Err(Error::InvalidField);
    }
    let mut needed = 1_usize;
    for line in comment.split('\n') {
        add_size(&mut needed, 3)?;
        add_size(&mut needed, line.len())?;
    }
    if output.len() < needed {
        return Err(Error::Capacity { buffer: Buffer::Output, needed });
    }
    let mut at = 0;
    for line in comment.split('\n') {
        put_field(output, &mut at, b": ", line.as_bytes());
    }
    output[at] = b'\n';
    Ok(at + 1)
}
