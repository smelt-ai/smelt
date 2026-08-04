#!/usr/bin/env python3
"""LLM triage：给 issue 自动分类（bug/feature/question）并定严重度（P0/P1/P2）。

多提供商支持（OpenAI 兼容 API，用 Python 标准库 urllib 调用，零第三方依赖）：

  LLM_PROVIDER=github_models（默认）：GitHub Models，用 Actions 自动注入的
      GITHUB_TOKEN，零新增配置，runner 直连秒回；模型 openai/gpt-4o-mini
  LLM_PROVIDER=deepseek：DeepSeek，需 DEEPSEEK_API_KEY；模型 deepseek-chat

任何失败（缺 token、网络错误、超时、返回格式不对）都静默降级为 LLM_OK=no，
绝不阻塞后续审核与飞书推送——LLM 只是加分项。

输入（环境变量）：
  LLM_PROVIDER     github_models（默认）/ deepseek
  GITHUB_TOKEN     github_models 的认证 token（Actions 自动注入）
  DEEPSEEK_API_KEY deepseek 的认证 key
  LLM_MODEL        可选，覆盖默认模型名
  LLM_API_URL      可选，覆盖 API 端点（本地 mock 测试用）
  GITHUB_EVENT_TITLE / GITHUB_EVENT_BODY

输出（写入 GITHUB_ENV）：
  LLM_OK        yes / no
  LLM_TYPE      bug / feature / question
  LLM_SEVERITY  P0 / P1 / P2
  LLM_SUMMARY   一句话摘要（GITHUB_ENV 特殊字符已转义）
"""

import json
import os
import signal
import urllib.request

# 各提供商默认端点与模型；LLM_API_URL / LLM_MODEL 可覆盖（本地 mock 测试用）
PROVIDERS = {
    "github_models": {
        "api_url": "https://models.github.ai/inference/chat/completions",
        "model": "openai/gpt-4o-mini",
        "token_env": "GITHUB_TOKEN",
    },
    "deepseek": {
        "api_url": "https://api.deepseek.com/chat/completions",
        "model": "deepseek-chat",
        "token_env": "DEEPSEEK_API_KEY",
    },
}

# socket 超时（秒）。注意 urllib 的 timeout 覆盖不到 DNS 解析/连接建立阶段，
# 所以下面还有 signal.alarm 硬超时兜底，避免 API 不可达时拖慢整个 workflow。
API_TIMEOUT = 15

SYSTEM_PROMPT = (
    "你是开源项目的 issue 管理员。根据 issue 的标题和正文，只输出 JSON，"
    "不要任何其他内容："
    '{"type": "bug|feature|question", "severity": "P0|P1|P2", "summary": "一句话总结"}'
)

VALID_TYPES = ("bug", "feature", "question")
VALID_SEVERITIES = ("P0", "P1", "P2")


def _timeout_handler(signum, frame):
    """signal.alarm 触发：DNS/连接阶段挂起时的最后防线。"""
    raise TimeoutError(f"LLM API 调用超过 {API_TIMEOUT}s 硬超时")


def env(key, default=""):
    return os.environ.get(key, default)


def write_env(key, value):
    with open(env("GITHUB_ENV"), "a") as f:
        f.write(f"{key}={value}\n")


def main():
    provider = env("LLM_PROVIDER", "github_models")
    conf = PROVIDERS.get(provider)
    if not conf:
        print(f"[LLM] 未知 LLM_PROVIDER={provider}，跳过分类")
        write_env("LLM_OK", "no")
        return

    key = env(conf["token_env"])
    if not key:
        print(f"[LLM] {provider} 缺少 {conf['token_env']}，跳过分类")
        write_env("LLM_OK", "no")
        return

    api_url = env("LLM_API_URL", conf["api_url"])
    model = env("LLM_MODEL", conf["model"])
    title = env("GITHUB_EVENT_TITLE").strip()
    body = env("GITHUB_EVENT_BODY").strip()

    payload = {
        "model": model,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": f"标题：{title}\n正文：{body}"},
        ],
        "temperature": 0,
    }
    req = urllib.request.Request(
        api_url,
        data=json.dumps(payload).encode(),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {key}",
        },
    )

    def _call():
        with urllib.request.urlopen(req, timeout=API_TIMEOUT) as resp:
            data = json.load(resp)
        content = data["choices"][0]["message"]["content"]
        return json.loads(content)

    # signal.alarm 只在主线程 + POSIX 可用（ubuntu runner / macOS 本地都满足）。
    # 用它给整个 API 调用加硬超时，DNS 解析或 TCP 连接挂起时也能及时降级。
    try:
        if hasattr(signal, "SIGALRM"):
            signal.signal(signal.SIGALRM, _timeout_handler)
            signal.alarm(API_TIMEOUT)
        try:
            result = _call()
        finally:
            if hasattr(signal, "SIGALRM"):
                signal.alarm(0)  # 正常返回也要取消闹钟
    except Exception as e:
        print(f"[LLM] {provider} 调用失败，降级跳过: {e}")
        write_env("LLM_OK", "no")
        return

    llm_type = str(result.get("type", "")).lower()
    severity = str(result.get("severity", "")).upper()
    if llm_type not in VALID_TYPES:
        llm_type = "question"
    if severity not in VALID_SEVERITIES:
        severity = "P2"
    summary = str(result.get("summary", "")).strip()

    write_env("LLM_OK", "yes")
    write_env("LLM_TYPE", llm_type)
    write_env("LLM_SEVERITY", severity)
    # GITHUB_ENV 写入规则：% → %25，换行 → %0A（多行值经 GitHub 解码后还原）
    write_env("LLM_SUMMARY", summary.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A"))
    print(f"[LLM] {provider}/{model} → {llm_type} · {severity} · {summary}")


if __name__ == "__main__":
    main()
