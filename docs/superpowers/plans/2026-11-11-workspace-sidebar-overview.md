# Workspace Sidebar Overview Implementation Plan

> 此计划记录上一轮实现；扁平列表、先选仓库与「工作区」命名已被
> [任务工作树计划](2026-11-12-task-worktrees.md)替代。覆层总览与通知环防护继续沿用。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 保留项目下的扁平会话列表，在会话标签前增加分支图标，用项目行 `工作区·N` 标签的悬浮覆层承载总览与显式操作，取消浏览工作区时隐式创建终端的行为。

**Architecture:** 工作区目录和 Git 状态继续由运行时推送及现有缓存提供，桌面不建立第二份业务目录。新增一个侧栏工作区模块负责纯投影、覆层临时状态和展示；复用并扩展现有新建会话选择器，使多仓库工作区先选择仓库，再明确选择启动类型。后端协议、SQLite schema、Git 生命周期及现有会话数据均不变。

**Tech Stack:** Rust 2024、GPUI `gpui-pre =0.3.11`、锁定修订的 `gpui-base` / `gpui-component`、现有 `smelt-model` 目录模型。

---

## 执行约束与依据

实施状态：业务代码、针对性用例和文档已写入当前 worktree；格式化及补丁空白检查已执行。2026-11-12 经用户授权完成 release 编译、App/DMG 打包、签名与镜像校验，并重启隔离测试实例；control RPC 响应正常，原有 2 个工作区和 4 个项目仓库记录保留。测试套件和用户端到端验收尚未执行。用例先于对应实现加入，但没有执行红／绿阶段。

11:28 修复更新：首次启动验收遗漏 GUI 活性，用户发现卡死后确认是状态实体与父视图的通知环。新增真实 GPUI 回归，旧实现触发 8 次通知（用例上限）而非预期 1 次；改用元素状态保存独立视图后，`cargo test -p smelt session_list:: -- --nocapture` 的 31 个侧栏用例全部通过。指定文件 rustfmt、`git diff --check`、GUI crate Clippy、release 打包及 App/DMG 校验完成。新 GUI 的 5 秒主线程采样有 420/425 样本处于事件等待，CPU 约 2%，不再停留于通知循环；daemon RPC 正常，2 个工作区和 4 个仓库记录保留。启动时应用自动更新 daemon 并恢复原有两个会话。未执行完整测试套件或用户交互端到端验收。

- 工作目录：`/Users/pengxu.liu/workspace/SMELT/worktrees/workspace-session-binding/smelt`；分支：`feat/workspace-session-binding`。
- 用户已确认 `docs/superpowers/specs/2026-11-11-workspace-sidebar-overview-design.md`，包括将操作搬到覆层条目的「⋯」和多仓库先选仓库。
- 目录里已有上一轮功能的大量未提交修改。只修改本计划的文件，不还原、不顺手重构、不自动提交。
- `AGENTS.md` 禁止未经授权运行构建、检查、测试或打包。以下测试命令供用户执行或在用户明确授权后执行；先写失败用例，但不得为完成计划而自行运行。
- 不安装依赖、不更新组件库、不改当前测试库、不删除现有终端，不启动或停止任何实例。
- 不把业务数据存到 JSON，不在 render 中读取库、扫描目录、执行 Git 或启动网络请求。

## 已核实的组件限制

依赖修订为 `0ebd729f4441ee57d630bcde4a3743bd1284edd6`：

