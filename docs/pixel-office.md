# 像素办公室插件

状态：2026-11-21 实现。来源 `smelt.office`，视图 `smelt.office/main`，代码在
`crates/plugins/office`。启动栏自动从视图注册表列出「像素办公室」，不在宿主增加专用入口。

## 使用与范围

打开「像素办公室」后，当前 smelt 工作区里的会话按机器和项目分房间，每个未归档会话一个角色。
显示会话标题、provider 图标和状态；悬停可看完整标题、工作目录及运行时上报的工具或问题。
点击角色打开已有会话，Tab 和回车也能操作。长标题省略，工位在窄窗口自动换行，页面可滚动。
每个工位带矮隔断和朝向公共走道的门口，休息椅位于隔间外。房间有后墙和窗户，宽度按实际
工位数收拢、最多四列；窄窗口自动换行。人物经门口进出，不穿隔断，状态中断后也从当前位置续走。

名册来自 smelt 已管理的会话，不扫描机器上其它应用私有目录。已归档会话和自动化后台运行不出现在
办公室里：后者由自动化插件承载，不是会话视图能选择的普通会话。分屏终端合成一个角色，状态取
最需要处理的一格；不同终端 provider 的名字合并显示。空房间不保留，机器移除后其房间一起移除。

provider 身份直接使用共享目录，不另列支持白名单：

- 结构化会话：`ConversationAgentKind::ALL` 的 Claude Code、GitHub Copilot、Codex、Grok、
  Antigravity、Cursor、OpenCode、Kiro、Pi、Oh My Pi，以及 Custom ACP。
- 终端会话：`TerminalAgentKind::ALL`，包含上面的内置 CLI 和终端专用的 Crush。
- 自定义或未来的 provider：身份原样显示，仍有角色和跳转，不会因为没有内置图标而被过滤掉。
- 任意 shell 会话仍能展示；没有权威结构化事件时只显示空闲，不能凭终端标题推断智能体在工作。

## Pixel Agents 实现研究

