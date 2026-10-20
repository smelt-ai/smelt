# 展台演示（Smelt Demo.app）

展台用的是单独的 **Smelt Demo.app**，不是正式版加上一个开关。它用另一套 bundle id（`com.zzfn.smelt.demo`），数据放在临时目录，不读也不写主人的 `~/.smelt`，也不改 `~/.claude`、`~/.codex` 这类智能体配置。

窗口固定 **1440×900** 逻辑点。菜单名是 Smelt Demo。自动更新在这个包里是空操作，也不会打出 dmg。

## 打包和打开

```sh
make demo
open "dist/Smelt Demo.app"
```

已经有 release 二进制时，可以只组装、不重新编译：

```sh
./scripts/package-mac.sh --demo
open "dist/Smelt Demo.app"
```

开发时不用打包，也可以从普通二进制进同一套隔离：

```sh
SMELT_DEMO=1 cargo run --bin smelt
cargo run --bin smelt -- --demo --demo-route conversation
```

`--demo` 不会进 `smelt agent` / `smelt automation` 那些子命令。

## 落到哪一页

启动参数 `--demo-route` 或环境变量 `SMELT_DEMO_ROUTE`：

| id | 页面 |
| --- | --- |
| `workspace`（`session` 同样） | 工作台。种子项目「灯塔笔记」和它的终端 |
| `agents` | 智能体目录。虚构的「灯塔编辑」「港湾审阅」 |
| `automations` | 自动化。内置模板之外，还有暂停着的「灯塔晨报」「港湾巡检」 |
| `conversation` | 一场只存在于内存里的对话：读文件、待批准的 diff |
| `settings-agent-runtime` | 设置里的「Agent 运行环境」 |

对话页不会去连真实模型。点允许或拒绝、再发一句，都留在这个窗口里。

## 里面有什么

种子都是虚构的，写在这次启动的临时家目录下：

- 项目 `projects/lumen-notes`：`src/notes.rs` 的提交是 `hello harbor`，工作区里是 `hello booth`（本机有 git 时才会有这次提交）
- 两个智能体，模型 id 是 `demo-local`，钩子是关的
- 两条暂停的自动化。晨报有一条已完成的运行记录和工具卡片；暂停是为了展台当场不会去拉起智能体
- 工作台里有一条终端会话，会在临时家目录里开一个真 shell

每次启动默认换一个新的临时目录。要反复打开同一份数据，把 `SMELT_DEMO_HOME` 设成一个不含 `..` 的绝对路径，而且不能落在你自己的家目录、`~/.smelt`、`~/.claude`、`~/.codex` 或 `~/Library` 里面。

## 不会发生的事

默认不启动真实智能体，也不检查更新。真要在隔离目录里试跑智能体，才设置 `SMELT_DEMO_LIVE_AGENTS=1`。

macOS 钥匙串不在这次隔离里。即便开了上面的开关，Claude / Codex 仍可能读到钥匙串里已有的登录。展台不要开这个开关。

窗口 1440×900，要 2× PNG 可用 `screencapture -l <id>`（先 `screencapture -l` 列出窗口）。
