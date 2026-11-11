//! Local CPU/allocation measurements for transport primitives, not HTTP/TLS or
//! remote-tool latency. SSE cases measure framing only, without JSON parsing.
//! Header/schema fixtures are parsed before timing. Buffers are reused and warm.
//! Workspace counts explicit buffers and retained state, not immutable fixture
//! bytes, timing samples, temporary stack frames, executable pages or process RSS.
//! Percentiles include one Instant pair per sample; the batched mean amortizes it.
//! The allocator counts alloc, alloc_zeroed and realloc across measured operations;
//! benchmark setup, warmup, sample allocation and CSV printing are excluded.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
use zmcpio::{
    http::{self, Header, Parameter},
    json::{Document, Limits, Token},
    rpc::{self, Message},
    sse::{self, Decoder, Event, Status},
};

struct Counter;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
// SAFETY: every pointer, layout and allocation size is forwarded unchanged to
// System. Instrumentation is confined to this benchmark, not the safe library.
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static COUNTER: Counter = Counter;

fn measure(name: &str, size: usize, workspace: usize, mut operation: impl FnMut()) {
    let smoke = std::env::var_os("ZMCPIO_BENCH_SMOKE").is_some()
        || std::env::args().any(|arg| arg == "--test");
    let samples = if smoke { 11 } else { 501 };
    let batch = if smoke { 16 } else { 512 };
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..16 {
        operation();
    }
    let before = ALLOCS.load(Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..batch {
        operation();
    }
    let mean = start.elapsed().as_nanos() as f64 / batch as f64;
    for _ in 0..samples {
        let start = Instant::now();
        operation();
        timings.push(start.elapsed().as_nanos());
    }
    let allocations = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "allocation in {name}");
    timings.sort_unstable();
    let percentile = |p: usize| timings[(timings.len() - 1) * p / 100];
    println!(
        "{name},{size},{mean:.2},{},{},{},{:.2},{allocations},{workspace}",
        percentile(50),
        percentile(95),
        percentile(99),
        1e9 / mean.max(1.0)
    );
}

fn sse_framing() {
    for size in [64, 256, 1024, 4096, 65536] {
        let payload = format!("{{\"v\":\"{}\"}}", "x".repeat(size - 8));
        assert_eq!(payload.len(), size);
        let event = Event {
            data: Some(payload.as_bytes()),
            event: "message",
            id: Some("event-1"),
            retry_ms: Some(1000),
        };
        let mut output = vec![0; payload.len() + 128];
        let workspace = output.len() + std::mem::size_of::<Event<'_>>();
        measure("sse_encode", payload.len(), workspace, || {
            let len = sse::write_event(black_box(event), &mut output).unwrap();
            black_box(&output[..len]);
        });
        let wire_len = sse::write_event(event, &mut output).unwrap();
        let wire = &output[..wire_len];
        for (name, chunk_size) in [("sse_decode_whole", wire_len), ("sse_decode_chunks_17", 17)] {
            decode_case(name, wire, payload.len(), chunk_size);
        }
        if size == 256 {
            decode_case("sse_decode_chunks_1", wire, payload.len(), 1);
        }
    }
}

fn decode_case(name: &str, wire: &[u8], payload_len: usize, chunk_size: usize) {
    let mut line = vec![0; payload_len + 64];
    let mut data = vec![0; payload_len];
    let mut event_name = [0; 32];
    let mut event_id = [0; 32];
    let mut decoder = Decoder::new(
        &mut line,
        &mut data,
        &mut event_name,
        &mut event_id,
        wire.len() + 1,
    );
    let workspace = decoder.memory_bytes();
    measure(name, wire.len(), workspace, || {
        decoder.reset();
        for chunk in wire.chunks(chunk_size) {
            let step = decoder.feed(black_box(chunk));
            assert_eq!(step.consumed, chunk.len());
            assert!(matches!(step.status, Status::Ready | Status::NeedInput));
        }
        let event = decoder.event().expect("complete SSE event");
        assert_eq!(event.data.unwrap().len(), payload_len);
        black_box(event);
        decoder.consume();
        decoder.finish().unwrap();
    });
}

