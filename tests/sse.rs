use zmcpio::sse::{self, Buffer, Decoder, Error, Event, Status};

#[derive(Debug, PartialEq, Eq)]
struct Owned {
    data: Option<Vec<u8>>,
    event: String,
    id: Option<String>,
    retry_ms: Option<u64>,
}

fn owned(event: Event<'_>) -> Owned {
    Owned {
        data: event.data.map(<[u8]>::to_vec),
        event: event.event.to_owned(),
        id: event.id.map(str::to_owned),
        retry_ms: event.retry_ms,
    }
}

fn decode<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> Vec<Owned> {
    let mut line = [0; 1024];
    let mut data = [0; 2048];
    let mut name = [0; 128];
    let mut id = [0; 128];
    let mut decoder = Decoder::new(&mut line, &mut data, &mut name, &mut id, 8192);
    let mut events = Vec::new();
    for mut part in parts {
        while !part.is_empty() {
            let step = decoder.feed(part);
            part = &part[step.consumed..];
            match step.status {
                Status::Ready => {
                    events.push(owned(decoder.event().unwrap()));
                    decoder.consume();
                }
                Status::NeedInput => assert!(part.is_empty()),
                other => panic!("unexpected decoder status: {other:?}"),
            }
        }
    }
    decoder.finish().unwrap();
    events
}

#[test]
fn every_chunk_boundary_preserves_multiline_unicode_and_crlf() {
    let wire = b"\xef\xbb\xbf: heartbeat\r\nid: abc\r\nevent: message\r\nretry: 0015\r\ndata: {\"text\":\"\xe4\xbd\xa0\xe5\xa5\xbd\"}\r\ndata: second\r\n\r\ndata:\n\n";
    let expected = vec![
        Owned {
            data: Some("{\"text\":\"你好\"}\nsecond".as_bytes().to_vec()),
            event: "message".into(),
            id: Some("abc".into()),
            retry_ms: Some(15),
        },
        Owned { data: Some(vec![]), event: "message".into(), id: None, retry_ms: None },
    ];
    for split in 0..=wire.len() {
        assert_eq!(decode([&wire[..split], &wire[split..]]), expected, "split {split}");
    }
    for size in 1..=wire.len() {
        assert_eq!(decode(wire.chunks(size)), expected, "chunk size {size}");
    }
}

#[test]
fn cr_lf_and_crlf_delimiters_are_equivalent() {
    for ending in ["\r", "\n", "\r\n"] {
        let wire = format!("data: one{ending}{ending}data: two{ending}{ending}");
        let result = decode(wire.as_bytes().chunks(1));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].data.as_deref(), Some(b"one".as_slice()));
        assert_eq!(result[1].data.as_deref(), Some(b"two".as_slice()));
    }
}

#[test]
fn pending_event_applies_backpressure_without_consuming_next_bytes() {
    let mut line = [0; 64];
    let mut data = [0; 64];
    let mut name = [0; 32];
    let mut id = [0; 32];
    let mut d = Decoder::new(&mut line, &mut data, &mut name, &mut id, 256);
    let wire = b"data: one\r\n\r\ndata: two\n\n";
    let step = d.feed(wire);
    assert_eq!(step.status, Status::Ready);
    let remaining = &wire[step.consumed..];
    let again = d.feed(remaining);
    assert_eq!(again.consumed, 0);
    assert_eq!(again.status, Status::Ready);
    assert_eq!(d.event().unwrap().data, Some(b"one".as_slice()));
    assert_eq!(d.finish(), Err(Error::UnconsumedEvent));
    d.consume();
    let step = d.feed(remaining);
    assert_eq!(step.status, Status::Ready);
    assert_eq!(step.consumed, remaining.len());
    assert_eq!(d.event().unwrap().data, Some(b"two".as_slice()));
    d.consume();
    d.finish().unwrap();
    assert_eq!(d.feed(b"").status, Status::Error(Error::Closed));
}

#[test]
fn metadata_only_blocks_and_empty_ids_are_not_lost() {
    let result = decode([b"id: checkpoint\nretry: 42\n\nid:\n\nid: bad\0id\n\n".as_slice()]);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].data, None);
    assert_eq!(result[0].id.as_deref(), Some("checkpoint"));
    assert_eq!(result[0].retry_ms, Some(42));
    assert_eq!(result[1].id.as_deref(), Some(""));
    assert_eq!(result[1].data, None);
}

