"""Independent, bounded subprocess checks for the MCP client and zcall bridge.

No network or model endpoint is contacted. --serve runs the tiny Python fixture
used below; ordinary invocation builds and checks the actual Rust examples.
"""
import json
import os
import pathlib
import signal
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = pathlib.Path(__file__).resolve()
CURSOR = 'next/第二页/🌍/"\\\n'
TOOL = "unicode_echo"
ARGUMENTS = {"text": '你好，MCP 🌍\n"quoted"\\tail', "number": 17}


def send(value):
    # ASCII-escaped wire output exercises Rust's Unicode/surrogate decoding.
    wire = json.dumps(value, ensure_ascii=True, separators=(",", ":"))
    sys.stdout.buffer.write(wire.encode("utf-8") + b"\n")
    sys.stdout.buffer.flush()


def serve(mode="ok"):
    """A separate implementation of the small legacy profile under test."""
    initialized = False
    pages = 0
    calls = 0
    request_ids = set()
    for line in sys.stdin.buffer:
        request = json.loads(line.decode("utf-8"))
        assert request["jsonrpc"] == "2.0"
        method = request["method"]
        params = request.get("params", {})
        if method == "notifications/initialized":
            assert "id" not in request and not initialized
            initialized = True
            continue
        request_id = request["id"]
        assert type(request_id) is int and request_id not in request_ids
        request_ids.add(request_id)
        if method == "initialize":
            assert not initialized and params["protocolVersion"] == "2025-11-25"
            result = {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {"listChanged": True}},
                "serverInfo": {"name": "independent-python-fixture", "version": "1"},
            }
        elif method == "tools/list":
            assert initialized
            if pages == 0:
                assert "cursor" not in params
                result = {
                    "tools": [{"name": "not_selected", "inputSchema": {"type": "object"}}],
                    "nextCursor": CURSOR,
                }
            else:
                assert pages == 1 and params["cursor"] == CURSOR
                result = {"tools": [{
                    "name": TOOL,
                    "description": "回显 Unicode 🌍 参数",
                    "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
                }]}
            pages += 1
        elif method == "tools/call":
            assert initialized and pages == 2 and calls == 0
            assert params["name"] == TOOL and params["arguments"] == ARGUMENTS
            calls += 1
            send({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
            if mode == "malformed":
                sys.stdout.buffer.write(b'{"jsonrpc":\n')
                sys.stdout.buffer.flush()
                return
            if mode == "rpc_error":
                send({"jsonrpc": "2.0", "id": request_id,
                      "error": {"code": -32602, "message": "Fixture rejects this call"}})
                return
            if mode == "closed":
                return
            if mode == "notifications_only":
                for _ in range(32):
                    send({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
                return
            result = {
                "content": [{"type": "text", "text": json.dumps(params["arguments"], ensure_ascii=False)}],
                "structuredContent": params["arguments"],
                "isError": mode == "tool_error",
            }
        else:
            raise AssertionError("Unexpected client method: " + method)
        send({"jsonrpc": "2.0", "id": request_id, "result": result})
    assert initialized and pages == 2 and calls == 1


def run(command, *, timeout=30, success=True):
    """Bound each subprocess; a timeout also cleans up fixture descendants."""
    options = {"start_new_session": True} if os.name != "nt" else {
        "creationflags": subprocess.CREATE_NEW_PROCESS_GROUP,
    }
    with subprocess.Popen(
        [str(part) for part in command], cwd=ROOT, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, encoding="utf-8", **options,
    ) as process:
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            if os.name == "nt":
                subprocess.run(
                    ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                    capture_output=True, timeout=10, check=False,
                )
            else:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.kill()
            process.communicate(timeout=10)
            raise AssertionError(f"Subprocess exceeded {timeout}s: {command[0]}") from None
        if success:
            assert process.returncode == 0, f"Subprocess failed ({process.returncode}): {stderr[-4000:]}"
        else:
            assert process.returncode != 0, "Expected the rejected exchange to exit unsuccessfully"
        return stdout, stderr


def one_json(stdout):
    lines = [line for line in stdout.splitlines() if line.strip()]
    assert len(lines) == 1, "Client stdout must contain only one final JSON tool result"
    return json.loads(lines[0])


def check():
    run([
        "cargo", "build", "--locked", "--features", "std",
        "--example", "mcp_stdio", "--example", "mcp_client", "--example", "zcall_tools",
    ], timeout=180)
    metadata, _ = run(["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"], timeout=60)
    example_dir = pathlib.Path(json.loads(metadata)["target_directory"]) / "debug" / "examples"
    extension = ".exe" if os.name == "nt" else ""
    client = example_dir / ("mcp_client" + extension)
    server = example_dir / ("mcp_stdio" + extension)
    bridge = example_dir / ("zcall_tools" + extension)

    arguments = {"text": "Rust stdio client → server"}
    stdout, _ = run([client, server, "echo", json.dumps(arguments, ensure_ascii=False)])
    result = one_json(stdout)
    assert result["isError"] is False
    assert json.loads(result["content"][0]["text"]) == arguments

    def independent(mode, success=True):
        return run([
            client, sys.executable, TOOL, json.dumps(ARGUMENTS, ensure_ascii=False),
            SCRIPT, "--serve", mode,
        ], success=success, timeout=15)

    stdout, stderr = independent("ok")
    result = one_json(stdout)
    assert result["isError"] is False and result["structuredContent"] == ARGUMENTS
    assert json.loads(result["content"][0]["text"]) == ARGUMENTS
    assert "notifications/tools/list_changed" in stderr

    stdout, stderr = independent("tool_error", success=False)
    assert one_json(stdout)["isError"] is True and "isError=true" in stderr
    for mode in ("malformed", "rpc_error", "closed", "notifications_only"):
        stdout, stderr = independent(mode, success=False)
        assert not stdout.strip(), "Rejected replies must not appear as successful tool output"
        assert stderr.strip(), "Rejected exchanges should explain their failure"
        if mode == "rpc_error":
            assert "-32602" in stderr
        if mode == "notifications_only":
            assert "32-message budget" in stderr

    stdout, _ = run([bridge])
    request = json.loads(stdout.splitlines()[-1])
    assert request["tools"][0]["function"]["name"] == "echo"
    assistant = next(message for message in request["messages"] if message["role"] == "assistant")
    assert assistant["tool_calls"][0]["id"] == "provider-call-7"
    response = request["messages"][-1]
    assert response["role"] == "tool" and response["tool_call_id"] == "provider-call-7"
    result = json.loads(response["content"])
    assert result["isError"] is False
    assert result["structuredContent"] == {"message": "hello from zcall"}
    assert json.loads(result["content"][0]["text"]) == result["structuredContent"]
    print("client smoke: Rust stdio + independent Python legacy/pagination/Unicode + rejection paths + zcall bridge passed")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--serve":
        serve(sys.argv[2] if len(sys.argv) > 2 else "ok")
    else:
        check()