const ASCII_CALL: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"echo","arguments":{}}}"#;
const UNICODE_CALL: &[u8] = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"天气","arguments":{}}}"#.as_bytes();

fn modern_headers() {
    for (label, raw, name) in [
        (
            "modern_headers_validate_ascii_preparsed",
            ASCII_CALL,
            "echo",
        ),
        (
            "modern_headers_validate_unicode_preparsed",
            UNICODE_CALL,
            "天气",
        ),
    ] {
        let mut tokens = [Token::EMPTY; 128];
        let document = Document::parse(raw, &mut tokens, Limits::default()).unwrap();
        let Message::Request(request) = rpc::parse(document.root()).unwrap() else {
            panic!("expected request")
        };
        let mut encoded = [0; 256];
        let encoded_name = http::encode_header_value(name, &mut encoded).unwrap();
        let headers = [
            Header {
                name: "Content-Type",
                value: http::JSON,
            },
            Header {
                name: "Accept",
                value: http::ACCEPT,
            },
            Header {
                name: "MCP-Protocol-Version",
                value: "2026-07-28",
            },
            Header {
                name: "Mcp-Method",
                value: "tools/call",
            },
            Header {
                name: "Mcp-Name",
                value: encoded_name,
            },
        ];
        let mut scratch = [0; 256];
        let workspace = 128 * std::mem::size_of::<Token>()
            + 256
            + scratch.len()
            + std::mem::size_of_val(&headers)
            + std::mem::size_of_val(&request)
            + std::mem::size_of_val(&document);
        measure(label, raw.len(), workspace, || {
            http::validate_standard_headers(black_box(&headers), black_box(request), &mut scratch)
                .unwrap();
            black_box(&scratch);
        });
    }
}

const SCHEMA: &[u8] = br#"{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"},"nested":{"type":"object","properties":{"count":{"type":"integer","x-mcp-header":"Count"},"ok":{"type":["boolean","null"],"x-mcp-header":"OK"}}},"nullable":{"type":["string","null"],"x-mcp-header":"Nil"}}}"#;
const ARGUMENTS: &[u8] =
    r#"{"region":" 你好\nwest ","nested":{"count":4.2e1,"ok":true},"nullable":null}"#.as_bytes();

fn custom_parameter_headers() {
    let mut schema_tokens = [Token::EMPTY; 128];
    let mut argument_tokens = [Token::EMPTY; 64];
    let schema = Document::parse(SCHEMA, &mut schema_tokens, Limits::default())
        .unwrap()
        .root();
    let arguments = Document::parse(ARGUMENTS, &mut argument_tokens, Limits::default())
        .unwrap()
        .root();
    let mut slots = [Parameter::EMPTY; 8];
    let mut output = [0; 512];
    let mut scratch = [0; 256];
    let workspace = (128 + 64) * std::mem::size_of::<Token>()
        + 8 * std::mem::size_of::<Parameter<'_>>()
        + output.len()
        + scratch.len()
        + std::mem::size_of_val(&schema)
        + std::mem::size_of_val(&arguments);
    {
        let parameters = http::parameters(schema, Some(arguments), &mut slots).unwrap();
        assert_eq!(parameters.len(), 4);
        measure(
            "custom_parameter_encode_preextracted",
            ARGUMENTS.len(),
            workspace,
            || {
                let len =
                    http::write_parameter_headers(black_box(parameters), &mut output, &mut scratch)
                        .unwrap();
                black_box(&output[..len]);
            },
        );
    }
    measure(
        "custom_parameter_extract_encode_preparsed",
        SCHEMA.len() + ARGUMENTS.len(),
        workspace,
        || {
            let parameters =
                http::parameters(black_box(schema), Some(black_box(arguments)), &mut slots)
                    .unwrap();
            let len = http::write_parameter_headers(parameters, &mut output, &mut scratch).unwrap();
            black_box(&output[..len]);
        },
    );
}

fn main() {
    println!(
        "case,size_bytes,mean_ns,p50_instrumented_ns,p95_instrumented_ns,p99_instrumented_ns,ops_per_second,total_allocations,workspace_bytes"
    );
    sse_framing();
    modern_headers();
    custom_parameter_headers();
}