#[test]
fn only_one_optional_space_is_removed_and_fields_are_case_sensitive() {
    let wire = b": comment\nunknown: ignored\nDATA: ignored\ndata:  leading\nDATA: ignored\ndata\n\n";
    let result = decode([wire.as_slice()]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].data.as_deref(), Some(b" leading\n".as_slice()));
}

#[test]
fn empty_and_colonless_data_dispatch_empty_payloads() {
    let result = decode([b"data\n\ndata:\n\ndata: \n\n".as_slice()]);
    assert_eq!(result.len(), 3);
    assert!(result.iter().all(|event| event.data == Some(vec![])));
}

#[test]
fn later_fields_replace_names_and_ids_but_null_id_is_ignored() {
    let result = decode([b"event: old\nevent: new\nid: first\nid: next\nid: \0bad\ndata: x\n\ndata: y\n\n".as_slice()]);
    assert_eq!(result[0].event, "new");
    assert_eq!(result[0].id.as_deref(), Some("next"));
    assert_eq!(result[1].event, "message");
    // Inherited resume state belongs to the transport, not to this block.
    assert_eq!(result[1].id, None);
}

#[test]
fn event_name_resets_at_blank_boundary_without_data() {
    let result = decode([b"event: custom\n\ndata: later\n\n".as_slice()]);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].event, "custom");
    assert_eq!(result[0].data, None);
    assert_eq!(result[1].event, "message");
}

#[test]
fn retry_requires_nonempty_ascii_digits_and_saturates_overflow() {
    let result = decode([b"retry: 0004\nretry: +5\nretry: -1\nretry: 1.5\nretry:  3\nretry:\n\nretry: 999999999999999999999999999999\n\n".as_slice()]);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].retry_ms, Some(4));
    assert_eq!(result[1].retry_ms, Some(u64::MAX));
}

#[test]
fn invalid_utf8_is_replaced_without_allocation_or_chunk_sensitivity() {
    let bytes: Vec<u8> = (0..=255).filter(|b| !matches!(*b, b'\r' | b'\n')).collect();
    let mut wire = b"data: ".to_vec();
    wire.extend_from_slice(&bytes);
    wire.extend_from_slice(b"\n\n");
    let expected = String::from_utf8_lossy(&bytes).as_bytes().to_vec();
    for size in 1..=wire.len() {
        let result = decode(wire.chunks(size));
        assert_eq!(result[0].data.as_deref(), Some(expected.as_slice()));
    }
    let result = decode([b"event: \xff\nid: \xe2\x82\ndata: ok\n\n".as_slice()]);
    assert_eq!(result[0].event, "\u{fffd}");
    assert_eq!(result[0].id.as_deref(), Some("\u{fffd}"));
}

#[test]
fn only_one_initial_bom_is_stripped() {
    let result = decode([b"\xef\xbb\xbfdata: yes\n\n\xef\xbb\xbfdata: no\n\n".as_slice()]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].data.as_deref(), Some(b"yes".as_slice()));
}

#[test]
fn partial_eof_discards_data_and_id_but_exposes_complete_retry_hint() {
    let mut line = [0; 64];
    let mut data = [0; 64];
    let mut name = [0; 16];
    let mut id = [0; 16];
    let mut d = Decoder::new(&mut line, &mut data, &mut name, &mut id, 256);
    assert_eq!(d.feed(b"id: uncommitted\nretry: 7\ndata: partial\n").status, Status::NeedInput);
    assert_eq!(d.retry_hint(), Some(7));
    assert_eq!(d.event(), None);
    d.consume(); // Does not discard partial state.
    assert_eq!(d.retry_hint(), Some(7));
    d.finish().unwrap();
    assert_eq!(d.event(), None);
    d.reset();
    assert_eq!(d.retry_hint(), None);
    assert_eq!(d.feed(b"\xef\xbb\xbfdata: fresh\n\n").status, Status::Ready);
    assert_eq!(d.event().unwrap().data, Some(b"fresh".as_slice()));
    assert_eq!(d.event().unwrap().id, None);
}

#[test]
fn eof_without_blank_line_never_dispatches() {
    for wire in [b"data: x".as_slice(), b"data: x\n", b"data: x\r\n", b"\xef\xbb"] {
        assert!(decode(wire.chunks(1)).is_empty());
    }
}

