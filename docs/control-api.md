# smeltd Control API v1

状态：已实现。契约真源是 `smelt-core/src/control_api.rs`，服务端分发在
`smeltd/src/control.rs`。

Control API 是 smeltd 的**版本化短请求协议**，不是 HTTP 服务，也不取代 ACP。
它复用 `~/.smelt/smeltd.sock` 的首帧分流，供桌面、Mobile、MCP、CLI 和未来的进程外
插件共享同一份控制契约。

## 边界

smeltd 现有连接有三种不同生命周期，不能为了表面统一塞进同一种 RPC：

| 通道 | 连接生命周期 | 例子 |
|---|---|---|
| Control API | 一次请求、一次响应 | 能力发现、列表、命令 |
| 状态订阅 | 长连接 JSON 事件流 | `event_subscribe` |
| 会话数据流 | 长连接、可能双向、含二进制帧 | `open`、`watch`、ACP stream |

短请求统一使用 `op=control`；`open`、`watch` 和 ACP stream 等长连接继续使用各自的
流式协议，不通过 Control API 转发。

正式状态客户端使用 `op=event_subscribe`、版本化 `SubscribeRequest` 和
`EventClientAuth`。旧 `op=subscribe` 已移除，避免未认证的 legacy 长连接绕过正式
订阅鉴权。

## 请求与响应

请求：

```json
{
  "op": "control",
  "control_version": 1,
  "request_id": "request-abc",
  "method": "system.describe",
  "params": {}
}
```

成功响应：

```json
{
  "control_version": 1,
  "request_id": "request-abc",
  "status": "ok",
  "result": {}
}
```

失败响应：

```json
{
  "control_version": 1,
  "request_id": "request-abc",
  "status": "error",
  "error": {
    "code": "invalid_params",
    "message": "..."
  }
}
```

服务端必须原样回显合法 `request_id`；客户端必须同时校验版本和 ID，不能把并发请求的
响应串线。请求和响应对象会忽略未知字段，允许同一版本增加不影响旧语义的可选字段；
改变既有字段含义、必填性或副作用语义时必须提升协议版本或增加新 method。

`unsupported_version` 可以由服务端当前版本的信封返回，客户端应先识别这类错误再做
响应版本等值校验。它额外携带机器可读的版本范围：

```json
{
  "code": "unsupported_version",
  "message": "...",
  "supported_versions": {
    "min_supported_version": 1,
    "max_supported_version": 1
  }
}
```

## v1 方法

| method | 性质 | 说明 |
|---|---|---|
| `system.describe` | 只读 | 返回版本范围、方法列表和 daemon 运行信息 |
| `session.list` | 只读 | 返回 smeltd 已发布的终端与 ACP 会话 ID |
| `agent.list` | 只读、需能力令牌 | 返回当前 agent 可见的跨 Agent 目标 |
| `agent.send` | 有副作用、需能力令牌 | 向目标 Agent 投递一次消息 |
| `agent.definition.list` | 只读 | 返回产品智能体定义清单，不是运行中会话 |
| `agent.definition.get` | 只读 | 按 id 读取一条定义 |
| `agent.definition.create` | 有副作用 | 新建定义。可带稳定 `id`；缺省生成 UUID。重复 id 返回 `conflict` |
| `agent.definition.update` | 有副作用 | 按 id 补丁更新；禁止整表覆盖 |
| `agent.definition.delete` | 有副作用 | 按 id 删除。仍被自动化引用时返回 `failed_precondition` |
| `settings.get` | 只读 | 返回已登记的外观设置 |
| `settings.update` | 有副作用 | 按 key patch；未知 key 返回 `invalid_params` |
| `automation.list` | 只读 | 定义清单；webhook 令牌脱敏 |
| `automation.get` | 只读 | 按 id 读取 |
| `automation.upsert` | 有副作用 | 新建或覆盖一条自动化，daemon 分配工作区 |
| `automation.delete` | 有副作用 | 按 id 删除 |
| `automation.set_enabled` | 有副作用 | 幂等设值，不是 toggle |
| `automation.run_once` | 有副作用 | 立即跑一次，不推进定时游标 |

CLI：`smelt agent` / `smelt automation` / `smelt settings`。教模型用的 skill 安装到 `~/.agents/skills/smelt/SKILL.md`。

`agent.list/send` 的 `source_session_id` 与 `source_token` 放在类型化 params 内；领域层仍会
在提交前重新校验当前 runtime 代际和能力令牌，Control API 不绕过原有授权边界。
`agent.send` 还要求调用方提供稳定 `command_id`。smeltd 以
`source_session_id + command_id` 查找已完成结果。同一 ID 会绑定到
`source_session_id + 请求的 target 原文 + message` 的 SHA-256 指纹；改变目标别名原文
或消息后复用 ID 会返回 `conflict`，且首次查重发生在目标解析之前。

`smelt_send_message` MCP tool 的 `command_id` 是可选输入。省略时 MCP server 在任何参数
校验或传输前生成一次；显式传入时原样复用。成功、领域错误和 `result_unknown` 的
`structuredContent` 都返回该 ID，调用方可用它查询不确定结果。内部传输重试不会换 ID。

## 版本与重试

1. 服务端接受 `min_supported_version..=max_supported_version` 内的请求，并使用请求版本
   回包；客户端可根据 `unsupported_version.supported_versions` 选择双方共有版本。留在
   同一支持范围内的版本必须保持 method payload 可加性兼容；破坏性变更应新增 method，
   或在提升版本的同时提升 `min_supported_version`，不能假装继续兼容旧 DTO。
2. `agent.send` 在发送前生成一次 `command_id`。响应丢失后允许用相同 ID 查询/重试；
   不得换一个新 ID 自动重放。
3. 调用方必须根据稳定错误码分支，不能解析自然语言文案：参数、授权、目标缺失、前置
   条件、冲突、忙碌和临时不可用分别返回 `invalid_params`、`permission_denied`、
   `not_found`、`failed_precondition`、`conflict`、`busy`、
   `temporarily_unavailable`。外部 PTY/provider 副作用已发生、但 SQLite 的命令结果与
   outbox 事实尚未提交时返回非自动重试的 `result_unknown`；无法进一步分类的投递失败
   才使用 `operation_failed`。

## 扩展规则

- 新增核心短命令：在共享契约的静态 method 声明中增加一组 Params/Result；该声明同时
  生成能力清单和服务端穷举分发用的 enum，不能另建平行字符串清单。领域逻辑
  必须是可独立测试的函数，由 Control API 直接调用，禁止复制实现。
- 流式能力：继续走独立 op/连接，不伪装成短 RPC。
- 插件能力：后续由进程外插件 manifest 声明 action/event/capability，经受限适配器调用
  Control API；不要把每个第三方插件名硬编码成新的核心 method，也不要让插件直接进入
  smeltd 进程或绕过 capability 校验。
- 在没有第二个真实调用方前，不引入动态 handler registry、IDL 编译器或新的网络服务。
