"""Independent Python HTTP peer, plus the real Rust client/server loopback demo."""
import http.client
import json
import pathlib
import subprocess
import sys
from urllib.parse import urlsplit

ROOT = pathlib.Path(__file__).resolve().parents[1]
subprocess.run(["cargo", "build", "--locked", "--features", "std", "--example", "mcp_http"], cwd=ROOT, check=True)
EXE = ROOT / "target" / "debug" / ("mcp_http.exe" if sys.platform == "win32" else "mcp_http")
VERSION = "2026-07-28"


def launch(mode):
    process = subprocess.Popen([str(EXE), "server", "0", mode, "--once"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    url = process.stdout.readline().strip()
    assert url.startswith("http://127.0.0.1:"), url
    return process, urlsplit(url)


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
print("HTTP interoperability passed: Python JSON/SSE, Rust JSON/SSE, Origin rejection")