#[test]
fn data_capacity_retry_preserves_line_and_reports_exact_needed_bytes() {
    let mut line = [0; 64];
    let mut small = [0; 1];
    let mut large = [0; 16];
    let mut name = [0; 16];
    let mut id = [0; 16];
    let mut d = Decoder::new(&mut line, &mut small, &mut name, &mut id, 256);
    let wire = b"data: hello\n\n";
    let step = d.feed(wire);
    assert_eq!(step.status, Status::NeedCapacity { buffer: Buffer::Data, needed: 5 });
    assert_eq!(&wire[step.consumed..], b"\n\n");
    d.replace_buffer(Buffer::Data, &mut large).unwrap();
    assert_eq!(d.feed(&wire[step.consumed..]).status, Status::Ready);
    assert_eq!(d.event().unwrap().data, Some(b"hello".as_slice()));
}

#[test]
fn line_growth_is_recoverable_and_failed_replacement_is_atomic() {
    let mut small = [0; 2];
    let mut too_small = [0; 1];
    let mut large = [0; 64];
    let mut data = [0; 64];
    let mut name = [0; 16];
    let mut id = [0; 16];
    let mut d = Decoder::new(&mut small, &mut data, &mut name, &mut id, 256);
    let wire = b"data: 123\n\n";
    let step = d.feed(wire);
    assert_eq!(step.consumed, 2);
    assert_eq!(step.status, Status::NeedCapacity { buffer: Buffer::Line, needed: 3 });
    assert_eq!(
        d.replace_buffer(Buffer::Line, &mut too_small),
        Err(Error::Capacity { buffer: Buffer::Line, needed: 2 })
    );
    d.replace_buffer(Buffer::Line, &mut large).unwrap();
    assert_eq!(d.feed(&wire[step.consumed..]).status, Status::Ready);
    assert_eq!(d.event().unwrap().data, Some(b"123".as_slice()));
}

#[test]
fn event_name_and_id_can_grow_independently() {
    let mut line = [0; 64];
    let mut data = [0; 64];
    let mut small_name = [0; 1];
    let mut small_id = [0; 1];
    let mut name = [0; 16];
    let mut id = [0; 16];
    let mut d = Decoder::new(&mut line, &mut data, &mut small_name, &mut small_id, 256);
    let wire = b"event: custom\nid: session\ndata: {}\n\n";
    let step = d.feed(wire);
    assert_eq!(step.status, Status::NeedCapacity { buffer: Buffer::Event, needed: 6 });
    let mut at = step.consumed;
    d.replace_buffer(Buffer::Event, &mut name).unwrap();
    let step = d.feed(&wire[at..]);
    assert_eq!(step.status, Status::NeedCapacity { buffer: Buffer::Id, needed: 7 });
    at += step.consumed;
    d.replace_buffer(Buffer::Id, &mut id).unwrap();
    assert_eq!(d.feed(&wire[at..]).status, Status::Ready);
    let event = d.event().unwrap();
    assert_eq!(event.event, "custom");
    assert_eq!(event.id, Some("session"));
}

#[test]
fn utf8_replacement_capacity_accounts_for_expansion() {
    let mut line = [0; 32];
    let mut data = [0; 3];
    let mut name = [0; 4];
    let mut id = [0; 4];
    let mut d = Decoder::new(&mut line, &mut data, &mut name, &mut id, 64);
    assert_eq!(
        d.feed(b"data: \xff\xff\n\n").status,
        Status::NeedCapacity { buffer: Buffer::Data, needed: 6 }
    );
}

#[test]
fn cumulative_block_limit_counts_ignored_and_comment_lines() {
    for wire in [b":1234\n:1234\n:1234\n:1234\n".as_slice(), b"x:123\nx:123\nx:123\nx:123\n"] {
        let mut line = [0; 32];
        let mut data = [0; 32];
        let mut name = [0; 4];
        let mut id = [0; 4];
        let mut d = Decoder::new(&mut line, &mut data, &mut name, &mut id, 19);
        let step = d.feed(wire);
        assert_eq!(step.consumed, 19);
        assert_eq!(step.status, Status::Error(Error::BlockTooLarge { limit: 19 }));
        assert_eq!(d.feed(b"\n").consumed, 0);
        assert_eq!(d.finish(), Err(Error::BlockTooLarge { limit: 19 }));
        d.reset();
        assert_eq!(d.feed(b"data: x\n\n").status, Status::Ready);
    }
}

