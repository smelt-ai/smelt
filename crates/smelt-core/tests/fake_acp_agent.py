#!/usr/bin/env python3
"""最小的 ACP agent（stdio JSON-RPC），给 `acp_conn` 的 session/load 端到端用例用。

只做三件事：initialize 宣告 loadSession；session/load 先推两条历史
session/update 再回响应（ACP 规范要求重放先于响应）；其余请求一律回空结果。
"""

import json
import sys


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


REPLAY = [
    {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "早先的问题"}},
    {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "早先的回答"}},
]

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    if "id" not in message or "method" not in message:
        continue
    request_id = message["id"]
    method = message["method"]
    if method == "initialize":
        send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}},
            }
        )
    elif method == "session/load":
        session_id = message["params"]["sessionId"]
        for update in REPLAY:
            send(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {"sessionId": session_id, "update": update},
                }
            )
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    else:
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
