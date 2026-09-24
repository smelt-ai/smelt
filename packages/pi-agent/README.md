# Smelt Pi Runtime

Smelt 的受管 Pi 运行时包。它由 Smelt 内置 Bun 启动，标准输入输出直接使用 Pi
官方 `--mode rpc` JSONL 协议；这里没有 ACP Agent，也没有 Smelt 自定义智能体协议。

`src/main.ts` 只提供稳定入口。随 Smelt 强制加载的是权限闸门和唯一选择题工具
`smelt_ask_user`（会话原语）。`smelt_*` 是宿主保留命名空间；用户 Extension
占用保留名时宿主不会覆盖，但对应调用会被明确阻断。其余业务能力继续以 Pi
Extension / Tool / Skill 交付。

## 边界

- Smelt 负责进程、流式 UI、取消、恢复与权限交互。
- Pi RPC/Runtime 负责模型调用、智能体循环、会话落盘、工具、Extension 与 Skill。
- `~/.pi/agent/extensions/`、项目 `.pi/extensions/` 以及 Pi 的 Skill 目录由
  `DefaultResourceLoader` 原生发现，不需要在 Smelt 核心里登记业务功能。
- Pi 本身不内置 MCP；当前不要把业务能力只放在 ACP/MCP 配置中。

## 运行与安全

发布时入口、Smelt Extensions、依赖补丁、`package.json` 和 `bun.lock` 会嵌入
`smelt-core`。用户首次打开 Pi 对话时，Smelt 用锁定的 Bun 在
`~/.smelt/runtime/pi-agent-v0.4.6/` 物化文件并以
`--frozen-lockfile --production --ignore-scripts` 安装锁定依赖，不依赖系统 Bun。

读取与搜索工具只有在 Pi registry 证明其确为 built-in 时才默认直接执行；写文件、
执行命令和 Extension 注册的 Tool 由权限 Extension 通过 Pi RPC 的 Extension UI
请求向 Smelt 逐次申请。选择题参数形状不参与权限判断；只有来源精确匹配 Smelt
具名内联 Extension 的 `smelt_ask_user` 免审批。只有上层明确把会话模式切成
`bypassPermissions` 才由 Smelt 自动批准，供声明了 FullAccess 的无人值守任务使用。

`smelt-context-usage` 也是强制加载的宿主 Extension。它只通过当前 Pi RPC 进程的
Extension UI 通道向本机 Smelt 宿主发布上下文用量，以及用于会话审计的实际 system
prompt、active tool schemas 和 provider request payload。provider payload 在 Pi 侧先按
敏感字段递归脱敏，Smelt 接收侧还会再次脱敏；这些 capture 不由该 Extension 发送到
第三方端点，也不被伪装成带 turn 归属的执行步骤。

Pi Extension 是本机代码，拥有与 Smelt 用户相同的系统权限。工具审批只能拦截 Pi
Tool 调用，不能沙箱 Extension 自己在加载期或事件回调中直接执行的代码；只应加载
可信 Extension。