- `gpui_component::hover_card::HoverCard` 支持鼠标在触发区和内容间移动，但桌面没有公开的点击打开、程序化开关接口。不要只加一个 HoverCard 就声称同时支持点击和键盘。
- `gpui_component::popover::Popover` 支持 `.open(bool)`、`.on_open_change(...)` 和键盘，但打开时会主动移动焦点。不能因鼠标路过项目标签就夺走终端输入焦点。
- `gpui_base::Popup::new(id, trigger).anchor(...).content(...)` 负责测量、窗口边界避让、deferred 绘制及鼠标遮挡，不擅自接管焦点。使用它承载本功能共享的悬浮／点击覆层，不复制组件库的定位算法。
- 分支图标复用项目已注册的 `smelt-icons/git-branch.svg`。组件库的兼容 `IconName` 不包含 `GitBranch`，不能根据完整资产枚举推断组件枚举可用；使用 `Icon::empty().path(...)`，不新增图标依赖。
- `Window::use_keyed_state` 会订阅状态实体并通知当前父视图。若该实体同时观察父视图并调用 `cx.notify()`，会形成同步通知环。独立 Render 视图只用元素状态保存实体，不自动转发子视图通知给父视图；保留父到子的单向刷新。2026-11-12 卡死现场已确认此环使 GUI 主线程持续运行 `flush_effects`、CPU 100%，而 daemon RPC 仍正常，因此启动验收必须同时确认 GUI 事件循环可用，不能仅凭后台 RPC 判定。

## 文件职责

| 文件 | 操作与职责 |
|---|---|
| `crates/smelt/src/session_list/workspaces.rs` | 新建。工作区过滤及概览、覆层交互状态、数量标签和内容、条目操作；单元测试放本文件。 |
| `crates/smelt/src/session_list.rs` | 注册新模块，项目行接入标签，删除旧工作区行，会话标签增加图标及完整信息提示。 |
| `crates/smelt/src/session_list/new_session.rs` | 扩展绑定工作区的新建入口，提供仓库选择及目标说明，复用现有启动动作、Pin 和 confirm 分支。 |
| `crates/smelt/src/project_repositories.rs` | 只调整该功能的用户可见「工作集」称呼为「工作区」，不改变仓库管理行为。 |
| `crates/smelt/src/workspace_sessions.rs`、`crates/smelt/src/workspace_persist.rs` | 仅同步此功能涉及的重命名标题、错误提示称呼；保留显式终端创建方法及后端调用。 |
| `docs/workspace.md` | 同步入口、自动概览、无隐式终端和 UI 称呼。 |

不修改 `sidebar_order.rs` 的排序模型；不新增工作区导航路由、颜色配置、简介字段、数据库表或 RPC。

### Task 1: 固定投影规则及覆层状态转换

**Files:** Create `crates/smelt/src/session_list/workspaces.rs`；Modify `crates/smelt/src/session_list.rs` 的模块声明。

- [x] **Step 1: 先写项目归属和仓库概览用例。**

在新模块的 `#[cfg(test)] mod tests` 中引入 `smelt_model::workspace_directory::{WorkspaceWorkset, WorkspaceWorksetRepository}`，用以下完整 fixture 和用例固定项目归属、持久顺序及概览顺序：

```rust
fn workset(id: &str, root: &str, position: u32) -> WorkspaceWorkset {
    WorkspaceWorkset {
        id: id.into(),
        project_root: root.into(),
        task: id.into(),
        branch: format!("feat/{id}"),
        position,
        repositories: vec![
            WorkspaceWorksetRepository {
                name: "tools".into(),
                position: 1,
                ..Default::default()
            },
            WorkspaceWorksetRepository {
                name: "smelt".into(),
                position: 0,
                ..Default::default()
            },
        ],
    }
}

#[test]
fn overview_filters_exact_project_and_keeps_configured_order() {
    let rows = vec![
        workset("later", "/project", 1),
        workset("other", "/project-other", 0),
        workset("first", "/project", 0),
    ];
    let selected = project_worksets(&rows, "/project/");
    assert_eq!(
        selected.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        vec!["first", "later"]
    );
    assert_eq!(repository_summary(selected[0]), "2 个仓库：smelt、tools");
}
```

- [x] **Step 2: 实现对应的内存投影，不依赖会话数量。**

