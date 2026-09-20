#!/usr/bin/env python3
"""最小的 OpenAI 兼容流式端点，给 `dsh_live_tests` 当模型用。

存在的理由：dsh 桥的呈现映射（流式正文、reasoning、usage、回合收尾顺序）
过去只有单测，而单测测的是我们自己的假设。指着这个端点就能跑一轮**真实**
成功回合，不需要任何人的 API key，也不花钱。

    python3 crates/smelt-core/tests/fake_llm.py 8177 &
    SMELT_TEST_LLM_BASE_URL=http://127.0.0.1:8177/v1 \
      cargo test -p smelt-core --lib dsh_live -- --ignored --test-threads=1

注意分多段 delta 返回：一次性吐完整段就测不出「正文必须排在回合结束之前」。

剧本用 `FAKE_LLM_SCRIPT` 选，默认 `text`。新增场景加一张表项即可，不改分发逻辑。
`read` 剧本让模型去读一个**相对路径**，于是这个端点同时是「文件工具的根到底
在哪」的判别器：相对路径由 `fs-sandbox` 的 `cwd` 决定，读到了就说明根是对的。
"""

import http.server
import json
import os
import sys

TEXT_CHUNKS = [
    {"choices": [{"index": 0, "delta": {"reasoning_content": "先想一下"}}]},
    {"choices": [{"index": 0, "delta": {"content": "你"}}]},
    {"choices": [{"index": 0, "delta": {"content": "好"}}]},
    {"choices": [{"index": 0, "delta": {"content": "，世界"}}]},
    {
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 4, "total_tokens": 15},
    },
]

# 工具调用同样分段吐 arguments，这才和真实适配器的 tool-call-delta 一致。
READ_CHUNKS = [
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_probe",
                            "type": "function",
                            "function": {"name": "read", "arguments": ""},
                        }
                    ]
                },
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "function": {"arguments": '{"file_path":'},
                        }
                    ]
                },
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "function": {"arguments": '"./probe.txt"}'},
                        }
                    ]
                },
            }
        ]
    },
    {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
]

WRITE_CHUNKS = [
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_write",
                            "type": "function",
                            "function": {
                                "name": "write",
                                "arguments": ""
                            }
                        }
                    ]
                }
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "function": {
                                "arguments": "{\"file_path\":\"./written-by-agent.txt\","
                            }
                        }
                    ]
                }
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "function": {
                                "arguments": "\"content\":\"hi\"}"
                            }
                        }
                    ]
                }
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }
        ]
    }
]

OUTSIDE_CHUNKS = [
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_out",
                            "type": "function",
                            "function": {
                                "name": "write",
                                "arguments": ""
                            }
                        }
                    ]
                }
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "function": {
                                "arguments": "{\"file_path\":\"/tmp/dsh-cmp/outside.txt\","
                            }
                        }
                    ]
                }
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "function": {
                                "arguments": "\"content\":\"escaped\"}"
                            }
                        }
                    ]
                }
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }
        ]
    }
]


BASH_CHUNKS = [
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_bash",
                            "type": "function",
                            "function": {
                                "name": "bash",
                                "arguments": ""
                            }
                        }
                    ]
                }
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "function": {
                                "arguments": "{\"command\":"
                            }
                        }
                    ]
                }
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "function": {
                                "arguments": "\"pwd\",\"description\":\"print working directory\"}"
                            }
                        }
                    ]
                }
            }
        ]
    },
    {
        "choices": [
            {
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }
        ]
    }
]


SCRIPTS = {
    "text": [TEXT_CHUNKS],
    # 第一轮发工具调用，工具结果回来后模型再收尾——所以是两段应答。
    "read": [READ_CHUNKS, TEXT_CHUNKS],
    # 写入受 `sandbox-policy.workspaceRoot` 管辖，是「沙箱根在哪」的判别器。
    "write": [WRITE_CHUNKS, TEXT_CHUNKS],
    # 越界写：验证沙箱边界到底以谁为准（session cwd 还是 profile 里的 root）。
    "outside": [OUTSIDE_CHUNKS, TEXT_CHUNKS],
    # terminal 卡片 + 「bash 的工作目录到底在哪」。
    "bash": [BASH_CHUNKS, TEXT_CHUNKS],
}


def script_turns():
    name = os.environ.get("FAKE_LLM_SCRIPT", "text")
    turns = SCRIPTS.get(name)
    if turns is None:
        raise SystemExit(f"unknown FAKE_LLM_SCRIPT={name!r}; have {sorted(SCRIPTS)}")
    return turns


class Handler(http.server.BaseHTTPRequestHandler):
    turns = []
    seen = 0

    def log_message(self, *args):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        # 最后一段应答重复使用，免得模型多问一轮就把端点耗干。
        index = min(Handler.seen, len(Handler.turns) - 1)
        Handler.seen += 1
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for chunk in Handler.turns[index]:
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
            self.wfile.flush()
        # 适配器要求以 [DONE] 收尾，缺了会报 STREAM_CLOSED。
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8177
    Handler.turns = script_turns()
    http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()