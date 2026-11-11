//! Local CPU/allocator measurements, not model or network latency claims.
//!
//! `client_call_encode_receive` and `zcall_bridge_encode_receive` include a fresh
//! bounded client per iteration, so the fully validated fixture reply always
//! correlates with request 1. `client_server_roundtrip` retains both endpoints
//! and includes server request validation and response encoding as well.
//! All caller-owned buffers and parsed model fixtures exist before timing.
//! The bridge case begins with a complete zcall ToolCall, not HTTP bytes or
//! partial streaming deltas. It decodes escaped identities/arguments, encodes
//! MCP, validates the complete reply and converts the correlated whole result.
//! `workspace_bytes` sums retained endpoint state and explicit caller buffers;
//! it excludes immutable fixture bytes, timing storage, temporary parser stack
//! frames and code. It is neither peak stack usage nor process RSS.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
use zcall::v1 as model;
use zmcpio::{
    bridge,
    client::{Client, Event, Pending},
    json::{Document, Limits, Token},
    mcp::{Action, Era, Server},
    tools::{Content, ToolResult},
};

struct Counter;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
// SAFETY: allocations, reallocations and deallocations forward the same pointer,
// layout and size to System. Only this benchmark binary permits unsafe code.
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

const ARGUMENTS: &[u8] = br#"{"message":"hello \"MCP\"","n":9007199254740993}"#;
const REPLY: &[u8] = br#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"1"}},"content":[{"type":"text","text":"hello \"MCP\""},{"type":"image","data":"aGk=","mimeType":"image/png"}],"structuredContent":{"n":9007199254740993},"isError":false}}"#;

fn measure(name: &str, memory: usize, mut operation: impl FnMut()) {
    let smoke =
        std::env::var_os("ZMCPIO_BENCH_SMOKE").is_some() || std::env::args().any(|a| a == "--test");
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
        "{name},{mean:.2},{},{},{},{:.2},{allocations},{memory}",
        percentile(50),
        percentile(95),
        percentile(99),
        1e9 / mean.max(1.0)
    );
}

fn client_encode_receive() {
    let mut argument_tokens = [Token::EMPTY; 32];
    let arguments = Document::parse(ARGUMENTS, &mut argument_tokens, Limits::default())
        .unwrap()
        .root();
    let mut slots = [Pending::EMPTY; 1];
    let mut output = [0; 2048];
    let mut tokens = [Token::EMPTY; 128];
    let memory = std::mem::size_of::<Client<'_>>()
        + std::mem::size_of_val(&slots)
        + 32 * std::mem::size_of::<Token>()
        + std::mem::size_of_val(&tokens)
        + output.len();
    measure("client_call_encode_receive", memory, || {
        let mut client = Client::new("bench", "1", Era::Modern, &mut slots);
        let outgoing = client
            .call_tool("echo", Some(black_box(arguments)), &mut output)
            .unwrap();
        black_box(&output[..outgoing.len]);
        let Event::ToolResult { id, result } = client
            .receive(black_box(REPLY), &mut tokens, Limits::default())
            .unwrap()
        else {
            panic!("expected complete tool result")
        };
        assert_eq!(id, outgoing.id);
        assert!(!result.is_error());
        black_box(result.raw());
        assert_eq!(client.pending_count(), 0);
    });
}

