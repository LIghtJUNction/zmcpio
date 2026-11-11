//! In-memory wire examples: the host supplies all resource contents and task state.
//! No network, filesystem reads, model calls, scheduler, or automatic callbacks.
use zmcpio::{
    client::{Client, Event, Pending},
    extensions::{Method, TaskStatus, TaskView},
    json::{Document, Limits, Token, Value},
    mcp::{Action, Era, Server},
};

fn json<'a>(input: &'a [u8], tokens: &'a mut [Token]) -> Value<'a> {
    Document::parse(input, tokens, Limits::default())
        .unwrap()
        .root()
}

fn resources() {
    let mut pending = [Pending::EMPTY; 1];
    let mut client = Client::new("example", "1", Era::Modern, &mut pending);
    let mut server = Server::new("example", "1").with_extensions(&[Method::ResourcesRead]);
    let mut params_tokens = [Token::EMPTY; 16];
    let params = json(br#"{"uri":"demo://greeting"}"#, &mut params_tokens);
    let mut request = [0; 2048];
    let sent = client
        .request(Method::ResourcesRead, Some(params), &mut request)
        .unwrap();
    let mut request_tokens = [Token::EMPTY; 128];
    let mut response = [0; 2048];
    let Action::Extension { context, method } = server
        .handle(
            &request[..sent.len],
            &mut request_tokens,
            Limits::default(),
            &mut response,
        )
        .unwrap()
    else {
        panic!("expected opted-in resource request");
    };
    assert_eq!(method, Method::ResourcesRead);
    // Host authorization and resource lookup belong here; this example has one fixture.
    let uri = context
        .request
        .params
        .unwrap()
        .get("uri")
        .unwrap()
        .string()
        .unwrap();
    assert!(uri.matches("demo://greeting"));
    let mut result_tokens = [Token::EMPTY; 32];
    let result = json(
        br#"{"contents":[{"uri":"demo://greeting","mimeType":"text/plain","text":"Hello"}]}"#,
        &mut result_tokens,
    );
    let len = server
        .extension_result(context, result, &mut response)
        .unwrap();
    let mut response_tokens = [Token::EMPTY; 128];
    let Event::ExtensionResult { id, method, result } = client
        .receive(&response[..len], &mut response_tokens, Limits::default())
        .unwrap()
    else {
        panic!("expected borrowed resource result");
    };
    assert_eq!((id, method), (sent.id, Method::ResourcesRead));
    assert_eq!(result.get("contents").unwrap().elements().count(), 1);
    assert_eq!(client.pending_count(), 0);
}

fn tasks() {
    let mut caps_tokens = [Token::EMPTY; 32];
    let capabilities = json(
        br#"{"extensions":{"io.modelcontextprotocol/tasks":{}}}"#,
        &mut caps_tokens,
    );
    let mut pending = [Pending::EMPTY; 1];
    let mut client = Client::new("example", "1", Era::Modern, &mut pending)
        .with_client_capabilities(capabilities)
        .unwrap();
    let methods = [Method::TasksGet, Method::TasksUpdate, Method::TasksCancel];
    let mut server = Server::new("example", "1").with_extensions(&methods);
    let mut request = [0; 2048];
    let mut response = [0; 2048];
    let mut request_tokens = [Token::EMPTY; 128];
    let mut response_tokens = [Token::EMPTY; 128];

    let sent = client.call_tool("slow-demo", None, &mut request).unwrap();
    let Action::CallTool(call) = server
        .handle(
            &request[..sent.len],
            &mut request_tokens,
            Limits::default(),
            &mut response,
        )
        .unwrap()
    else {
        panic!("expected tool request");
    };
    // In production, authorize, generate an unguessable ID, and durably create
    // the task BEFORE returning a handle. Here both states are fixed fixtures.
    // Implement get/update/cancel handlers before advertising the Tasks extension.
    let mut task_tokens = [Token::EMPTY; 64];
    let task = json(
        br#"{"taskId":"demo-1","status":"working","createdAt":"2026-11-11T00:00:00Z","lastUpdatedAt":"2026-11-11T00:00:00Z","ttlMs":60000,"pollIntervalMs":1000}"#,
        &mut task_tokens,
    );
    let len = server
        .task_result(call.context, task, &mut response)
        .unwrap();
    let Event::TaskCreated { id, task } = client
        .receive(&response[..len], &mut response_tokens, Limits::default())
        .unwrap()
    else {
        panic!("expected task handle, not tool output");
    };
    assert_eq!(id, sent.id);
    assert_eq!(task.status(), TaskStatus::Working);
    assert!(task.task_id().matches("demo-1"));

    // Poll explicitly; never repeat the original side-effecting tools/call.
    // A real caller honors pollIntervalMs and enforces its own timeout policy.
    let mut params_tokens = [Token::EMPTY; 16];
    let params = json(br#"{"taskId":"demo-1"}"#, &mut params_tokens);
    let sent = client
        .request(Method::TasksGet, Some(params), &mut request)
        .unwrap();
    let Action::Extension { context, method } = server
        .handle(
            &request[..sent.len],
            &mut request_tokens,
            Limits::default(),
            &mut response,
        )
        .unwrap()
    else {
        panic!("expected explicit task poll");
    };
    assert_eq!(method, Method::TasksGet);
    // The host checks task ownership and reads state. No job runs inside zmcpio.
    let completed = json(
        br#"{"taskId":"demo-1","status":"completed","createdAt":"2026-11-11T00:00:00Z","lastUpdatedAt":"2026-11-11T00:00:01Z","ttlMs":60000,"result":{"content":[{"type":"text","text":"Done"}]}}"#,
        &mut task_tokens,
    );
    let len = server
        .extension_result(context, completed, &mut response)
        .unwrap();
    let Event::ExtensionResult { result, .. } = client
        .receive(&response[..len], &mut response_tokens, Limits::default())
        .unwrap()
    else {
        panic!("expected task state");
    };
    let completed = TaskView::parse_detailed(result).unwrap();
    assert!(completed.task_id().matches("demo-1"));
    assert_eq!(completed.status(), TaskStatus::Completed);
    assert!(
        completed
            .raw()
            .get("result")
            .unwrap()
            .get("content")
            .is_some()
    );
    assert_eq!(client.pending_count(), 0);
}

fn main() {
    resources();
    tasks();
    println!("Resources and Tasks wire roundtrips passed; all work remains host-owned.");
}
