use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
use zmcpio::{
    json::{Document, Limits, Token},
    mcp::{Action, Server},
    tools::{Content, Tool, ToolResult},
};

struct Counter;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
// SAFETY: pointers, layouts and sizes are forwarded unchanged to System. This
// instrumentation is in the benchmark binary, not the unsafe-forbidden library.
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(p, layout, size) }
    }
}
#[global_allocator]
static COUNTER: Counter = Counter;
fn measure(name: &str, size: usize, memory: usize, mut f: impl FnMut()) {
    let smoke =
        std::env::var_os("ZMCPIO_BENCH_SMOKE").is_some() || std::env::args().any(|a| a == "--test");
    let samples = if smoke { 11 } else { 501 };
    let batch = if smoke { 16 } else { 512 };
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..16 {
        f();
    }
    let before = ALLOCS.load(Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..batch {
        f();
    }
    let mean = start.elapsed().as_nanos() as f64 / batch as f64;
    for _ in 0..samples {
        let start = Instant::now();
        f();
        timings.push(start.elapsed().as_nanos());
    }
    let allocations = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "allocation in {name}");
    timings.sort_unstable();
    let p = |percent: usize| timings[(timings.len() - 1) * percent / 100];
    println!(
        "{name},{size},{mean:.2},{},{},{},{:.2},{allocations},{memory}",
        p(50),
        p(95),
        p(99),
        1e9 / mean.max(1.0)
    );
}
fn modern(method: &str, fields: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}{fields}}}}}"#
    )
}
fn main() {
    println!(
        "case,size,mean_ns,p50_instrumented_ns,p95_instrumented_ns,p99_instrumented_ns,ops_per_second,total_allocations,workspace_bytes"
    );
    for size in [64, 256, 1024, 4096, 65536] {
        let raw = format!("{{\"x\":\"{}\"}}", "x".repeat(size - 8));
        let mut tokens = [Token::EMPTY; 16];
        assert_eq!(raw.len(), size);
        measure("json_object", size, std::mem::size_of_val(&tokens), || {
            black_box(
                Document::parse(black_box(raw.as_bytes()), &mut tokens, Limits::default())
                    .unwrap()
                    .tokens_used(),
            );
        });
    }
    for (name, raw) in [
        ("initialize", r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"b","version":"1"}}}"#.to_owned()),
        ("legacy_ping", r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_owned()),
        ("discover", modern("server/discover", "")),
    ] {
        let mut tokens = [Token::EMPTY; 128]; let mut output = [0; 2048];
        measure(name, raw.len(), std::mem::size_of_val(&tokens) + output.len(), || {
            let mut server = Server::new("bench", "1");
            let Action::Reply(n) = server.handle(raw.as_bytes(), &mut tokens, Limits::default(), &mut output).unwrap() else { panic!() }; black_box(n);
        });
    }
    let raw = modern("tools/call", r#", "name":"echo", "arguments":{"x":1}"#);
    let mut tokens = [Token::EMPTY; 128];
    let mut output = [0; 2048];
    let mut server = Server::new("bench", "1");
    measure(
        "local_tool_roundtrip",
        raw.len(),
        std::mem::size_of_val(&tokens) + output.len() + server.memory_bytes(),
        || {
            let Action::CallTool(call) = server
                .handle(raw.as_bytes(), &mut tokens, Limits::default(), &mut output)
                .unwrap()
            else {
                panic!()
            };
            assert!(call.name.matches("echo"));
            let text = core::str::from_utf8(call.arguments_bytes()).unwrap();
            black_box(
                server
                    .tool_result(
                        call.context,
                        ToolResult::new(&[Content::Text(text)]),
                        &mut output,
                    )
                    .unwrap(),
            );
        },
    );
    for count in [1, 4, 16, 64, 256, 1024] {
        let mut schema_tokens = [Token::EMPTY; 16];
        let schema = Document::parse(
            br#"{"type":"object"}"#,
            &mut schema_tokens,
            Limits::default(),
        )
        .unwrap();
        let names: Vec<String> = (0..count).map(|i| format!("tool-{i}")).collect();
        let tools: Vec<Tool<'_>> = names
            .iter()
            .map(|name| Tool::new(name, "benchmark tool", schema.root()).unwrap())
            .collect();
        let raw = modern("tools/list", "");
        let mut output = vec![0; count * 256 + 1024];
        let mut tokens = [Token::EMPTY; 128];
        let mut server = Server::new("bench", "1");
        let memory =
            output.len() + std::mem::size_of_val(&tokens) + std::mem::size_of_val(tools.as_slice());
        measure("tools_list", count, memory, || {
            let Action::ListTools { context, .. } = server
                .handle(raw.as_bytes(), &mut tokens, Limits::default(), &mut output)
                .unwrap()
            else {
                panic!()
            };
            black_box(
                server
                    .list_tools(context, &tools, None, &mut output)
                    .unwrap(),
            );
        });
    }
}