fn client_server_roundtrip() {
    let mut argument_tokens = [Token::EMPTY; 32];
    let arguments = Document::parse(ARGUMENTS, &mut argument_tokens, Limits::default())
        .unwrap()
        .root();
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("bench", "1", Era::Modern, &mut slots);
    let mut server = Server::new("echo-bench", "1");
    let mut request = [0; 2048];
    let mut reply = [0; 4096];
    let mut client_tokens = [Token::EMPTY; 128];
    let mut server_tokens = [Token::EMPTY; 128];
    let memory = client.memory_bytes()
        + server.memory_bytes()
        + 32 * std::mem::size_of::<Token>()
        + std::mem::size_of_val(&client_tokens)
        + std::mem::size_of_val(&server_tokens)
        + request.len()
        + reply.len();
    measure("client_server_roundtrip", memory, || {
        let outgoing = client
            .call_tool("echo", Some(black_box(arguments)), &mut request)
            .unwrap();
        let Action::CallTool(call) = server
            .handle(
                black_box(&request[..outgoing.len]),
                &mut server_tokens,
                Limits::default(),
                &mut reply,
            )
            .unwrap()
        else {
            panic!("expected tools/call action")
        };
        assert!(call.name.matches("echo"));
        let text = core::str::from_utf8(call.arguments_bytes()).unwrap();
        let content = [Content::Text(text)];
        let mut result = ToolResult::new(&content);
        result.structured_content = call.arguments;
        let written = server
            .tool_result(call.context, result, &mut reply)
            .unwrap();
        let Event::ToolResult { id, result } = client
            .receive(
                black_box(&reply[..written]),
                &mut client_tokens,
                Limits::default(),
            )
            .unwrap()
        else {
            panic!("expected complete tool result")
        };
        assert_eq!(id, outgoing.id);
        assert!(!result.is_error());
        black_box(result.raw());
        assert_eq!(client.pending_count(), 0);
    });
}

fn zcall_bridge_encode_receive() {
    let mut model_tokens = model::TokenStorage::<128>::new();
    let mut text = |raw: &'static [u8]| {
        model::Json::parse(raw, &mut model_tokens, model::Limits::default())
            .unwrap()
            .as_text()
            .unwrap()
    };
    let model_call = model::Content::ToolCall {
        id: model::Presence::Value(text(br#""call\u005f7""#)),
        name: text(br#""\u0065cho""#),
        arguments: model::Data::JsonText(text(
            br#""{\"message\":\"hello \\\"MCP\\\"\",\"n\":9007199254740993}""#,
        )),
        legacy: false,
    };
    let mut slots = [Pending::EMPTY; 1];
    let mut identity = [0; 64];
    let mut argument_bytes = [0; 256];
    let mut argument_tokens = [Token::EMPTY; 32];
    let mut output = [0; 2048];
    let mut reply_tokens = [Token::EMPTY; 128];
    let memory = std::mem::size_of::<Client<'_>>()
        + std::mem::size_of_val(&slots)
        + std::mem::size_of_val(&argument_tokens)
        + std::mem::size_of_val(&reply_tokens)
        + std::mem::size_of_val(&model_tokens)
        + identity.len()
        + argument_bytes.len()
        + output.len();
    measure("zcall_bridge_encode_receive", memory, || {
        let mut client = Client::new("bench", "1", Era::Modern, &mut slots);
        let prepared = bridge::prepare_call(
            black_box(model_call),
            &mut identity,
            &mut argument_bytes,
            &mut argument_tokens,
            Limits::default(),
        )
        .unwrap();
        let invocation = prepared.start(&mut client, &mut output).unwrap();
        black_box(&output[..invocation.outgoing().len]);
        let Event::ToolResult { id, result } = client
            .receive(black_box(REPLY), &mut reply_tokens, Limits::default())
            .unwrap()
        else {
            panic!("expected complete tool result")
        };
        let model::Content::ToolResult {
            id: model::Presence::Value(model_id),
            name: model::Presence::Value(name),
            result: model::Data::Json(data),
            ..
        } = invocation
            .content(id, result, &mut model_tokens, model::Limits::default())
            .unwrap()
        else {
            panic!("expected model tool result")
        };
        assert!(model_id.eq_str("call_7"));
        assert!(name.eq_str("echo"));
        assert_eq!(data.raw(), result.raw().raw().as_bytes());
        black_box(data);
        assert_eq!(client.pending_count(), 0);
    });
}

fn main() {
    println!(
        "case,mean_ns,p50_instrumented_ns,p95_instrumented_ns,p99_instrumented_ns,ops_per_second,total_allocations,workspace_bytes"
    );
    client_encode_receive();
    client_server_roundtrip();
    zcall_bridge_encode_receive();
}
