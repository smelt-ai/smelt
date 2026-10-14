# smelt Backlog（杂项）

产品主航道见 [`product-roadmap.md`](product-roadmap.md)。  
本文只收主航道外的待做点子。

---

## 智能体记忆（暂停）

第一阶段、来源防护、开关、召回已完成；剩余待办（跨进程并发、记忆卡片管理、手机端、召回持久化与
标定、后台提炼等）见 [`agent-memory.md`](agent-memory.md) 第 13 节。

---

## 智能体人设保护（待定）

现状：产品对话和自动化都跑完全访问（`bypassPermissions`），所有工具调用自动批准；
`smelt agent update --prompt` 直接写 SQLite，智能体能经 bash 悄悄改掉自己的工作方式。
调研结论是不新增 SOUL.md 一类人设文件，改为保护已有的指令入口：

- **受保护操作完全访问下也要确认，无人值守直接拒**：参考 Claude Code 完全访问下写受保护目录仍弹确认、
  Codex 可写沙箱里 `.git` / `.codex` / `.agents` 保持只读。
  - Pi 侧 `packages/pi-agent/src/smelt-permission.ts` 给命中的调用打「受保护」标记；
  - Rust 侧 `crates/smelt-core/src/pi_rpc.rs` 的 `handle_permission_request` 对标记调用在完全访问下也弹确认；
  - 自动化发起的运行要能被识别，碰到受保护操作直接拒绝，原因回给智能体。
  - 未定：保护范围。候选从窄到宽：只保护 Smelt 自身配置（`smelt agent` 增删改、`smelt automation`
    写操作、`~/.smelt` 下配置）→ 加任意目录的 `AGENTS.md` / `CLAUDE.md` → 加全局指令与技能
    （`~/.agents`、`~/.claude`、`~/.pi`）。范围过大会频繁打断，用户养成无脑批准，等于没保护。
  - 定位是防误改与提示注入，不防刻意绕过：bash 命令识别靠字符串，智能体直接改 SQLite 拦不住。
- **智能体提议调整工作方式**：由智能体提出修改建议，用户确认后才写入，排在上一条之后。

---

## 系统感知

- **输入模拟**（enigo / tfc）：替用户点按、宏、把文本敲进当前输入框；须用户显式触发，防抢焦点
- **屏幕捕获**（xcap / scrap）：会话缩略图优先；需屏幕录制权限，节流
- **辅助功能上下文**（暂缓）：读 AX 树拿浏览器 URL / 文档标题——隐私重、收益未证

---

## smeltd / 会话

- 长 detach + Claude Code Ctrl+C 实机验证
- 守护崩溃后的落盘恢复
- 会话列表分页 / 全文搜索（会话本身已在 SQLite，不再另起一套 JSONL 索引）
- 会话卡片：运行时长、token / 上下文余量

---

## 会话监控

- ✅ 会话状态不靠轮询 `~/.claude/projects/*.jsonl`。agent hook → `smelt-notify`，以及 ACP / provider 原生协议，已经是信源；OSC 不是信源。
- ✅ 守护相位 `DaemonPhase` 已落地（connecting / thinking / executing_tool / awaiting_approval / waiting_for_user / succeeded / failed / idle / dead）。这是守护事实，不是侧栏 UI 态。ACP 对话页读同一套相位，不再另养 `AcpPhase`。

> worktree · Remix · 交互式 diff 已归入 [`product-roadmap.md`](product-roadmap.md)

---

## 终端渲染（边际）

- 跨行攒批 `shape_line`（对齐 Zed `BatchedTextRun`）
- 纯事件驱动：bell/title/OSC 通知改事件通道后再删 30ms 兜底轮询
- 光标闪烁（`BlinkManager`）
- 可选：Vi 模式、前景 minimum-contrast（连字与 `force_width` 冲突，不做）

**不抄：** AgentHub 关卡片杀 shell（与 smeltd 保活哲学相反）· Seatbelt 沙箱（可选后置）