```rust
fn project_worksets<'a>(
    worksets: &'a [WorkspaceWorkset],
    project_root: &str,
) -> Vec<&'a WorkspaceWorkset> {
    let mut result = worksets
        .iter()
        .filter(|workset| {
            crate::sidebar_order::same_project_root(&workset.project_root, project_root)
        })
        .collect::<Vec<_>>();
    result.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
    result
}

fn repository_summary(workset: &WorkspaceWorkset) -> String {
    let mut repositories = workset.repositories.iter().collect::<Vec<_>>();
    repositories.sort_by(|a, b| {
        a.position.cmp(&b.position).then_with(|| a.name.cmp(&b.name))
    });
    let names = repositories
        .iter()
        .map(|repository| repository.name.as_str())
        .collect::<Vec<_>>()
        .join("、");
    if names.is_empty() {
        "0 个仓库".into()
    } else {
        format!("{} 个仓库：{names}", repositories.len())
    }
}
```

运行时隔离发生在调用处：只传所属 `RuntimeSpace.worksets`，禁止从所有运行时按路径拼成一份列表。

- [x] **Step 3: 先写覆层行为用例，再实现下面的纯状态。**

覆盖三条行为：鼠标离开触发区、尚未进入内容时不能同步关闭；进入内容后到期关闭事件不能关掉；点击固定后鼠标离开不能关掉，显式关闭才复位。

```rust
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct OverviewVisibility {
    open: bool,
    pinned: bool,
    trigger_hovered: bool,
    content_hovered: bool,
    focused: bool,
}

impl OverviewVisibility {
    fn trigger_hover(&mut self, hovered: bool) {
        self.trigger_hovered = hovered;
        if hovered {
            self.open = true;
        }
    }

    fn content_hover(&mut self, hovered: bool) {
        self.content_hovered = hovered;
    }

    fn pin(&mut self) {
        self.open = true;
        self.pinned = true;
    }

    fn close_if_idle(&mut self) {
        if !self.pinned && !self.trigger_hovered && !self.content_hovered && !self.focused {
            self.open = false;
        }
    }

    fn dismiss(&mut self) {
        *self = Self::default();
    }
}

#[test]
fn crossing_into_content_cancels_hover_close_and_click_pins() {
    let mut visibility = OverviewVisibility::default();
    visibility.trigger_hover(true);
    visibility.trigger_hover(false);
    assert!(visibility.open);
    visibility.content_hover(true);
    visibility.close_if_idle();
    assert!(visibility.open);
    visibility.pin();
    visibility.content_hover(false);
    visibility.close_if_idle();
    assert!(visibility.open);
    visibility.dismiss();
    assert!(!visibility.open);
}
```

另加未固定、无焦点且指针已离开的到期关闭用例。这里只控制交互状态，不保存目录数据，不在 render 启动定时任务。

- [x] **Step 4: 注册 `mod workspaces;`。记录待执行命令，不自行运行：**

```bash
cargo test -p smelt --bin smelt session_list::workspaces::tests
```

预期：新增投影及交互用例通过。首次确认失败应在步骤 1、3 尚无对应实现时进行，由有权限执行测试的人完成。

### Task 2: 实现项目数量标签和可操作覆层

**Files:** `crates/smelt/src/session_list/workspaces.rs`。

- [x] **Step 1: 增加 `WorkspaceBadge` 的 RenderOnce 包装，以及窗口内 keyed view。**

构造接口采用：

```rust
pub(super) fn workspace_badge(
    workspace: Entity<Workspace>,
    project: ProjectRef,
) -> WorkspaceBadge
```

`WorkspaceBadge` 保存以上两个参数；在自身 `RenderOnce::render` 中用 `project_element_id("workspace-overview", &project)` 建立 keyed view，而不是在父 Workspace 的可变借用期间再次读取同一 Entity。

