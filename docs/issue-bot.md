# GitHub Issue 机器人（自动审核 + 飞书推送）

新 issue 创建或重新打开时，机器人自动做**规则审核**，审核结果**推送到飞书群**，
不合格的 issue 自动评论提示并打 `needs-triage` 标签。

## 工作流程

```
issue opened/reopened
        │
        ▼
① 规则审核（标题长度 / 描述长度 / bug 须带复现步骤 / 标题查重）
        │
        ▼
② DeepSeek 分类（bug/feature/question + P0/P1/P2，失败自动降级跳过）
        │
        ├─ ①通过 ─► 清理 needs-triage 标签 ─┐
        └─ ①不通过 ─► 评论列问题 + 打标签 ──┤
        └─ ②成功 ─► 打分类标签 ─────────────┤
                                             ▼
                                  推 interactive 卡片到飞书群
```

实现文件：

- `.github/workflows/issue-bot.yml` — workflow 定义
- `.github/scripts/issue-triage.py` — 规则审核（Python 3 标准库，可本地直接跑）
- `.github/scripts/issue-llm-triage.py` — DeepSeek 分类（标准库 urllib 调用，无第三方依赖）

## 配置步骤

### 1. 飞书群加机器人

1. 打开目标飞书群 → **设置** → **群机器人** → **添加机器人** → **自定义机器人**
2. 机器人名称随意（如 `issue-bot`），复制 webhook 地址
   （形如 `https://open.feishu.cn/open-apis/bot/v2/hook/xxxx`）
3. 安全设置建议勾选**签名校验**，把密钥一并记下；权限勾选"发消息"

### 2. 在 GitHub 仓库存 secret

仓库 **Settings → Secrets and variables → Actions → New repository secret**：

| Secret | 必填 | 值 |
|---|---|---|
| `FEISHU_ISSUE_WEBHOOK` | ✅ | 上面复制的 webhook 地址 |
| `FEISHU_WEBHOOK_SECRET` | 可选 | 签名校验密钥（勾选了签名才需要） |

> LLM 分类**零新增配置**：默认走 GitHub Models，直接用 Actions 自动注入的
> `GITHUB_TOKEN`（workflow 已声明 `models: read` 权限）。只有想切回 DeepSeek
> 时才需要 `DEEPSEEK_API_KEY`（见下文"切换提供商"）。

### 3. 验证

新建一个测试 issue 提交即可。预期：

- 飞书群收到一条蓝色卡片（含标题、作者、审核结论、LLM 分类、"查看详情"链接）
- 内容不完整的 issue 会收到机器人评论 + `needs-triage` 标签
- 分类成功时，issue 自动带上 `bug`/`feature`/`question` 和
  `severity:P0` 等分类标签；调用失败则静默跳过，不影响其他功能

> 注意：workflow 只在**默认分支（main）**上生效，改动后要先合入 main。

## 审核规则

| 规则 | 判定 |
|---|---|
| 标题长度 | 少于 10 字判不合格 |
| 描述长度 | 少于 30 字判不合格 |
| 复现步骤 | 标题/描述命中 bug、崩溃、报错等关键词时，正文必须包含复现/重现/steps 等 |
| 标题查重 | 提取标题关键词搜 open issues（排除自身），命中则提示"可能重复：#N" |

查重依赖 GitHub Search API，失败时静默跳过，不阻塞审核。

## LLM 分类（GitHub Models，已启用）

workflow 的"规则审核"之后会自动调用 `.github/scripts/issue-llm-triage.py`，
给 issue 分类（bug/feature/question）并定严重度（P0/P1/P2），附带一句话摘要：

- 分类结果写入评论和飞书卡片，并自动打 `bug` / `feature` / `question` /
  `severity:P0` 等标签
- **零配置**：默认走 GitHub Models（`openai/gpt-4o-mini`），用 Actions 自动注入的
  `GITHUB_TOKEN`，runner 直连秒回，有免费额度（限速，issue 分类这种低频场景够用）
- **容错**：缺 token、网络错误、超时（15s 硬超时）、返回格式异常，一律降级跳过，
  不影响审核与推送

### 切换提供商

脚本支持多提供商（OpenAI 兼容 API），通过 `LLM_PROVIDER` 环境变量切换：

| 提供商 | `LLM_PROVIDER` | 认证 | 默认模型 |
|---|---|---|---|
| GitHub Models（默认） | `github_models` | `GITHUB_TOKEN` | `openai/gpt-4o-mini` |
| DeepSeek | `deepseek` | `DEEPSEEK_API_KEY` | `deepseek-chat` |

切到 DeepSeek：workflow 的 LLM 步骤把 `GITHUB_TOKEN` 换成
`DEEPSEEK_API_KEY: ${{ secrets.DEEPSEEK_API_KEY }}` 并设 `LLM_PROVIDER: deepseek`，
再在仓库加 `DEEPSEEK_API_KEY` secret。模型名可用 `LLM_MODEL` 覆盖。

## 本地调试审核脚本

不依赖 GitHub 环境，可直接在本地跑：

```sh
GITHUB_ENV=/tmp/env.txt \
GITHUB_EVENT_TITLE="App 启动后三秒内崩溃" \
GITHUB_EVENT_BODY="打开 App 后立即闪退，无任何报错提示" \
GITHUB_REPOSITORY=smelt-ai/smelt \
GITHUB_EVENT_NUMBER=1 \
python3 .github/scripts/issue-triage.py
cat /tmp/env.txt   # 查看 AUDIT_PASS / AUDIT_PROBLEMS / AUDIT_DUP
```

LLM 分类脚本同理（GitHub Models 需 `export GITHUB_TOKEN=...`，DeepSeek 需
`export DEEPSEEK_API_KEY=...`）：

```sh
export GITHUB_TOKEN=ghp_xxx   # 或 DEEPSEEK_API_KEY=sk-xxx
GITHUB_ENV=/tmp/env-llm.txt \
GITHUB_EVENT_TITLE="App 启动后三秒内崩溃" \
GITHUB_EVENT_BODY="打开 App 后立即闪退。复现步骤：1. 启动 2. 等待三秒 3. 崩溃" \
python3 .github/scripts/issue-llm-triage.py
cat /tmp/env-llm.txt   # 查看 LLM_OK / LLM_TYPE / LLM_SEVERITY / LLM_SUMMARY
```

本地 mock 测试（不起真实 API）：用 `LLM_API_URL` 指向本地 server，`LLM_PROVIDER`
按需指定，`LLM_MODEL` 任意。
