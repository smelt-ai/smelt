# smeltd Control API v3

状态：已实现。方法名的真源是
`crates/smelt-runtime-client/src/protocol/control.rs` 的 `define_control_methods!`。
参数和结果类型在 `crates/smelt-model/src/control_api.rs`（`smelt-core` 再导出同一模块，
没有 `smelt-core/src/control_api.rs` 这个文件）。服务端分发在 `smeltd/src/control.rs`。

Control API 是 smeltd 的**版本化短请求协议**，不是 HTTP 服务，也不取代 ACP。
当前 `CONTROL_API_VERSION = 3`，`CONTROL_API_MIN_SUPPORTED_VERSION = 3`
（`crates/smelt-runtime-client/src/protocol/control.rs`）。
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
  "control_version": 3,
  "request_id": "request-abc",
  "method": "system.describe",
  "params": {}
}
```

成功响应：

`system.describe` 的成功响应（daemon 上的新字段都是可选的，`None` 表示运行时早于这个字段）：

```json
{
  "control_version": 3,
  "request_id": "request-abc",
  "status": "ok",
  "result": {
    "control": {
      "current_version": 3,
      "min_supported_version": 3,
      "max_supported_version": 3,
      "methods": ["system.describe", "session.list"]
    },
    "daemon": {
      "version": "2026.1023.0-rc.1",
      "exe_mtime": 0,
      "exe": "/home/me/.smelt/bin/smeltd",
      "pid": 4242,
      "started_at": 0,
      "session_count": 2,
      "agent_event_version": 2,
      "runtime_id": "01HRUNTIME",
      "os": "linux",
      "arch": "x86_64",
      "hostname": "gpu-box",
      "desktop": true,
      "smelt_home": "/home/me/.smelt"
    },
    "events": {
      "protocol_version": 2,
      "projection_version": 1,
      "session_projection_version": 3,
      "session_state_schema_version": 3
    }
  }
}
```

`daemon` 字段：`runtime_id` 是运行时永久身份；`os` / `arch` 来自 `std::env::consts`；`hostname` 是默认机器名；`desktop` 表示这台机器装了远程桌面（`install.sh --with-desktop`）；`smelt_home` 是这台运行时的 `SMELT_HOME`，客户端用它认出远程机器上哪些会话是智能体对话。`methods` 在真实响应里是完整清单，这里只示意两项。

失败响应：

```json
{
  "control_version": 3,
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
    "min_supported_version": 3,
    "max_supported_version": 3
  }
}
```

## 方法

| method | 性质 | 说明 |
|---|---|---|
| `system.describe` | 只读 | 返回版本范围、方法列表和 daemon 运行信息 |
| `session.list` | 只读 | 返回 smeltd 已发布的终端与 ACP 会话 ID |
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

上表是最早一批仍在用的方法，不是全集。同一份 `define_control_methods!` 里还有这些组（只加方法不升 `control_version`，客户端用 `system.describe` 的方法清单判断运行时有没有）：

| 组 | 方法 |
|---|---|
| 远程与设备 | `remote.config.get` / `remote.config.set` / `remote.status` / `remote.status.wait`（状态和 `known` 不同就回，最多挂 20 秒）；`device.pair_begin` / `device.pair` / `device.list` / `device.revoke` |
| 会话生命周期 | `new_session.catalog`、`session.create`、`session.delete`、`session.agent_bindings`、`session.launch.prepare`、`session.loaded_skills`、`conversation.create_for_agent`、`conversation.image`、`subagent.transcript` |
| 工作区与文件 | `workspace.conversation.create` / `update` / `delete`、`workspace.project.open` / `close`（单条改目录，改了就推 `workspace.changed` 整份目录；关对话时运行时顺带收掉它的终端和 ACP 会话）；`workspace.load` / `workspace.save`（整份读写，桌面换到单条改动后删掉）；`workspace.browse_dirs`；`project.tree` / `project.search` / `project.delete_entry` / `project.watch`；`file.open` / `file.save` |
| Git | `git.discover`、`git.repo_info`、`git.status`、`git.diff.*`、`git.commit` 等。没有 `git.worktree.*`、`git.is_dirty` |
| 智能体记忆 | 见下面的 `agent.memory.*` 表。另有 `memory.test_embedding` |
| Pi / 额度 | `pi.config.*`、`pi.models.*`、`pi.providers.*`、`pi.mcp.*`、`pi.plugins.*`、`pi.auth.*`；`quota.get` / `quota.refresh` |
| 其它 | `resource.usage`（守护进程和各会话进程树的 CPU、内存）、`agent.hooks.*`、`agent.cli.inspect` / `agent.cli.install`、`agent.host.get` / `agent.host.apply_edit`、`launch.get` / `launch.replace`、`new_session.pins.*`、`terminal_theme.publish`、`history.*`、`desktop.type_text`、`automation.run_detail` |

### `agent.memory.*`

记忆正文在文件系统，元数据由主守护代写。会话侧的三个方法用会话 id 和 daemon 发给该会话的 capability token 证明身份。设置页的方法按智能体 id 操作两层：`agent`（这个智能体自己的）和 `user`（跨智能体的用户全局记忆）。语义见 `docs/agent-memory.md`。

| method | 说明 |
|---|---|
| `agent.memory.meta` | 读这个会话两层记忆的使用统计和来源 |
| `agent.memory.record_read` | 记下读过哪一条。参数带 `layer` 和 `file` |
| `agent.memory.record_write` | 记下写入。`trust` 是 `trusted` 或 `pending` |
| `agent.memory.list` | 设置页列出某个智能体的两层记忆 |
| `agent.memory.delete` | 删除一条。用户全局层还会从所有智能体里消失 |
| `agent.memory.approve` | 采纳一条待确认记忆 |
| `agent.memory.ensure_dir` | 确保某层目录存在，返回它在那台机器上的路径 |
| `agent.memory.watch` | 关注某个智能体的两层记忆：之后目录里有文件变化，运行时推 `agent_memory.changed`（`agent_id` 为空是用户全局层），客户端据此重取列表。幂等，监听保持到运行时退出 |
| `memory.test_embedding` | 试一次向量检索服务。`api_key` 省略时用已存密钥，响应不回密钥本身 |

CLI：`smelt agent` / `smelt automation` / `smelt settings`（无这些子命令时 `smelt` 仍是 GUI）。教模型用这组 CLI 的技能（`packages/pi-agent/skills/smelt`）随 smelt-pi 发布，产品智能体勾选了才传给 Pi，不写进 `~/.agents/skills`。无头机器上的安装、配对、中继和 MCP 预设是 `smeltd install` / `smeltd pair` / `smeltd devices` / `smeltd relay` / `smeltd mcp`，不是这组 CLI。

## 版本与重试

1. 服务端接受 `min_supported_version..=max_supported_version` 内的请求，并使用请求版本
   回包；客户端可根据 `unsupported_version.supported_versions` 选择双方共有版本。留在
   同一支持范围内的版本必须保持 method payload 可加性兼容；破坏性变更应新增 method，
   或在提升版本的同时提升 `min_supported_version`，不能假装继续兼容旧 DTO。2026-10-18
   移除 worktree 功能时删掉了 `git.worktree.*`、`git.is_dirty`，`git.repo_info` 只回当前
   分支，版本仍是 1：客户端按 `system.describe` 的方法清单判断运行时有没有要用的方法，
   旧桌面连新运行时的 Git 面板需要升级桌面。v2 改了 `workspace.load` / `workspace.save`
   的文档形状，同时删掉跨 Agent 消息的 `agent.list` / `agent.send`，两边最低版本都抬到 2。
   v2 → v3：删掉 DeepSeek Harness 之后，`agent.host.get` / `agent.host.apply_edit` 的智能体配置去掉 `native_dsh_profiles`。旧运行时缺这个字段会拒收修改，所以最低版本抬到 3。同一版 `system.describe` 的 daemon 信息增加 `smelt_home`。
2. 调用方必须根据稳定错误码分支，不能解析自然语言文案：参数、授权、目标缺失、前置
   条件、冲突、忙碌和临时不可用分别返回 `invalid_params`、`permission_denied`、
   `not_found`、`failed_precondition`、`conflict`、`busy`、
   `temporarily_unavailable`。无法进一步分类的失败才使用 `operation_failed`。

## 扩展规则

- 新增核心短命令：在共享契约的静态 method 声明中增加一组 Params/Result；该声明同时
  生成能力清单和服务端穷举分发用的 enum，不能另建平行字符串清单。领域逻辑
  必须是可独立测试的函数，由 Control API 直接调用，禁止复制实现。
- 流式能力：继续走独立 op/连接，不伪装成短 RPC。
- 插件能力：后续由进程外插件 manifest 声明 action/event/capability，经受限适配器调用
  Control API；不要把每个第三方插件名硬编码成新的核心 method，也不要让插件直接进入
  smeltd 进程或绕过 capability 校验。
- 在没有第二个真实调用方前，不引入动态 handler registry、IDL 编译器或新的网络服务。
