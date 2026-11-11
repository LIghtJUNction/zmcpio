use zcall::v1 as model;
use zmcpio::{
    ErrorKind,
    bridge::{self, BridgeError, ResultEncoding},
    client::{Client, Event, Pending},
    json::{Limits, Token},
    mcp::Era,
};

fn model_call<'a>(id: &'a str, name: &'a str, arguments: model::Data<'a>) -> model::Content<'a> {
    model::Content::ToolCall {
        id: model::Presence::Value(model::Text::new(id)),
        name: model::Text::new(name),
        arguments,
        legacy: false,
    }
}

fn response(id: i64, fields: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"resultType":"complete","_meta":{{"io.modelcontextprotocol/serverInfo":{{"name":"server","version":"1"}}}},{fields}}}}}"#,
    )
}

#[test]
fn discovered_signatures_feed_zcall_without_losing_escaped_text_or_schema() {
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("app", "1", Era::Modern, &mut slots);
    let mut output = [0; 2048];
    let sent = client.list_tools(None, &mut output).unwrap();
    let input = response(
        sent.id,
        r#""tools":[{"name":"\u0065cho","description":"a\nb","inputSchema":{"type":"object","properties":{"n":{"type":"integer","maximum":9007199254740993}}},"outputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}]"#,
    );
    let mut tokens = [Token::EMPTY; 128];
    let Event::Tools { page, .. } = client
        .receive(input.as_bytes(), &mut tokens, Limits::default())
        .unwrap()
    else {
        panic!("expected tool list")
    };
    let mut model_tokens = model::TokenStorage::<128>::new();
    let tool = bridge::tool_definition(
        page.tools().next().unwrap(),
        &mut model_tokens,
        model::Limits::default(),
    )
    .unwrap();
    assert!(tool.name.eq_str("echo"));
    assert!(tool.description.value().unwrap().eq_str("a\nb"));
    assert_eq!(tool.strict, model::Presence::Value(false));
    assert!(!tool.legacy);
    // Conversion output does not borrow the zcall token scratch.
    model::Json::parse(b"null", &mut model_tokens, model::Limits::default()).unwrap();
    let schema = tool.parameters.value().unwrap();
    assert!(
        std::str::from_utf8(schema.raw())
            .unwrap()
            .contains("9007199254740993")
    );
    let mut records = model::RecordStorage::<8>::new();
    let mut request = model::RequestBuilder::new(
        model::Protocol::OpenAiChat.into(),
        model::Presence::Value(model::Text::new("test-model")),
        &mut records,
    )
    .unwrap()
    .finish();
    request.append_tool(tool).unwrap();
    assert!(request.append_tool(tool).is_err());
    assert_eq!(request.tools().count(), 1);
    request
        .append_message(model::Role::User, &[model::Content::text("use echo")])
        .unwrap();
    for protocol in [
        model::Protocol::OpenAiChat,
        model::Protocol::OpenAiResponses,
        model::Protocol::AnthropicMessages,
        model::Protocol::GeminiGenerateContent,
        model::Protocol::GeminiInteractions,
    ] {
        let mut wire = [0; 4096];
        let report = request
            .encode(
                protocol.into(),
                &mut wire,
                &mut [0; 1024],
                &mut model_tokens,
                model::EncodeContext::default().with_max_tokens(128),
                model::Policy::default(),
            )
            .unwrap_or_else(|e| panic!("{protocol:?}: {e}"));
        assert!(
            std::str::from_utf8(&wire[..report.written])
                .unwrap()
                .contains("9007199254740993")
        );
        assert!(!report.has_losses());
    }
}

