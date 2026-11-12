# Task Worktrees Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将任务工作树作为多仓库会话的目录和侧栏分组单位，保留普通项目会话。

**Architecture:** 在现有 WorkspaceWorkset 上推导任务根；新建选择器直接使用任务根。项目模式的绘制、键盘和会话切换共用工作树行投影；绑定不变，Git 沿用已有多仓库面板。旧会话不迁移进程目录。

**Tech Stack:** Rust 2024、GPUI 0.3.11、现有 SQLite 工作区目录与运行时路径模型。

---

按用户要求在现有专用 worktree 内 inline 实施，不创建 commit、不启动子代理、不自行构建或测试。规范见 `../specs/2026-11-12-task-worktrees-design.md`。

后续授权：2026-11-12 用户明确授权针对性编译与测试、修复问题后分步骤创建本地提交；不 push、不修改远程配置、不打包或重启实例。

### Task 1: 任务根与目录归属

**Files:** `crates/smelt-model/src/workspace_directory.rs`、`crates/smelt/src/workspace_sessions.rs`。

- [x] 先增加单／多仓库同根、Windows 路径和越界拒绝用例。
- [x] 在 WorkspaceWorkset 上实现共用路径入口：

```rust
pub fn task_root(&self, style: PathStyle) -> String {
    style.join(&style.join(&self.project_root, "worktrees"), &self.task)
}
pub fn contains_cwd(&self, style: PathStyle, cwd: &str) -> bool {
    style.is_absolute(cwd) && style.starts_with(&style.clean(cwd), &style.clean(&self.task_root(style)))
}
```

- [x] 桌面绑定校验改用 contains_cwd；已有 cwd 不被改写。
- [x] 任务根与路径范围用例已随 `cargo test -p smelt-model -p smelt-core -p smeltd workspace` 通过。

### Task 2: 不选仓库的新建入口

**Files:** `crates/smelt/src/session_list/new_session.rs`。

- [x] 删除 WorksetRepositorySelection 及三条旧仓库选择用例，保留完整工作树元数据作为启动目标。
- [x] 删除仓库列表、索引、选择／更换仓库和键盘分流；保留启动类型、Pin、离线提示。
- [x] 确认启动前同步目录、验证目标仍存在；使用 `workset.task_root(path_style(runtime.key(), cx))` 传给 ACP 与终端。
- [x] 选择器显示工作树名字和任务根；工作树行提供图标型＋触发器，覆层菜单继续用文字型触发器。

### Task 3: 分组投影及侧栏工作树行

**Files:** `crates/smelt/src/session_list/workspaces.rs`、`session_list.rs`、`runtime_space.rs`。

- [x] 新增纯 ProjectSessionRow 投影测试：普通会话在前，同 ID 的共享会话归在一个工作树行下，不丢失未知绑定的会话，不画空工作树。
- [x] 项目模式按投影逐项渲染；工作树行使用分支 SVG、折叠箭头、计数与绑定启动＋；嵌套会话再缩进一档，并去除重复标签。
- [x] 折叠保存在 RuntimeSpace 的 `HashSet<String>`，使用 workset ID，不按名字或列表下标。
- [x] 沿用无自动反向通知的视图状态保存方式，不重引 use_keyed_state 通知环。

### Task 4: 导航、上下文与排序

**Files:** `session_list/keyboard.rs`、`session_list.rs`、`workspace_sessions.rs`、`runtime_space.rs`；核对 `git_panel/workspace.rs`。

- [x] SidebarItem 增加 Worktree(ProjectRef, String)，纯父子导航用例覆盖工作树兄弟、普通会话与跨项目。
- [x] 可见行、快捷键顺序共用 ProjectSessionRow；激活时展开父工作树，拖拽不得跨绑定调整。
- [x] 工作树选中与绑定会话激活使 active_project_ref 返回任务根；普通项目上下文与现有产品对话规则不变。终端创建完成也复用 activate，避免上下文滞留在上一个项目。
- [x] 文件树／Git 共用任务根上下文；核实已有仓库发现识别 `.git` 文件式 worktree，Git 提交目标不使用非仓库任务根。已知任务的仓库目录直接投影到现有 RepoSet，避免扫描上限遗漏参与仓库；未选仓库时回退实际子仓库，并补充干净工作树的目标回归。

### Task 5: 文案、文档与静态交付

**Files:** `project_repositories.rs`、`workspace_persist.rs`、`workspace_sessions.rs`、`session_list/workspaces.rs`、`session_list/new_session.rs`、`docs/workspace.md`。

- [x] 只将此功能的用户文案改为「工作树」，不替换通用 Workspace 概念或 Git 的工作区／暂存区术语。包括后端直接展示给用户的生命周期错误。
- [x] 更新用户文档，标记上一版扁平列表与选仓库方案已被本设计替代。
- [x] 对实际变动 Rust 文件执行 `rustfmt --edition 2024 --config skip_children=true`，执行 `git diff --check`。
- [x] 提供授权后执行的命令：`cargo test -p smelt --bin smelt session_list::`、`cargo test -p smelt-model workset`、`cargo test -p smelt --bin smelt task_root_is_not_a_git_write_target`、`cargo clippy -p smelt --bin smelt --no-deps`。不声称未执行的用例通过，不自行打包重启。

### 授权后的验证结果

- `cargo test -p smelt-git --test worktree --test project_repository`：18 个通过。
- `cargo test -p smelt-store`：77 个通过，覆盖历史迁移、v36/v37 升级与绑定保护。
- `cargo test -p smelt-model -p smelt-core -p smeltd workspace`：56 个通过。
- `cargo test -p smelt --bin smelt -- session_list:: git_panel::tests::`：62 个通过，包含真实 GPUI 通知环回归。
- `cargo test -p smelt --bin smelt -- project_repositories:: tool_panel`：9 个通过。
- `cargo clippy -p smelt --bin smelt --no-deps`、指针样式检查、打包脚本语法检查、限定文件 rustfmt 与 `git diff --check` 通过；Clippy 仍有 warning。

验证中修复了测试临时目录清理、macOS 路径别名断言、历史迁移夹具和侧栏渲染上下文类型。另用失败回归确认：仅有仓库或工作树、没有打开项目与会话时，旧快照空判定会隐藏任务元数据；现已把任务元数据纳入空判定，升级后的仓库信息也可正常读取。

实现与自动化验证已完成。本轮没有重新打包、重启或做 GUI 交互验收；现有测试包不包含最新改动。
