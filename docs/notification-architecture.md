# 状态与通知架构

## 目标

状态描述 agent 当前事实，关注事件描述一次需要告知用户的变化，投递渠道只决定在哪里
展示。三者不得互相代替：

```text
各 provider hook / ACP / OSC / BEL
          ↓
AgentEvent v1 → smeltd reducer → DaemonPhase / fallback event
          ↓
AgentStatus + AttentionItem
          ↓
铃铛 / toast / 系统通知 / Dock / 菜单栏
```

## 领域模型

- `DaemonPhase`：守护状态通道的细粒度事实。
- `AgentStatus`：UI 五态，由 `AgentStatus::from_daemon_phase` 统一压缩。
- `AttentionItem`：一次性关注事件，包含会话、标题、正文和 `AttentionKind`。
- `AttentionKind`：审批、输入、成功、失败、响铃、普通提示。
- `AttentionStore`：按 session id 保存当前事件、已读状态、行动项解决状态、投递队列
  和 60 秒去重指纹。

`AgentStatus` 不携带未读；`AttentionItem` 不充当持续状态。用户读过一条完成消息后，
会话仍可处于 `Succeeded`，但不应继续显示未读。

## 当前实现

Claude、Codex、Copilot 的 hook 先由各自适配规则归一化为版本化 `AgentEvent`，事件包含
生命周期语义以及可用的 tool/agent 身份。smeltd reducer 统一维护 phase，审批和输入等待
保持粘性，只有对应工具完成、新 prompt 或明确回合终态才能解除，避免并行子任务消息覆盖。
旧 `state` op 仍保留；新 helper 遇到旧 daemon 会自动回退。

`AgentEvent v1` 覆盖会话开始/结束、用户提交、工具开始/成功/失败、审批、用户输入、
子任务开始/结束、回合成功/失败。provider 工具名只在适配层用于把原始 hook 判定为这些
稳定语义之一，不进入 UI 的状态判断逻辑。

结构化 hook/ACP 通过 `apply_daemon_transition` 写入 store；OSC/BEL fallback 在
`TerminalView` 中只负责翻译成 `AttentionItem`，不再自行保存未读或投递系统通知。
结构化事件一旦启用，fallback 永久让位，避免双通知。

所有出口读取同一个 store：

- 正在查看对应 pane：标记已读，不弹通知。
- 应用前台但未查看：应用内 toast。
- 应用后台：macOS 系统通知。
- 铃铛展示未读；Dock 和菜单栏角标统计尚未解决的行动项。
- 用户看过审批/输入只会标记已读；daemon 离开等待 phase 后才真正解决行动项。
- 用户关闭会话或 pane 时清理对应 store 条目和待投递事件。
- 通知设置只控制 toast/系统通知渠道，不会抹掉状态事实或未读记录。

`AgentStatus` 的 session/pane 映射统一由 `AgentStatus::from_daemon_phase` 处理；
`Succeeded` 只有在 store 仍有未读成功事件时才映射成 UI 的 `Done`。

## 扩展约束

手机推送、深链和远程审批必须订阅同一 store 或同源的 `AttentionItem` 流，不得重新解释
daemon phase。新增事件类型时必须补齐状态转换、去重、已读/解决和投递渠道测试。