#[test]
fn complete_call_decodes_escapes_and_reuses_argument_storage_after_encoding() {
    let mut model_tokens = model::TokenStorage::<32>::new();
    let id = model::Json::parse(
        br#""call\u005f9""#,
        &mut model_tokens,
        model::Limits::default(),
    )
    .unwrap()
    .as_text()
    .unwrap();
    let name = model::Json::parse(
        br#""\u0065cho""#,
        &mut model_tokens,
        model::Limits::default(),
    )
    .unwrap()
    .as_text()
    .unwrap();
    let arguments = model::Json::parse(
        br#""{\"n\":9007199254740993}""#,
        &mut model_tokens,
        model::Limits::default(),
    )
    .unwrap()
    .as_text()
    .unwrap();
    let content = model::Content::ToolCall {
        id: model::Presence::Value(id),
        name,
        arguments: model::Data::JsonText(arguments),
        legacy: false,
    };
    let mut identity = [0; 64];
    let mut argument_bytes = [0; 64];
    let mut tokens = [Token::EMPTY; 16];
    let prepared = bridge::prepare_call(
        content,
        &mut identity,
        &mut argument_bytes,
        &mut tokens,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(prepared.model_call_id(), "call_9");
    assert_eq!(prepared.name(), "echo");
    assert_eq!(prepared.arguments().raw(), r#"{"n":9007199254740993}"#);
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("app", "1", Era::Modern, &mut slots);
    let mut output = [0; 2048];
    let invocation = prepared.start(&mut client, &mut output).unwrap();
    // These borrows have ended although the model identity binding remains live.
    argument_bytes.fill(0);
    tokens.fill(Token::EMPTY);
    assert_eq!(invocation.model_call_id(), "call_9");
    let wire = std::str::from_utf8(&output[..invocation.outgoing().len]).unwrap();
    assert!(wire.contains(r#""name":"echo""#));
    assert!(wire.contains(r#""arguments":{"n":9007199254740993}"#));
}

#[test]
fn unescaped_calls_need_no_identity_or_argument_byte_scratch() {
    let mut identity = [];
    let mut arguments = [];
    let mut tokens = [Token::EMPTY; 16];
    let call = model_call(
        "provider-id",
        "echo",
        model::Data::JsonText(model::Text::new(r#"{"x":1}"#)),
    );
    let prepared = bridge::prepare_call(
        call,
        &mut identity,
        &mut arguments,
        &mut tokens,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(prepared.model_call_id(), "provider-id");
    assert_eq!(prepared.arguments().raw(), r#"{"x":1}"#);
}

#[test]
fn invalid_or_incomplete_calls_are_not_guessed_or_executed() {
    let mut identity = [0; 64];
    let mut arguments = [0; 64];
    let mut tokens = [Token::EMPTY; 32];
    let mut model_tokens = model::TokenStorage::<16>::new();
    for data in [
        model::Data::Text(model::Text::new("{}")),
        model::Data::JsonText(model::Text::new("{\"x\":")),
        model::Data::JsonText(model::Text::new("[]")),
        model::Data::JsonText(model::Text::new(r#"{"x":1,"x":2}"#)),
        model::Data::Json(
            model::Json::parse(b"null", &mut model_tokens, model::Limits::default()).unwrap(),
        ),
    ] {
        assert!(
            bridge::prepare_call(
                model_call("id", "echo", data),
                &mut identity,
                &mut arguments,
                &mut tokens,
                Limits::default()
            )
            .is_err()
        );
    }
    for call in [
        model::Content::text("not a call"),
        model_call("", "echo", model::Data::EmptyObject),
        model_call("id", "", model::Data::EmptyObject),
        model::Content::ToolCall {
            id: model::Presence::Null,
            name: model::Text::new("echo"),
            arguments: model::Data::EmptyObject,
            legacy: false,
        },
    ] {
        assert!(
            bridge::prepare_call(
                call,
                &mut identity,
                &mut arguments,
                &mut tokens,
                Limits::default()
            )
            .is_err()
        );
    }
}

#[test]
fn exact_identity_capacity_and_output_retry_are_bounded() {
    let mut model_tokens = model::TokenStorage::<8>::new();
    let id = model::Json::parse(br#""\u0069d""#, &mut model_tokens, model::Limits::default())
        .unwrap()
        .as_text()
        .unwrap();
    let call = model::Content::ToolCall {
        id: model::Presence::Value(id),
        name: model::Text::new("echo"),
        arguments: model::Data::EmptyObject,
        legacy: false,
    };
    let mut identity = [0; 1];
    let mut args = [];
    let mut tokens = [Token::EMPTY; 8];
    let error = bridge::prepare_call(
        call,
        &mut identity,
        &mut args,
        &mut tokens,
        Limits::default(),
    )
    .unwrap_err();
    assert!(
        matches!(error, BridgeError::Mcp(error) if error.kind == ErrorKind::Buffer && error.needed == 2)
    );
    let mut identity = [0; 2];
    let prepared = bridge::prepare_call(
        call,
        &mut identity,
        &mut args,
        &mut tokens,
        Limits::default(),
    )
    .unwrap();
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("app", "1", Era::Modern, &mut slots);
    assert!(prepared.start(&mut client, &mut [0; 1]).is_err());
    let invocation = prepared.start(&mut client, &mut [0; 1024]).unwrap();
    assert_eq!(invocation.model_call_id(), "id");
}

#[test]
fn checked_results_preserve_model_id_full_multimodal_envelope_and_execution_error() {
    let mut identity = [];
    let mut args = [];
    let mut call_tokens = [Token::EMPTY; 16];
    let call = model_call("provider-call-9", "echo", model::Data::EmptyObject);
    let prepared = bridge::prepare_call(
        call,
        &mut identity,
        &mut args,
        &mut call_tokens,
        Limits::default(),
    )
    .unwrap();
    let mut slots = [Pending::EMPTY; 2];
    let mut client = Client::new("app", "1", Era::Modern, &mut slots);
    let mut output = [0; 2048];
    let invocation = prepared.start(&mut client, &mut output).unwrap();
    let input = response(
        invocation.outgoing().id,
        r#""content":[{"type":"text","text":"line\n2"},{"type":"image","data":"AA==","mimeType":"image/png"}],"structuredContent":null,"isError":true"#,
    );
    let mut result_tokens = [Token::EMPTY; 128];
    let Event::ToolResult { id, result } = client
        .receive(input.as_bytes(), &mut result_tokens, Limits::default())
        .unwrap()
    else {
        panic!("expected tool result")
    };
    let mut model_tokens = model::TokenStorage::<128>::new();
    let mut tiny_tokens = model::TokenStorage::<1>::new();
    assert!(
        matches!(invocation.content(id, result, &mut tiny_tokens, model::Limits::default()), Err(BridgeError::Model(error)) if error.kind == model::ErrorKind::Capacity)
    );
    assert!(
        matches!(invocation.content(id + 1, result, &mut model_tokens, model::Limits::default()), Err(BridgeError::Mcp(e)) if e.kind == ErrorKind::UnknownId)
    );
    let model::Content::ToolResult {
        id,
        name,
        result: data,
        is_error,
    } = invocation
        .content(id, result, &mut model_tokens, model::Limits::default())
        .unwrap()
    else {
        panic!()
    };
    assert!(id.value().unwrap().eq_str("provider-call-9"));
    assert!(name.value().unwrap().eq_str("echo"));
    assert_eq!(is_error, model::Presence::Missing);
    let model::Content::ToolResult { is_error, .. } = invocation
        .content_with_encoding(
            invocation.outgoing().id,
            result,
            ResultEncoding::NativeErrorFlag,
            &mut model_tokens,
            model::Limits::default(),
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(is_error, model::Presence::Value(true));
    let model::Data::Json(json) = data else {
        panic!("must not flatten content")
    };
    assert_eq!(json.raw(), result.raw().raw().as_bytes());
    assert!(
        std::str::from_utf8(json.raw())
            .unwrap()
            .contains(r#""structuredContent":null"#)
    );
}

#[test]
fn append_result_uses_zcall_checked_history_and_rejects_duplicates() {
    let mut identity = [];
    let mut args = [];
    let mut call_tokens = [Token::EMPTY; 8];
    let call = model_call("call-1", "echo", model::Data::EmptyObject);
    let prepared = bridge::prepare_call(
        call,
        &mut identity,
        &mut args,
        &mut call_tokens,
        Limits::default(),
    )
    .unwrap();
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("app", "1", Era::Modern, &mut slots);
    let invocation = prepared.start(&mut client, &mut [0; 1024]).unwrap();
    let input = response(
        invocation.outgoing().id,
        r#""content":[],"structuredContent":42"#,
    );
    let mut result_tokens = [Token::EMPTY; 64];
    let Event::ToolResult { id, result } = client
        .receive(input.as_bytes(), &mut result_tokens, Limits::default())
        .unwrap()
    else {
        panic!()
    };
    let mut model_tokens = model::TokenStorage::<64>::new();
    let mut storage = model::RecordStorage::<8>::new();
    let mut builder = model::RequestBuilder::new(
        model::Protocol::OpenAiChat.into(),
        model::Presence::Missing,
        &mut storage,
    )
    .unwrap();
    builder.message(model::Role::Assistant, &[call]).unwrap();
    let mut request = builder.finish();
    invocation
        .append_result(
            &mut request,
            id,
            result,
            &mut model_tokens,
            model::Limits::default(),
        )
        .unwrap();
    assert_eq!(request.messages().count(), 2);
    let before = request.records_used();
    assert!(
        matches!(invocation.append_result(&mut request, id, result, &mut model_tokens, model::Limits::default()), Err(BridgeError::Model(e)) if e.kind == model::ErrorKind::Lifecycle)
    );
    assert_eq!(request.records_used(), before);
}

#[test]
fn portable_results_encode_success_and_error_for_all_tool_capable_zcall_targets() {
    for failed in [false, true] {
        let mut identity = [];
        let mut args = [];
        let mut call_tokens = [Token::EMPTY; 8];
        let call = model_call("provider-call", "echo", model::Data::EmptyObject);
        let prepared = bridge::prepare_call(
            call,
            &mut identity,
            &mut args,
            &mut call_tokens,
            Limits::default(),
        )
        .unwrap();
        let mut slots = [Pending::EMPTY; 1];
        let mut client = Client::new("app", "1", Era::Modern, &mut slots);
        let invocation = prepared.start(&mut client, &mut [0; 1024]).unwrap();
        let input = response(
            invocation.outgoing().id,
            &format!(
                r#""content":[{{"type":"text","text":"result"}}],"structuredContent":{{"value":42}},"isError":{failed}"#
            ),
        );
        let mut result_tokens = [Token::EMPTY; 128];
        let Event::ToolResult { id, result } = client
            .receive(input.as_bytes(), &mut result_tokens, Limits::default())
            .unwrap()
        else {
            panic!()
        };
        for protocol in [
            model::Protocol::OpenAiChat,
            model::Protocol::OpenAiResponses,
            model::Protocol::AnthropicMessages,
            model::Protocol::GeminiGenerateContent,
            model::Protocol::GeminiInteractions,
        ] {
            let mut model_tokens = model::TokenStorage::<256>::new();
            let mut storage = model::RecordStorage::<16>::new();
            let mut builder = model::RequestBuilder::new(
                protocol.into(),
                model::Presence::Value(model::Text::new("test-model")),
                &mut storage,
            )
            .unwrap();
            builder.message(model::Role::Assistant, &[call]).unwrap();
            let mut request = builder.finish();
            invocation
                .append_result(
                    &mut request,
                    id,
                    result,
                    &mut model_tokens,
                    model::Limits::default(),
                )
                .unwrap();
            let mut output = [0; 4096];
            let mut scratch = [0; 1024];
            let report = request
                .encode(
                    protocol.into(),
                    &mut output,
                    &mut scratch,
                    &mut model_tokens,
                    model::EncodeContext::default().with_max_tokens(128),
                    model::Policy::default(),
                )
                .unwrap_or_else(|e| panic!("{protocol:?}: {e}"));
            let wire = std::str::from_utf8(&output[..report.written]).unwrap();
            assert!(
                wire.contains(&format!(r#""isError":{failed}"#))
                    || wire.contains(&format!(r#"\"isError\":{failed}"#)),
                "{protocol:?}: error indicator lost: {wire}"
            );
            assert!(
                wire.contains("provider-call"),
                "{protocol:?}: model ID lost"
            );
            assert!(!report.has_losses());
        }
    }
}

#[test]
fn native_error_flag_is_opt_in_and_keeps_zcall_target_checks() {
    let mut identity = [];
    let mut args = [];
    let mut call_tokens = [Token::EMPTY; 8];
    let call = model_call("call-error", "echo", model::Data::EmptyObject);
    let prepared = bridge::prepare_call(
        call,
        &mut identity,
        &mut args,
        &mut call_tokens,
        Limits::default(),
    )
    .unwrap();
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("app", "1", Era::Modern, &mut slots);
    let invocation = prepared.start(&mut client, &mut [0; 1024]).unwrap();
    let input = response(invocation.outgoing().id, r#""content":[],"isError":true"#);
    let mut result_tokens = [Token::EMPTY; 64];
    let Event::ToolResult { id, result } = client
        .receive(input.as_bytes(), &mut result_tokens, Limits::default())
        .unwrap()
    else {
        panic!()
    };
    for protocol in [
        model::Protocol::AnthropicMessages,
        model::Protocol::OpenAiChat,
    ] {
        let mut model_tokens = model::TokenStorage::<128>::new();
        let mut storage = model::RecordStorage::<16>::new();
        let mut builder = model::RequestBuilder::new(
            protocol.into(),
            model::Presence::Value(model::Text::new("test-model")),
            &mut storage,
        )
        .unwrap();
        builder.message(model::Role::Assistant, &[call]).unwrap();
        let mut request = builder.finish();
        invocation
            .append_result_with_encoding(
                &mut request,
                id,
                result,
                ResultEncoding::NativeErrorFlag,
                &mut model_tokens,
                model::Limits::default(),
            )
            .unwrap();
        let mut output = [0; 4096];
        let encoded = request.encode(
            protocol.into(),
            &mut output,
            &mut [0; 1024],
            &mut model_tokens,
            model::EncodeContext::default().with_max_tokens(128),
            model::Policy::default(),
        );
        if protocol == model::Protocol::AnthropicMessages {
            let report = encoded.unwrap();
            assert!(
                std::str::from_utf8(&output[..report.written])
                    .unwrap()
                    .contains(r#""is_error":true"#)
            );
        } else {
            assert_eq!(encoded.unwrap_err().kind, model::ErrorKind::Unsupported);
        }
    }
}

#[test]
fn streaming_ready_tool_integrates_without_using_provider_item_id() {
    let input = concat!(
        "data: {\"id\":\"response\",\"model\":\"model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-stream\",\"type\":\"function\",\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"text\\\":\\\"stream\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let mut framing = [0; 2048];
    let mut payload = [0; 2048];
    let mut aggregate = [0; 4096];
    let mut workspace = model::StreamWorkspace::<128, 32, 4>::new();
    let mut session = model::Session::new(
        model::SessionConfig::new(model::Protocol::OpenAiChat.into()),
        &mut framing,
        &mut payload,
        &mut aggregate,
        &mut workspace,
    )
    .unwrap();
    session.prepare().unwrap();
    let step = session.step(input.as_bytes(), true, |_| model::Control::Continue);
    assert_eq!(step.status, model::StepStatus::Done);
    let ready = session.tool("call-stream").unwrap();
    let mut tokens = [Token::EMPTY; 16];
    let prepared = bridge::prepare_ready(ready, &mut tokens, Limits::default()).unwrap();
    assert_eq!(prepared.model_call_id(), "call-stream");
    assert_eq!(prepared.arguments().raw(), r#"{"text":"stream"}"#);
    let mut detached_identity = [0; 64];
    let prepared = bridge::prepare_ready_detached(
        ready,
        &mut detached_identity,
        &mut tokens,
        Limits::default(),
    )
    .unwrap();
    let mut slots = [Pending::EMPTY; 1];
    let mut client = Client::new("app", "1", Era::Modern, &mut slots);
    let invocation = prepared.start(&mut client, &mut [0; 1024]).unwrap();
    assert_eq!(invocation.model_call_id(), "call-stream");
    assert_eq!(
        session.pending_tool_count(),
        1,
        "encoding must not mark a model tool as completed"
    );
    // The detached invocation does not borrow Session, even though the actual
    // ToolReady event did. Acknowledgement is explicit after storing a result.
    session
        .submit_tool_result(invocation.model_call_id())
        .unwrap();
    assert_eq!(session.state(), model::CallState::ToolsResolved);
}
