# smelt Backlog（杂项）

产品主航道见 [`product-roadmap.md`](product-roadmap.md)。  
本文只收主航道外的待做点子。

---

## 智能体记忆（暂停）

第一阶段、来源防护、开关、召回已完成；剩余待办（跨进程并发、记忆卡片管理、手机端、召回持久化与
标定、后台提炼等）见 [`agent-memory.md`](agent-memory.md) 第 13 节。

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
