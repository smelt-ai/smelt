# smelt Backlog（杂项）

产品主航道见 [`product-roadmap.md`](product-roadmap.md)。  
本文只收主航道外的待做点子。

---

## 系统感知

- **输入模拟**（enigo / tfc）：替用户点按、宏、把文本敲进当前输入框；须用户显式触发，防抢焦点
- **屏幕捕获**（xcap / scrap）：会话缩略图优先；需屏幕录制权限，节流
- **辅助功能上下文**（暂缓）：读 AX 树拿浏览器 URL / 文档标题——隐私重、收益未证

---

## smeltd / 会话

- 长 detach + Claude Code Ctrl+C 实机验证
- 守护崩溃后的落盘恢复
- 会话 JSONL 落盘 + SQLite 索引（列表分页 / 全文搜索）
- smeltd JSON-RPC 结构化控制通道（状态从猜字节 → 协议事实）
- Claude Code hook → `smelt-notify` 直写 smeltd socket（OSC 第二信源）
- 会话卡片：运行时长、token / 上下文余量

---

## 会话监控

- fs watcher 驱动（不轮询）解析 `~/.claude/projects/*.jsonl`
- 五态：`Thinking / Executing Tool / Awaiting Approval / Waiting for User / Idle`

> worktree · Remix · 交互式 diff 已归入 [`product-roadmap.md`](product-roadmap.md)

---

## 宠物

- 近距凑近 / 划过身体害羞挤压
- Stage 3：多轮对话（输入框 + 历史）

---

## 终端渲染（边际）

- 跨行攒批 `shape_line`（对齐 Zed `BatchedTextRun`）
- 纯事件驱动：bell/title/OSC 通知改事件通道后再删 30ms 兜底轮询
- 光标闪烁（`BlinkManager`）
- 可选：Vi 模式、前景 minimum-contrast（连字与 `force_width` 冲突，不做）

**不抄：** AgentHub 关卡片杀 shell（与 smeltd 保活哲学相反）· Seatbelt 沙箱（可选后置）
