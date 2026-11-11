"""Independent Python HTTP peer, plus the real Rust client/server loopback demo."""
import http.client
import json
import pathlib
import queue
import socket
import threading
import subprocess
import sys
from urllib.parse import urlsplit

ROOT = pathlib.Path(__file__).resolve().parents[1]
subprocess.run(["cargo", "build", "--locked", "--features", "std", "--example", "mcp_http"], cwd=ROOT, check=True)
EXE = ROOT / "target" / "debug" / "examples" / ("mcp_http.exe" if sys.platform == "win32" else "mcp_http")
VERSION = "2026-07-28"


def launch(mode):
    process = subprocess.Popen([str(EXE), "server", "0", mode, "--once"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    lines = queue.Queue()
    threading.Thread(target=lambda: lines.put(process.stdout.readline()), daemon=True).start()
    try:
        url = lines.get(timeout=15).strip()
        assert url.startswith("http://127.0.0.1:"), url
        return process, urlsplit(url)
    except BaseException:
        process.kill()
        process.communicate()
        raise


def close(process):
    try:
        _, error = process.communicate(timeout=15)
        assert process.returncode == 0, error
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate()


for mode in ["json", "sse"]:
    process, endpoint = launch(mode)
    try:
        connection = http.client.HTTPConnection(endpoint.hostname, endpoint.port, timeout=10)
        request = {
            "jsonrpc": "2.0", "id": 27, "method": "tools/call",
            "params": {
                "name": "echo", "arguments": {"independent": "Python peer"},
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": VERSION,
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        }
        connection.request("POST", endpoint.path, json.dumps(request), {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "MCP-Protocol-Version": VERSION, "Mcp-Method": "tools/call", "Mcp-Name": "echo",
        })
        response = connection.getresponse()
        assert response.status == 200, (response.status, response.read())
        body = response.read().decode()
        if mode == "sse":
            assert response.getheader("Content-Type") == "text/event-stream"
            messages = []
            for block in body.replace("\r\n", "\n").split("\n\n"):
                data = "\n".join(line[6:] for line in block.splitlines() if line.startswith("data: "))
                if data:
                    messages.append(json.loads(data))
            assert len(messages) == 1, messages
            result = messages[0]
        else:
            result = json.loads(body)
        assert result["id"] == 27 and result["result"]["resultType"] == "complete", result
        assert "Python peer" in result["result"]["content"][0]["text"]
        connection.close()
    finally:
        close(process)

for mode in ["json", "sse"]:
    process, endpoint = launch(mode)
    try:
        result = subprocess.run([str(EXE), "client", endpoint.netloc], capture_output=True, text=True, timeout=15)
        assert result.returncode == 0, result.stderr
        assert "hello over HTTP" in json.loads(result.stdout)["content"][0]["text"]
    finally:
        close(process)

process, endpoint = launch("json")
try:
    connection = http.client.HTTPConnection(endpoint.hostname, endpoint.port, timeout=10)
    request = {"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": {
        "io.modelcontextprotocol/protocolVersion": VERSION,
        "io.modelcontextprotocol/clientCapabilities": {},
    }}}
    connection.request("POST", endpoint.path, json.dumps(request), {
        "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
        "MCP-Protocol-Version": VERSION, "Mcp-Method": "server/discover",
        "Origin": "https://untrusted.invalid",
    })
    response = connection.getresponse()
    assert response.status == 403, (response.status, response.read())
    response.read()
    connection.close()
finally:
    close(process)
# Invalid origins are rejected even when the enclosed JSON is malformed.
process, endpoint = launch("json")
try:
    connection = http.client.HTTPConnection(endpoint.hostname, endpoint.port, timeout=10)
    connection.request("POST", endpoint.path, "{", {
        "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
        "Origin": "https://untrusted.invalid",
    })
    response = connection.getresponse()
    assert response.status == 403, response.status
    response.read()
    connection.close()
finally:
    close(process)

for payload, error_code in [("{", -32700), ('{"jsonrpc":"2.0","id":true,"method":"tools/call"}', -32600), ("[]", -32600)]:
    process, endpoint = launch("json")
    try:
        connection = http.client.HTTPConnection(endpoint.hostname, endpoint.port, timeout=10)
        connection.request("POST", endpoint.path, payload, {
            "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
        })
        response = connection.getresponse()
        assert response.status == 400, response.status
        assert json.loads(response.read())["error"]["code"] == error_code
        connection.close()
    finally:
        close(process)

for method in ["GET", "DELETE", "PUT"]:
    process, endpoint = launch("json")
    try:
        connection = http.client.HTTPConnection(endpoint.hostname, endpoint.port, timeout=10)
        connection.request(method, endpoint.path)
        response = connection.getresponse()
        assert response.status == 405, response.status
        assert response.getheader("Allow") == "POST"
        response.read()
        connection.close()
    finally:
        close(process)
for expectation in ["100-continue", "unsupported"]:
    process, endpoint = launch("json")
    try:
        payload = json.dumps({
            "jsonrpc": "2.0", "id": 72, "method": "tools/call",
            "params": {"name": "echo", "arguments": {"expect": "accepted"},
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": VERSION,
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        }).encode()
        with socket.create_connection((endpoint.hostname, endpoint.port), timeout=10) as peer:
            headers = (
                f"POST {endpoint.path} HTTP/1.1\r\nHost: {endpoint.netloc}\r\n"
                f"Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n"
                f"MCP-Protocol-Version: {VERSION}\r\nMcp-Method: tools/call\r\nMcp-Name: echo\r\n"
                f"Content-Length: {len(payload)}\r\nExpect: {expectation}\r\n\r\n"
            )
            peer.sendall(headers.encode())
            if expectation == "100-continue":
                interim = bytearray()
                while not interim.endswith(b"\r\n\r\n"):
                    byte = peer.recv(1)
                    assert byte and len(interim) < 8192, "missing or oversized interim response"
                    interim.extend(byte)
                assert interim == b"HTTP/1.1 100 Continue\r\n\r\n", interim
                peer.sendall(payload)
            response = http.client.HTTPResponse(peer)
            response.begin()
            if expectation == "100-continue":
                assert response.status == 200, response.status
                result = json.loads(response.read())
                assert result["id"] == 72 and "accepted" in result["result"]["content"][0]["text"]
            else:
                assert response.status == 417, response.status
                response.read()
            response.close()
    finally:
        close(process)
print("HTTP interoperability passed: Python/Rust JSON/SSE, Origin rejection, malformed RPC, methods, 100-continue")