view 保存 `WeakEntity<Workspace>`、`ProjectRef`、`OverviewVisibility`、触发区及内容区各自的 `FocusHandle`、可取消的单次关闭任务、必要的订阅及前一次焦点。通过已有 Workspace 通知观察目录变化，绘制时读取所属 RuntimeSpace 的当前内存列表，禁止把首次打开的列表永久缓存。

- [x] **Step 2: 用共享 Popup 构造单个浮层宿主。**

具体构造顺序为 `gpui_base::Popup::new(稳定项目ID, 数量标签按钮).anchor(Anchor::TopRight).offset(px(4.))`；仅当 `visibility.open` 时附加内容。内容使用现有 `ui_theme::glass_floating()`、`card_stroke()`、主题字号和圆角，设置实色背景，避免后面的终端文字透出。

内容采用普通 `div` 列表，显式最大高度并 `.overflow_y_scroll()`，宽度限制在当前窗口可用范围内。不引入已有新建选择器刻意避开的虚拟 List。数量按钮保留独立命中区，并使用主题文字样式。

- [x] **Step 3: 绑定事件，保证悬浮不抢焦点、点击和键盘可用。**

| 事件 | 状态与副作用 |
|---|---|
| 进入数量标签 | `trigger_hover(true)`，取消待关闭任务，通知重绘；不 focus。 |
| 离开标签／内容 | 更新对应 hovered 状态，启动可取消的一次性关闭延迟，到期只调用 `close_if_idle()`。 |
| 进入内容 | `content_hover(true)`，取消待关闭任务。 |
| 点击标签／Enter／Space | `pin()`，保存当前焦点并聚焦内容。 |
| 打开条目菜单 | 先 `pin()`；父覆层不因指针移到子菜单而消失。 |
| 点击外部／Escape | 先关闭最上层子菜单；关闭本覆层时 `dismiss()`，仅在焦点仍属于本覆层时恢复前次焦点。 |

一次性关闭任务仅从交互事件创建，重入取消旧任务，并使用代次或任务取消保护防止旧回调关闭新打开的覆层。没有后台循环、sleep 重试或周期查询。父覆层和子 Popover 的外部点击判断必须使用现有焦点／弹层机制，不能在子菜单仍打开时先关闭父层。

触发按钮和内容拦截必要的鼠标事件，不能冒泡到项目行的展开、折叠和拖拽处理。

- [x] **Step 4: 渲染空状态和每条工作区信息。**

零条时保留 `工作区·0`，展示「暂无工作区，可通过项目右侧＋新建」。有条目时显示 task、`repository_summary`、分支图标和 branch，当前绑定标记直接读取激活会话的运行时及 `workset_id`，不能使用 `.position()` 找第一个会话再判断 active。

将旧行的 Git 状态聚合搬到模块内，继续读取现有缓存，缺失和失败显示未知。离线时使用现有运行时连接状态标记，操作不可用但保留最后已知列表。纯粹查看或点击名字没有启动回调。

### Task 3: 搬迁操作入口并显式选择会话工作目录

**Files:** `crates/smelt/src/session_list/workspaces.rs`、`crates/smelt/src/session_list/new_session.rs`。

- [x] **Step 1: 给现有新建选择器增加工作区仓库上下文。**

保留现有 `new_session_picker` 普通项目调用方式；增加绑定工作区的构造入口：

```rust
pub(super) fn workset_session_picker(
    workspace: Entity<Workspace>,
    runtime: crate::runtimes::Runtime,
    workset: WorkspaceWorkset,
) -> NewSessionPicker
```

给 `NewSessionPicker` 及 `NewSessionPickerState` 增加仓库选择数据：工作区名字、当前仓库清单及选中的仓库名。普通项目入口无此清单；单仓库自动选择唯一项；多仓库初始不选择，先画仓库名称与 worktree 路径选项。选中后再显示已有启动动作，并提供回到仓库选择的入口。

选中名映射到最新 `WorkspaceWorksetRepository.worktree`，不要继续传原旧行 `.min_by_key(position)` 的默认 cwd。

