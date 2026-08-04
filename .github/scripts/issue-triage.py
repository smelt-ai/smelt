#!/usr/bin/env python3
"""issue 自动审核：模板完整性 + 标题查重。

规则级审核，零第三方依赖，只依赖 Python 3 标准库，直接在 GitHub Actions
runner 上跑。LLM triage（分类/严重度）后续可在 workflow 里加一步扩展。

输入（环境变量）：
  GITHUB_EVENT_TITLE   issue 标题
  GITHUB_EVENT_BODY    issue 正文（可能为空字符串）
  GITHUB_EVENT_NUMBER  issue 编号（查重时排除自身）
  GITHUB_REPOSITORY    owner/repo
  GITHUB_TOKEN         GitHub token（查重用；缺失时自动跳过查重）

输出（写入 GITHUB_ENV，供后续 step 使用）：
  AUDIT_PASS      yes / no
  AUDIT_PROBLEMS  问题列表，以"；"连接；无问题为 ok
  AUDIT_DUP       查重提示；无重复为 ok
"""

import json
import os
import re
import urllib.parse
import urllib.request

TITLE_MIN = 10      # 标题最短长度（字符）
BODY_MIN = 30       # 正文最短长度（字符）

# 查重时从标题里提取关键词时排除的停用词
STOPWORDS = {
    "issue", "this", "that", "with", "from", "your", "the", "and", "for",
    "about", "when", "what", "help", "please", "can", "does",
}

# 判定为 bug 类 issue 的关键词；命中后强制要求复现步骤
BUG_KEYWORDS = re.compile(r"\b(bug|crash|panic)\b|崩溃|闪退|报错|死锁", re.I)
REPRO_STEPS = re.compile(r"复现|重现|repro|steps|如何触发", re.I)


def env(key, default=""):
    return os.environ.get(key, default)


def write_env(key, value):
    with open(env("GITHUB_ENV"), "a") as f:
        f.write(f"{key}={value}\n")


def check_duplicate(title, repo, number):
    """按标题关键词搜 open issues，排除自身；任何失败都静默跳过查重。"""
    token = env("GITHUB_TOKEN")
    if not token:
        print("[查重] 无 GITHUB_TOKEN，跳过")
        return "ok"

    words = [w for w in re.findall(r"[A-Za-z0-9]{4,}", title.lower())
             if w not in STOPWORDS]
    if not words:
        return "ok"

    # 取前 3 个词做 OR 匹配，太严的 AND 容易漏掉真实重复
    terms = " OR ".join(f'"{w}"' for w in words[:3])
    query = f"repo:{repo} is:issue is:open in:title ({terms})"
    url = "https://api.github.com/search/issues?" + urllib.parse.urlencode(
        {"q": query, "per_page": "3"}
    )
    req = urllib.request.Request(url, headers={
        "Accept": "application/vnd.github+json",
        "Authorization": f"Bearer {token}",
        "User-Agent": "smelt-issue-bot",
    })
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            data = json.load(resp)
    except Exception as e:
        print(f"[查重] 跳过（API 调用失败: {e}）")
        return "ok"

    hits = [it for it in data.get("items", []) if str(it["number"]) != number][:2]
    if not hits:
        return "ok"
    return "可能重复：" + "、".join(f"#{it['number']}" for it in hits)


def main():
    title = env("GITHUB_EVENT_TITLE").strip()
    body = env("GITHUB_EVENT_BODY").strip()
    repo = env("GITHUB_REPOSITORY")
    number = env("GITHUB_EVENT_NUMBER", "0")

    problems = []
    if len(title) < TITLE_MIN:
        problems.append(f"标题太短（{len(title)} 字，至少 {TITLE_MIN} 字）")
    if len(body) < BODY_MIN:
        problems.append(f"描述太少（{len(body)} 字，至少 {BODY_MIN} 字）")
    if BUG_KEYWORDS.search(title + " " + body) and not REPRO_STEPS.search(body):
        problems.append("疑似 bug，缺少复现步骤")

    dup = check_duplicate(title, repo, number)

    write_env("AUDIT_PASS", "yes" if not problems else "no")
    write_env("AUDIT_PROBLEMS", "；".join(problems) if problems else "ok")
    write_env("AUDIT_DUP", dup)
    print(f"审核结果: {'PASS' if not problems else 'FAIL'} {problems or '-'} | 查重: {dup}")


if __name__ == "__main__":
    main()
