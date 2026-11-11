"""Exercise the real stdio executable using Python's independent JSON parser."""
import json
import subprocess

meta = {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}
messages = [
    {"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": meta}},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": meta}},
    {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"_meta": meta, "name": "echo", "arguments": {"text": "hello"}}},
    {"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 999}},
]
p = subprocess.run(["cargo", "run", "--locked", "--quiet", "--example", "mcp_stdio"], input="".join(json.dumps(m) + "\n" for m in messages), text=True, capture_output=True, check=True, timeout=120)
responses = [json.loads(line) for line in p.stdout.splitlines()]
assert [r["id"] for r in responses] == [1, 2, 3]
assert all(r["result"]["resultType"] == "complete" for r in responses)
assert responses[1]["result"]["tools"][0]["name"] == "echo"
assert json.loads(responses[2]["result"]["content"][0]["text"]) == {"text": "hello"}
print("stdio round-trip: 3 requests + 1 notification passed")
