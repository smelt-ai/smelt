"""对话基准用的假 ACP agent（stdio JSON-RPC），不调任何模型。

prompt 里写 `stream <片段数> <每片字节> <间隔毫秒> <标签>`，就按这个节奏流式吐出助手
回复，最后一片是 `__END_<标签>__`，然后结束回合。标签用来区分各个回合。其余请求一律回空结果。
"""

import json
import re
import sys
import time
import uuid


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def chunk(session_id, text):
    send(
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}},
            },
        }
    )


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    if "id" not in message or "method" not in message:
        continue
    request_id = message["id"]
    method = message["method"]
    params = message.get("params") or {}
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {}}
    elif method == "session/new":
        # 每个进程一个会话 id：Smelt 不许两个会话共用同一个 provider 会话。
        result = {"sessionId": f"bench-{uuid.uuid4().hex}"}
    elif method == "session/prompt":
        text = " ".join(part.get("text", "") for part in params.get("prompt", []))
        found = re.search(r"stream (\d+) (\d+) (\d+) (\S+)", text)
        count, size, delay, tag = found.groups() if found else ("1", "16", "0", "none")
        count, size, delay = int(count), int(size), int(delay)
        for index in range(count):
            body = f"[{tag} {index:06d}] " + "x" * max(size - 10, 0) + "\n"
            chunk(params["sessionId"], body)
            if delay:
                time.sleep(delay / 1000)
        chunk(params["sessionId"], f"__END_{tag}__\n")
        result = {"stopReason": "end_turn"}
    else:
        result = {}
    send({"jsonrpc": "2.0", "id": request_id, "result": result})