核对的上游提交：
[`4dd14d612fd37d89ffbbc977959f572395c4eec8`](https://github.com/pixel-agents-hq/pixel-agents/tree/4dd14d612fd37d89ffbbc977959f572395c4eec8)。
以下结论来自代码，而非只看 README 的功能描述。

| 部分 | 上游做法 | smelt 的落地 |
| --- | --- | --- |
| 采集 | Claude hooks 归一化为 provider 事件；没有 hooks 时解析 JSONL 推断 | 复用 smeltd 的会话目录和结构化状态推送，所有 provider 使用同一入口 |
| 角色 | `OfficeState` 管角色、座位、布局；`characters.ts` 是 TYPE / IDLE / WALK 状态机，按工具分类切阅读或打字 | 七种活动驱动入场、走回工位、思考、打字、提醒、完成庆祝和回休息椅；状态中断时从当前位置重新规划 |
| 绘制 | Canvas 2D，关闭图片平滑，`requestAnimationFrame` 驱动，限制大帧间隔 | GPUI canvas 原生绘制 2pt 像素格；不嵌浏览器、不复制 JS 引擎 |
| 跳转 | `OfficeCanvas.performTap` 命中角色后调用 `onClick(agentId)` 聚焦终端 | 使用公开 `OPEN_VIEW` 命令和会话插件的 `SessionArgs`，保留机器与对话身份 |
| 布局 | 格子地图、家具阻挡、BFS 寻路、编辑和持久化布局 | 按项目分房间、按可见行虚拟化，1–4 列响应窗口；静态场景共享纹理，无需布局数据库或寻路引擎 |

对应源码：

- [Claude 事件归一化](https://github.com/pixel-agents-hq/pixel-agents/blob/4dd14d612fd37d89ffbbc977959f572395c4eec8/server/src/providers/hook/claude/claude.ts)
- [JSONL 解析](https://github.com/pixel-agents-hq/pixel-agents/blob/4dd14d612fd37d89ffbbc977959f572395c4eec8/server/src/transcriptParser.ts)
- [角色状态机](https://github.com/pixel-agents-hq/pixel-agents/blob/4dd14d612fd37d89ffbbc977959f572395c4eec8/webview-ui/src/office/engine/characters.ts)
- [办公室状态](https://github.com/pixel-agents-hq/pixel-agents/blob/4dd14d612fd37d89ffbbc977959f572395c4eec8/webview-ui/src/office/engine/officeState.ts)
- [帧循环](https://github.com/pixel-agents-hq/pixel-agents/blob/4dd14d612fd37d89ffbbc977959f572395c4eec8/webview-ui/src/office/engine/gameLoop.ts)
- [点击与坐标命中](https://github.com/pixel-agents-hq/pixel-agents/blob/4dd14d612fd37d89ffbbc977959f572395c4eec8/webview-ui/src/office/components/OfficeCanvas.tsx)

借鉴的是状态与交互思路。`scene.rs` 的人物、桌椅、电脑、盆栽和地板全部是原创代码像素画，
没有复制上游源码或素材，不新增外部素材下载及许可依赖。

## 数据和接口边界

`Host::workspace_directory` 是名册来源，`Host::session_states` 是状态来源。
插件只存显示投影，不持久化运行时镜像，不改 SQLite 表结构，不增加 provider 的接入路径。

订阅 `SessionsChanged`、`MachinesChanged` 和 `LocaleChanged`，在事件里重建房间和角色；
`ShownViewsChanged` 管可见性，`WindowActivated` 恢复显示。render 只读已缓存的投影。

相位可信度、已结束运行时和终端分屏的聚合优先级使用共享模型：
`WorkspaceConversation::status`、`AgentStatus::from_daemon_state`、`effective_phase`。
机器断线时显示离线，不继续播放上次缓存的工作动作。状态对应：

| 权威事实 | 办公室显示 |
| --- | --- |
| Thinking | 思考中 |
| ExecutingTool | 执行工具 |
| AwaitingApproval | 等待批准 |
| WaitingForUser | 等待输入 |
| Failed | 出错了 |
| 已结束、未启动、Connecting 或缺少回合级结构化来源 | 空闲 |
| 机器不可用 | 离线 |

不把工具名字符串当成状态事实。阅读与写代码等更细的动作需要运行时有足够结构化的信息后再扩展。

点击走 `smelt.app/open-view`，目标 `smelt.sessions/session`，参数：

```json
{"machine": null, "conversation_id": "<workspace conversation id>"}
```

远程机器的 `machine` 为 runtime id 字符串。这是会话插件的公开约定，权威在
`crates/plugins/sessions/src/contract.rs`；办公室不依赖会话插件 crate，不创建或重启会话。
命令或会话视图不可用时在办公室内显示错误。

## 动画与成本

`actors.rs` 管有限动作和沿通道走动的路线，`scene.rs` 只画取样后的帧。新角色从工位侧边入场；
工作时回到电脑前，思考时抬手并显示思考气泡，执行时打字、屏幕代码滚动。批准、输入、错误分别用
感叹号、问号和叉号区分，初次提醒有短暂弹出/挥手/摇头。明确成功后举手庆祝，再沿通道回休息椅。
休息角色闭眼并显示静态 Z；离线角色在当前位置定格并变灰。

状态中途变化时先取当前动作的位置，从那里走向新位置；不会跳回前一动作的起点。改名、普通状态
重推和语言切换不会重放入场。注册表按机器和会话 id 记角色，归档或移除后删掉对应动作状态。
只有运行时最新的权威成功终态才允许庆祝；普通空闲、失去运行时或离线不显示成功。

经显示器下一帧回调按 12fps 取样，没有 timer、sleep 或定时状态查询。prepaint 确认角色与
滚动裁剪区相交后才申请下一帧；多个工位共用一个节拍。入场、走动、提醒和坐下均为有限动作，
结束后的空闲、待处理、离线保持静态；只有正在工作时持续播放动作。视图隐藏、应用在后台、窗口
失焦或启用减弱动态效果时停止申请帧，并显示稳定姿势。
人物外观由会话 id 稳定派生，其它会话的增删与排序不改变它。

## 验证

在独立 worktree 中运行，不调用真实 agent，不消耗模型配额：

```sh
cargo check -j 2 -p smelt
cargo test -j 2 -p smelt-office
cargo test -j 2 -p smelt-plugin-api --test catalogs
cargo clippy -j 2 -p smelt-office --all-targets --no-deps -- -D warnings
bash scripts/lint-crate-deps.sh
```

原生 Metal / CoreText 截图覆盖完整 provider 目录、全部状态、离线、深浅主题和窄宽窗口：

```sh
cargo test -j 2 -p smelt-office office_snapshots -- --ignored --nocapture
```

输出位于该 worktree 的 `target/tmp/snapshots/office`。普通测试覆盖目录投影、状态可信度、
终端聚合、完整 provider 目录、自定义类型、跨平台路径、归档和机器移除、事件更新、鼠标与键盘跳转、
打开失败。截图是演示快照，不代表本机此刻正在运行这些 provider。

连续动画演示复用同一个 GPUI 视图、动作状态机和像素绘制代码，时钟和状态输入为演示数据：

```sh
# 需要本机 ffmpeg。导出 16 秒、12fps 的原生渲染录像，不启动任何 provider。
cargo test -j 2 -p smelt-office office_animation_demo -- --ignored --nocapture
```

视频在 `target/tmp/snapshots/office/office-demo.mp4`，关键帧在同目录的 `animation/`。
演示使用 100 个显示会话并覆盖完整 agent 目录，从入场、思考、执行，到批准/输入/错误/离线，
滚动到最后一批工位后，再回到工作、完成和休息。
导出测试同时断言镜头静止时工作画面确实变化、确实滚到最后几行、最后休息帧保持一致。演示时钟只在测试构建里存在，不向插件
的正式运行路径添加模拟状态、额外服务或开关。

会话数量的手动容量测量（不启动 provider、不消耗模型配额）：

```sh
cargo test -j 2 -p smelt-office office_capacity_benchmark -- --ignored --nocapture
cargo test -j 2 -p smelt-office office_interaction_benchmark -- --ignored --nocapture
```

同一项目分别放入 12、50、100、200、500、1000 个空闲或执行中的显示会话，视口为
1080×800pt。每组预热 5 帧，再测 25 次完整重绘的 CPU P50/P95。使用真实 CoreText，
关闭离屏 GPU renderer，逐帧清理元素 arena；不把 GPU present 或截图回读算作 CPU 时间。
输出保存在 `target/tmp/snapshots/office/capacity.json`。这测的是插件绘制开销，不包含
整个桌面、状态推送、GPU 和真实 agent 的资源竞争，不能据此承诺一个「不卡」的会话上限。

## 100 对话性能优化

房间在目录或列数变化时转换成标题行与工位行，GPUI `ListState` 只构建可见行。
1–4 列随视图实际宽度变化，调整宽度时锚定原工位；状态更新保持滚动位置，字号或语言变化重测行高。
工位中的人物仍按机器和会话 id 追踪，滚入视口后按当前时刻取样，不积压屏幕外的动画帧。

地板条带、后墙、桌椅和隔断在创建视图时生成 24 张共享纹理，原始像素约 11MiB，与会话数无关。
同时缓存人物轮廓的同色矩形，逐像素对比测试保证镜像、行走、抖动的像素不变；提示字形按同色行合并。
深浅主题纹理一次备齐，render 只读缓存，视图释放时经 `drop_image` 释放 atlas 条目。

2026-11-21 在 M5 Max / 64GB 上、同一开发构建与视口的重绘 CPU P95：

| 显示会话 | 优化前（工作中） | 优化后 | 构建工位数 |
| --- | --- | --- | --- |
| 100 | 76.9ms | 约 6ms | 16 |
| 1000 | 406.6ms | 约 6ms | 16 |

100 对话另测 100 帧持续滚动与批量状态切换，CPU P95 约 8ms，最多构建 20 个工位，
结果在 `interaction.json`。两个手动压测对 100 对话断言 CPU P95 < 16.7ms；性能结果依赖机器和负载，
不作为普通 CI 单测运行。基线保存在本 worktree 的 `capacity-before.json`，优化后结果在 `capacity.json`。
普通测试覆盖屏幕外工位不全量构建、滚到底部点击第 100 段会话、缩窄窗口、字号改变、状态变化保持
滚动位置，以及归档后移除旧的点击目标。以上验收针对 UI 显示数据，不代表同时运行 100 个真实 agent。
