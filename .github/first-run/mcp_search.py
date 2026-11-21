"""Minimal MCP stdio client for the first-run workflow.

Lists the tools, runs one csr_reflect_on_past search and exits 1 unless the
expected token is in the result. Usage: mcp_search.py <csr-engine> <query> <token>
"""
import json
import subprocess
import sys
import time

engine, query, token = sys.argv[1], sys.argv[2], sys.argv[3]
p = subprocess.Popen([engine], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.DEVNULL, text=True)


def send(obj):
    p.stdin.write(json.dumps(obj) + "\n")
    p.stdin.flush()


def recv(want):
    while True:
        line = p.stdout.readline()
        if not line:
            return None
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("id") == want:
            return msg


send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                 "clientInfo": {"name": "first-run", "version": "1"}}})
print("initialize:", (recv(1) or {}).get("result", {}).get("serverInfo"))
send({"jsonrpc": "2.0", "method": "notifications/initialized"})
send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
tools = [t["name"] for t in (recv(2) or {}).get("result", {}).get("tools", [])]
print("tools:", len(tools))
t0 = time.time()
send({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
      "params": {"name": "csr_reflect_on_past", "arguments": {"query": query, "limit": 5}}})
res = (recv(3) or {}).get("result", {})
text = "\n".join(c.get("text", "") for c in res.get("content", []))
print(f"search: {time.time() - t0:.2f}s, {len(text)} chars, isError={res.get('isError')}")
p.stdin.close()
p.terminate()
found = token.lower() in text.lower()
print("token found:", found)
sys.exit(0 if found and len(tools) >= 16 else 1)