- [x] **Step 2: 复用现有 confirm，不新增另一套启动分发。**

保留 `NewSessionTarget::Conversation`、`Terminal`、`BlankTerminal` 三条现有路径，以及对话的 Agent 配置、Pin 和远程启动项读取方式。在 confirm 进入这些分支之前，再检查目标工作区仍存在、所选仓库仍属于它、运行时可用；失败用选择器的可见错误文案反馈，不能静默改成普通项目会话或空白终端。

新增选择器目标说明，展示工作区、仓库和实际 cwd。目录变化时保留仍有效的用户选择，所选项失效则撤销并要求重选，不能默默换到排序第一的仓库。

首次读取 Pin 偏好及刷新远端启动项放在选择器 `on_open_change` 的事件中；`sync_picker_state` 和内容绘制只读取缓存，不能经由 `current_new_session_preferences` 在 render 发起请求。这样复用选择器不会把既有的懒加载副作用带进新的总览渲染路径。

- [x] **Step 3: 覆层每条工作区提供「⋯」操作区。**

使用现有 Popover 承载操作列表；在列表中挂载上一步的 `workset_session_picker`，触发文字为「新建会话…」。父操作列表在子选择器打开期间保持挂载，避免按下菜单项后销毁尚未显示的选择器。新建选择器打开后不显示项目级仓库管理入口。

父 Popover 的外部点击按其命中区判断，不自动排除子浮层。通过新建选择器的 `on_open_change` 通知覆层，在子选择器打开期间设父菜单 `.overlay_closable(false)`，关闭后恢复；不修改依赖或复制一套启动分发。

另外两个动作复用现有调用：

```rust
ws.start_rename(RenameTarget::Workset(workset_id), window, cx);
```

```rust
ws.request_workset_finish(
    runtime,
    smelt_model::workspace_directory::WorkspaceWorksetFinishParams { workset_id },
    cx,
);
```

在开始重命名或收工交互前关闭覆层及操作菜单，保留原有确认、绑定会话保护和错误反馈。运行时或条目已失效时不得继续调用旧目标。

- [x] **Step 4: 在现有 new_session 测试模块补充目标选择用例。**

为仓库选择映射编写几个直接覆盖行为的用例：多仓库未选择不能启动；选择后得到指定仓库的 worktree 而不是第一个；更新后仓库失效必须重选。复用现有 fixture，不测试常量文案或 trivial accessor，不创建真实 PTY。

待授权的最小命令：

```bash
cargo test -p smelt --bin smelt session_list::new_session::tests
```

预期：原有启动动作及新目标选择用例通过，普通项目选择器行为保持不变。

### Task 4: 接入项目行并移除隐式终端路径

**Files:** `crates/smelt/src/session_list.rs`。

- [x] **Step 1: 在项目标题的弹性内容之后、现有右侧操作槽之前挂数量标签。**

只在真实项目标题使用 `workspaces::workspace_badge(this.clone(), project.clone())`。不要与原会话数量／「＋」共用的 28px 操作槽叠放；原槽的 hover 行为不变。

将项目名改为可收缩、可截断的元素：移除标题名自身的 `.flex_shrink_0()`，增加 `.min_w_0().overflow_hidden()`，使用 `truncated_line(group_name.clone())`；不要让标签挤出项目「＋」。

- [x] **Step 2: 删除整段独立工作区渲染循环。**

移除当前 `for workset in &project_worksets` 的行、隐式点击启动、默认首仓库 cwd 以及仅属于旧行的菜单／closure。新菜单已在 Task 3 保留原功能，不能先删入口后遗忘搬迁。

保留无会话但有工作区的项目可见性判断，避免启用隐藏空项目后把唯一总览入口隐藏。继续使用现有会话循环，不按工作区重排。

