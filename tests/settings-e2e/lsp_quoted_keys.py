"""Send hover and completion requests to `runner lsp` for quoted task keys."""

import json
import subprocess
import sys

runner = sys.argv[1]
cases = [
    ('[tasks."package.json:build".runtime]\njavascript = "bun"\n', (1, 3), (1, 13)),
    ("[tasks.'package.json:build'.runtime]\njavascript = \"bun\"\n", (1, 3), (1, 13)),
    ('[tasks."build.prod"]\nruntime.javascript = "bun"\n', (1, 12), (1, 21)),
    ('[tasks.build.runtime]\njavascript = "bun"\n', (1, 3), (1, 13)),
]

proc = subprocess.Popen([runner, "lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)


def send(message):
    assert proc.stdin is not None
    body = json.dumps(message).encode()
    proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    proc.stdin.flush()


def receive(want_id):
    assert proc.stdout is not None
    while True:
        length = 0
        while True:
            line = proc.stdout.readline().strip()
            if not line:
                break
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":")[1])
        message = json.loads(proc.stdout.read(length))
        if message.get("id") == want_id:
            return message


send({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {"capabilities": {}},
})
receive(1)
send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
request = 2
for index, (text, hover_at, complete_at) in enumerate(cases):
    uri = f"file:///tmp/case{index}/runner.toml"
    send({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "toml",
                "version": 1,
                "text": text,
            }
        },
    })
    send({
        "jsonrpc": "2.0",
        "id": request,
        "method": "textDocument/hover",
        "params": {
            "textDocument": {"uri": uri},
            "position": {"line": hover_at[0], "character": hover_at[1]},
        },
    })
    hover = receive(request)["result"]
    request += 1
    send({
        "jsonrpc": "2.0",
        "id": request,
        "method": "textDocument/completion",
        "params": {
            "textDocument": {"uri": uri},
            "position": {"line": complete_at[0], "character": complete_at[1]},
        },
    })
    completion = receive(request)["result"]
    request += 1
    items = (
        completion
        if isinstance(completion, list)
        else (completion or {}).get("items", [])
    )
    header = text.split("\n")[0]
    print(
        f"--- {header!r}: hover={'yes' if hover else 'null'} completion={[i['label'] for i in items][:4]}"
    )
send({"jsonrpc": "2.0", "id": request, "method": "shutdown"})
receive(request)
send({"jsonrpc": "2.0", "method": "exit"})
proc.wait(timeout=10)
