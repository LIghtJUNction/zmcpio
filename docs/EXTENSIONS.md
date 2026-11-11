# Explicit MCP extension boundaries

The crate provides a bounded wire boundary for optional MCP methods. The host
implements resources, prompts, models, user interaction, subscriptions and tasks.
Only enable methods your application actually serves; a capability is not an
authorization decision.

## Method and result support

| Feature | Entry point | Application responsibility |
|---|---|---|
| Resources | list, templates/list, read; legacy subscribe/unsubscribe | URI authorization, content lookup, template/schema semantics and change delivery |
| Prompts | list/get | Prompt lookup, argument rules and permission checks |
| Completion | completion/complete | Generate values, limits and schema semantics |
| Legacy logging | logging/setLevel | Configure host logging and deliver permitted notices |
| Legacy callbacks | roots/list, sampling/createMessage, elicitation/create | Correlate callback IDs, obtain consent, run the model/UI and return a result |
| Modern MRTR | input_required, requestState, inputRequests/inputResponses | Protect opaque state, authorize each requested input, explicitly continue with a new request ID |
| Modern subscriptions | subscriptions/listen and Subscription | Own the long-lived stream, emit acknowledgments/notifications, bind it to authentication |
| Modern Tasks extension | task handle, tasks/get, tasks/update, tasks/cancel | Durable task state, ownership, expiry, scheduler and lifecycle transitions |
| Legacy 2025-11-25 Tasks | nested task handle, tasks/get/result/list/cancel | Legacy task storage and execution, negotiated task-augmented calls |

Generic result views preserve the full borrowed JSON rather than copying into a
partial model. Required container shapes, discriminators, IDs, supported versions
and capability gates are checked. This is not full JSON Schema validation of
every nested resource, prompt, sampling or elicitation result.

## Request and server dispatch

Enable server routes with `Server::with_extensions(&[Method::...])`.
`Client::request(method, params, output)` validates the method's wire fields,
reserves a bounded pending slot only after successful serialization and adds
modern request-scoped metadata while preserving non-reserved application metadata.
It rejects caller attempts to replace reserved protocol/capability/clientInfo keys.

`Server::handle` returns `Action::Extension { context, method }`. The application
authorizes and performs the operation, then calls `Server::extension_result`.
Disabled/unknown methods remain method-not-found; enabling resources does not
quietly enable filesystem access or callbacks. `Client::receive` returns a
borrowed `Event::ExtensionResult` for the correlated request.

Run the public-API example:

```sh
cargo run --locked --example mcp_extensions
```

It performs an in-memory resource exchange, returns a modern task handle, and
explicitly polls a completed fixture. The fixture is not a task scheduler.

## Capabilities, callbacks and MRTR

Client capabilities default to empty. Use `with_client_capabilities` with a
retained parsed object to opt in. Sampling tools/context and elicitation form/URL
sub-capabilities are checked independently. URL permission does not grant form
permission; a toolChoice requires the sampling tools capability. Capability
objects are type-checked rather than treating arbitrary truthy values as consent.

Legacy `callback_request` encodes an authorized server-to-client request. The
application owns the callback ID registry, deadlines and returned result
validation. Modern callbacks instead appear inside MRTR input requests; they are
not unsolicited nested JSON-RPC exchanges on the HTTP response stream.

For modern tools/call, resources/read and prompts/get,
`Server::input_required` can return opaque requestState and/or inputRequests.
The client validates requested callbacks against its declared capabilities before
releasing the pending slot. It does not execute them or automatically retry the
operation. After explicit approval and input handling, use `Client::request`
with inputResponses and the original opaque requestState; a new RPC ID is
allocated. Keep state integrity, ownership, expiry and one-time continuation
policy in the host.

## Modern Tasks

Opt in with this client capability object:

```json
{"extensions":{"io.modelcontextprotocol/tasks":{}}}
```

A server advertises modern Tasks when get, update and cancel handlers are all
enabled. The host must create and authenticate-bind the task before calling
`Server::task_result`. The result is a flat `resultType: "task"` handle,
reported to the client as `Event::TaskCreated`; it is not a completed tool result.

Poll using tasks/get, supply inputs using tasks/update and request cancellation
using tasks/cancel. There is no modern tasks/list or tasks/result. Poll results
carry detailed state: completed output, failure information or pending inputs.
`TaskView` checks status-specific wire containers and nonnegative integer
millisecond fields. The host enforces timestamp formatting, expiry and legal
state transitions and checks the returned taskId against its retained task ID.

No automatic polling, scheduling, state database, generated task ID, callback
execution or task ownership policy is included. In particular, do not run
tools/call again to “poll” a task.

## Legacy Tasks

Legacy Tasks require the negotiated 2025-11-25 profile and the server's advertised
tasks.requests.tools.call support. Use an explicit params.task for task-augmented
tools/call. A successful creation contains a nested task object; later result
retrieval uses tasks/result rather than the modern inline get result. Older
legacy profiles reject these methods before allocating a pending slot.

Task TTL and polling interval field names differ by era. The codec keeps those
distinctions; it does not manufacture a modern task from a legacy response.

## Subscriptions

Keep the request's filter and the acknowledgment input/token storage alive while
using `Subscription`. Notifications must carry the matching subscriptionId.
The acknowledgment must precede data and cannot broaden the requested filters;
the final response must also carry the matching ID. Duplicate acknowledgments,
unrequested notification classes and wrong IDs are rejected.

Resource updates match exact URIs by default. Use
`observe_with_resource_match` only with an explicit, authorized URI-aware
subresource rule; naive string prefixes are not a safe resource policy.
Task subscriptions require the Tasks capability and accept only requested task
IDs with valid detailed task states.

The host chooses stream lifetime, cancellation, delivery queues and notification
authorization. The library does not open a background connection or retain an
unbounded notification queue.

## Validation scope and evidence

Capacity failures do not consume a pending request ID or slot. Malformed or
uncorrelated replies do not complete outstanding work. Tests cover resource
roundtrips, callback sub-capabilities, MRTR continuation with new IDs, task-era
differences, task input validation, subscription acknowledgment order, filter
subsets and final-response correlation.

Results beyond checked wire fields remain application data. This crate neither
claims to implement every future/vendor extension nor passes unrecognized
requests through to an executor. JSON-RPC batches are not supported.

Sources checked 2026-11-11:

- https://modelcontextprotocol.io/specification/2026-07-28
- https://modelcontextprotocol.io/specification/2025-11-25
- https://modelcontextprotocol.io/extensions/tasks/overview
- https://github.com/modelcontextprotocol/ext-tasks/blob/main/schema/2026-07-28/schema.ts

See `tests/extensions.rs`, `tests/client.rs` and
`examples/mcp_extensions.rs` for executable contracts.