保留 `Workspace::add_session_in_workset`：显式选择 `BlankTerminal` 仍然需要它。消除的只是旧工作区行浏览时的调用，不是显式终端能力。

- [x] **Step 3: 给已有会话标签增加分支图标和完整信息提示。**

将现有文字 pill 改为 flex，图标使用 `Icon::empty().path("smelt-icons/git-branch.svg").size(px(12.))` 且不收缩；文字继续用 `truncated_line(task)`。保留主题的 muted 文字和原背景，不分配工作区颜色。

查到标签时一并带上 branch，悬浮显示完整工作区名及绑定分支。不要为图标另加独立 hover 提示，避免同一标签触发两个覆层。

### Task 5: 同步称呼与文档

**Files:** `crates/smelt/src/session_list.rs`、`crates/smelt/src/session_list/new_session.rs`、`crates/smelt/src/project_repositories.rs`、`crates/smelt/src/workspace_sessions.rs`、`crates/smelt/src/workspace_persist.rs`、`docs/workspace.md`。

- [x] **Step 1: 将本功能用户可见的「工作集」统一为「工作区」。**

范围包括新建入口、弹窗标题、创建按钮、仓库引用提示、重命名和收工反馈。内部类型、参数、RPC 和历史设计文档保持原名，不做全仓替换。

- [x] **Step 2: 更新 workspace 文档。**

说明项目数字标签与悬浮总览、自动概览来源、显式选择仓库和启动类型、创建不启动终端，以及已有终端不自动清理。移除把点击工作区与自动新建会话绑在一起的旧描述。

### Task 6: 静态复查及人工验收交接

**Files:** 本计划涉及的全部变更。

- [x] **Step 1: 按设计逐条核对。**

检查：runtime + project 身份、计数不依赖会话、名字变更后菜单目标不串行、目录更新时列表不陈旧、鼠标悬浮不抢焦点、跨悬浮间隙不闪退、子选择器不被父菜单销毁、Escape 按层关闭、错误不伪造成功。确认没有遗留旧工作区行的隐式启动 handler。

- [x] **Step 2: 仅对实际改过的 Rust 文件使用已有格式化方式。**

```bash
rustfmt --edition 2024 --config skip_children=true \
  crates/smelt/src/session_list/workspaces.rs \
  crates/smelt/src/session_list/new_session.rs \
  crates/smelt/src/session_list.rs \
  crates/smelt/src/project_repositories.rs \
  crates/smelt/src/workspace_sessions.rs \
  crates/smelt/src/workspace_persist.rs
git diff --check
```

不执行全树 cargo fmt。未修改的文件从命令中移除。

- [x] **Step 3: 交付待授权命令与人工场景，不把未运行写成通过。**

合并目标选择器、工作区投影及交互用例到一次最小测试调用：

```bash
cargo test -p smelt --bin smelt session_list::
```

预期：侧栏现有排序、键盘、新建动作及新增工作区用例通过。需要进一步编译或打包时先取得用户授权，不默认执行 `make dist-build`。

人工验收使用现有隔离测试项目：两个工作区、零工作区、多仓库新建、同工作区多会话、切换深浅主题、窄侧栏、长列表滚动、悬浮后继续终端输入、菜单与 Esc、离线与目录更新、收工保护。创建／浏览前后终端数量必须不变；只有显式选择启动动作才增加会话。不得为验收自动删除现有数据。

## 设计覆盖自查

- 标签图标、无身份色、扁平列表及项目行数量：Task 2、4。
- 自动仓库概览、绑定分支、当前会话和已缓存 Git 状态：Task 1、2。
- 悬浮、点击、键盘、焦点、子菜单和滚动：Task 1、2、3。
- 新建、重命名、收工入口及保护：Task 3、5。
- 零工作区、无会话工作区、离线、目录变更和可见错误：Task 2、3、4。
- 无隐式终端、不更改已有绑定及持久化：Task 3、4、6。
- 术语、文档和未授权不构建：Task 5、6。