#[test]
fn exact_limit_and_crlf_accounting_are_chunk_independent() {
    for wire in [b"data: x\n\n".as_slice(), b"data: x\r\n\r\n"] {
        let mut line = [0; 32];
        let mut data = [0; 32];
        let mut name = [0; 4];
        let mut id = [0; 4];
        let mut d = Decoder::new(&mut line, &mut data, &mut name, &mut id, 9);
        for byte in wire {
            let step = d.feed(core::slice::from_ref(byte));
            assert!(!matches!(step.status, Status::Error(_)));
            if step.status == Status::Ready {
                assert_eq!(d.event().unwrap().data, Some(b"x".as_slice()));
                d.consume();
            }
        }
        d.finish().unwrap();
    }
}

#[test]
fn zero_payload_buffers_support_explicit_empty_fields() {
    let mut line = [0; 32];
    let mut data = [];
    let mut name = [];
    let mut id = [];
    let mut d = Decoder::new(&mut line, &mut data, &mut name, &mut id, 64);
    assert_eq!(d.feed(b"event:\nid:\ndata:\n\n").status, Status::Ready);
    assert_eq!(d.event().unwrap(), Event {
        data: Some(&[]), event: "message", id: Some(""), retry_ms: None,
    });
}

#[test]
fn encoder_round_trips_multiline_data_without_field_injection() {
    let event = Event {
        data: Some(" first\nevent: evil\nid: bad\n\nlast\n你好\n".as_bytes()),
        event: "custom",
        id: Some("safe-id"),
        retry_ms: Some(u64::MAX),
    };
    let mut output = [0; 512];
    let len = sse::write_event(event, &mut output).unwrap();
    let result = decode(output[..len].chunks(1));
    assert_eq!(result, vec![owned(event)]);
}

#[test]
fn encoder_handles_empty_payload_and_metadata_only_blocks() {
    for event in [
        Event { data: Some(&[]), event: "message", id: None, retry_ms: None },
        Event { data: None, event: "message", id: Some(""), retry_ms: Some(0) },
    ] {
        let mut output = [0; 64];
        let len = sse::write_event(event, &mut output).unwrap();
        assert_eq!(decode([&output[..len]]), vec![owned(event)]);
    }
}

#[test]
fn encoder_rejects_injection_and_invalid_utf8_before_mutating_output() {
    let base = Event { data: Some(b"{}"), event: "message", id: None, retry_ms: None };
    for event in [
        Event { event: "message\nid: evil", ..base },
        Event { event: "message\rdata: bad", ..base },
        Event { id: Some("id\nbad"), ..base },
        Event { id: Some("id\r"), ..base },
        Event { id: Some("id\0"), ..base },
        Event { data: Some(b"first\rdata: bad"), ..base },
        Event { data: Some(&[0xff]), ..base },
    ] {
        let mut output = [0xa5; 64];
        assert!(sse::write_event(event, &mut output).is_err());
        assert_eq!(output, [0xa5; 64]);
    }
}

#[test]
fn encoder_capacity_failure_is_atomic_and_exact() {
    let event = Event { data: Some(b"{}\n"), event: "notify", id: Some("42"), retry_ms: Some(15) };
    let mut tiny = [0xa5; 3];
    let needed = match sse::write_event(event, &mut tiny) {
        Err(Error::Capacity { buffer: Buffer::Output, needed }) => needed,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(tiny, [0xa5; 3]);
    let mut exact = vec![0; needed];
    assert_eq!(sse::write_event(event, &mut exact).unwrap(), needed);
    assert_eq!(decode([exact.as_slice()]), vec![owned(event)]);
}

#[test]
fn heartbeat_comments_do_not_generate_messages() {
    let mut output = [0; 128];
    let len = sse::write_comment("keepalive\ndata: not a message", &mut output).unwrap();
    assert!(decode(output[..len].chunks(1)).is_empty());
    let mut tiny = [0xa5; 1];
    assert!(matches!(sse::write_comment("x", &mut tiny), Err(Error::Capacity { .. })));
    assert_eq!(tiny, [0xa5; 1]);
    assert_eq!(sse::write_comment("x\rdata: bad", &mut output), Err(Error::InvalidField));
}
