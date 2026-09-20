# 变更栏改造方案

> 目标：补齐变更栏的交互与功能缺口，并让能力覆盖桌面 + 移动端。
> 结论先行：**不换技术栈，补能力层**。理由见第 3 节。

## 1. 现状盘点

### 1.1 代码分布

| 层 | 位置 | 行数 | 职责 |
| --- | --- | --- | --- |
| 领域逻辑 | `crates/smelt-core/src/git.rs` | 1501 | 跑 `git` 子进程、解析 porcelain/diff、分支/worktree/stash 操作。已按 `Subprocess` trait 抽象，可测 |
| 面板状态 | `crates/smelt/src/git_panel/workspace.rs` | 1721 | `impl Workspace` 的动作方法，字段仍散在 `main.rs` 的 `Workspace` 上 |
| 渲染 | `crates/smelt/src/git_panel/view.rs` | 1190 | 文件树 / diff / 提交框 / 各种确认弹窗 |
| diff 渲染 | `crates/smelt/src/git_panel/diff.rs` | 1471 | unified diff 元素、词级高亮 |
| 历史 | `crates/smelt/src/git_log.rs`, `git_log_view.rs` | — | `GitTab::Log` |

### 1.2 已具备的能力

状态刷新（notify 文件监听）、文件树聚合、子仓/submodule 展开、文件级 stage/unstage、
**hunk 级 stage/unstage/discard**（`git apply --cached [--reverse]`）、全部丢弃、
提交 / 提交并推送 / 仅推送、fetch / pull --rebase、stash push/pop、
分支切换 / 删除（本地 + 远端）/ 合并、worktree 新建 / 删除 / 清理失效、
diff scope 切换（工作区 / 暂存）、hunk 跳转。

### 1.3 对比 VS Code 的缺口（本次要解决的痛点）

按价值排序：

0. **多仓库识别（最高优先级，是 bug 不是缺失）** —— 详见第 1.5 节。
1. **行级 / 选区暂存与丢弃** —— 目前最小粒度是 hunk。VS Code 的 `stageSelectedRanges` 是高频能力。
2. **合并冲突处理** —— `git.rs` 里没有任何 `UU`/`AA`/unmerged 分支路径。状态解析会把冲突文件当普通改动，操作语义错误。这是**正确性缺陷**，不只是功能缺失。
3. **提交增强** —— amend、提交信息历史/复用、`--no-verify`、`--signoff`、模板。
4. **diff 视图** —— 并排（side-by-side）模式、忽略空白、切换上下文行数。
5. **任意 ref 对比 / 文件历史 / blame** —— 目前只能看工作区与暂存区。
6. **列表过滤与键盘流** —— 文件多时没有搜索/过滤；缺少纯键盘的 stage→commit 流。
7. **revert / cherry-pick / rebase 续作** —— 中断态（rebase in progress、merge in progress）目前完全不可见。

### 1.4 多仓库识别：发现模型选错了（核心问题）

案例：`/Users/zehua.wang/Desktop/work/maglev-config-model`——父仓 + 13 个 submodule，
多数在 `.gitmodules` 里配了 `ignore = all`，部分尚未初始化。但问题不限于 submodule。

**现有模型**：唯一数据源是父仓的 `git status`，发现 gitlink 后递归进去把子仓文件
**摊回父仓列表**（`git.rs::expand_nested_git_status`，拼成 `sub/inner`）。两个后果：

**后果一：发现不全。** 没在父仓 status 里出现的仓库，结构上永远看不到（已实测）：

```sh
# 父仓 .gitignore 里有 vendor/，vendor/lib 是独立仓库且有未提交改动
$ git status --porcelain=v1 -uall --ignore-submodules=none -b
## main
?? plain/                  # 另一个独立仓库，只是一条未跟踪目录，没有仓库身份
                           # vendor/lib 完全没出现
$ git -C vendor/lib status --porcelain
 M a.txt                   # 改动真实存在，面板里不可见
```

同类发现不了的：干净的子仓（status 不报）、平级的独立仓库、关联 worktree。

**后果二：读写语义断层。** 子仓文件能看能暂存，但**永远提交不出去**：

| 操作 | 实际作用的仓库 | 代码位置 |
| --- | --- | --- |
| stage / unstage / discard / diff | **按路径路由到子仓** | `git.rs::git_op_target` |
| commit / push / 分支显示 / ahead-behind / stash / discard_all | **写死父仓 `active_project_root`** | `git_panel/workspace.rs:1580` `commit()`、`push_only()` 等 |

最小复现（已验证）：

```sh
# 父仓 + submodule sub（ignore=all），改子仓文件
$ git status --porcelain=v1 -uall --ignore-submodules=none -b
## main
 M sub                      # 面板展开成 sub/f.txt
$ git -C sub add f.txt      # 面板勾选 stage → 落到子仓 index（正确）
$ git commit -m "fix"       # 面板点提交 → 在父仓执行
nothing to commit, working tree clean
exit=1
```

顺带确认两点现有实现是**对的**，改造时不要弄丢：

- 命令行 `--ignore-submodules=none` 能覆盖 `.gitmodules` 里的 `ignore = all`（实测有效），
  否则这类仓库的子仓改动会完全不可见。
- `is_nested_git_workdir` 用 `.git` 存在性探测，未初始化的空 submodule 目录自然跳过。

### 1.5 VS Code 的多仓库模型（参考实现，读自 `extensions/git` 源码）

根本区别：**Repository 是一等实体，父仓 status 从不用来发现子仓。**

**五条独立的发现来源**，全部收口到同一个 `Model.openRepository(path)`（`model.ts:600`）：

| 来源 | 机制 | 配置（默认值） |
| --- | --- | --- |
| workspace folders | `onDidChangeWorkspaceFolders` | — |
| **子目录扫描** | `traverseWorkspaceFolder()` BFS 遍历目录（跳过 `.git` 与忽略列表），逐个尝试 open | `repositoryScanMaxDepth`(1)、`repositoryScanIgnoredFolders` |
| 打开的编辑器 | 对文件所在目录 open | `autoRepositoryDetection: 'openEditors'` |
| 显式列表 | 用户配的路径 | `git.scanRepositories` |
| submodule / worktree | repo open 后读 `.gitmodules` / worktree 列表，逐个再走 `openRepository` | `detectSubmodules`(true) / `detectSubmodulesLimit`(10)、`detectWorktrees(Limit)` |

`openRepository()` 里是一整套守门逻辑，每一条都对应一类真实事故：

1. `getRepositoryExact` 去重；
2. `git rev-parse --show-toplevel` 求真实根（处理大小写不敏感文件系统）；
3. 忽略规则；
4. **父目录仓库不自动打开**（`openRepositoryInParentFolders: prompt`）——防止误拿到 home 仓库；
5. unsafe repository（`safe.directory` 不匹配）单独提示，不静默失败；
6. 用户手动关闭过的仓库不自动重开（`ClosedRepositoriesManager` 持久化）；
7. 信任检查后才建 `Repository` 实例。

每个 Repository 带 `kind: 'repository' | 'submodule' | 'worktree'`（`repository.ts:321`），
UI 上是**独立的 SCM provider**：自己的分支、提交输入框、提交按钮、计数徽章。
父仓 status 里的 submodule **只是一条 gitlink 资源，不展开**（`repository.ts:576`，
点开是 `submoduleOf` 的指针 diff）。超过 limit 时给警告并停止自动展开，而不是扫穿磁盘。

这也解释了为什么问题「不仅仅是 submodule」：submodule 只是五条发现来源里的一条，
它们汇入同一个 Repository 模型，因此平级仓库、被 ignore 的仓库、worktree 自然都能覆盖。

### 1.6 端的真实情况（重要）

- **remote-web 已在 `f691f16` 下线**（「下线 Web 支持（Cloudflare / WebRTC / 浏览器面板）」）。
  仓库里的 `remote-web/{dist,node_modules}` 是未清理的构建残留，且被 `.gitignore` 忽略，
  **不是活代码**。→ 建议顺手删除，避免继续误导。
- **移动端是 Flutter**，通过 iroh 隧道连 `smelt-remote-gateway`，
  FFI 只桥 `crate::api_iroh`（见 `mobile/flutter_rust_bridge.yaml`）。
  网关对外只有 `/acp/*` 与 `/terminal/*`，**没有任何 git 接口**。
- 决定性约束：**手机上没有代码仓库**。移动端的变更栏只能是「远程操作 PC 上的 git」，
  不可能在本地跑 git。

## 2. VS Code git 扩展可复用性结论

- 许可证：`microsoft/vscode` 的 `extensions/git` 随主仓 **MIT**，兼容本项目 Apache-2.0（保留版权声明即可）。
- 不能整体复用：它没有发 npm 包，整份代码 `import * as vscode from 'vscode'`——
  SCM provider、`TextDocumentContentProvider`（`git:` scheme）、`FileDecorationProvider`、
  QuickPick、`l10n` 全绑死在宿主 API 上。拿过来等于实现半个 extension host。
- **值得按算法抄写**（改写成 Rust，注明来源与 MIT 声明）：
  - `src/model.ts` 的仓库发现与 `openRepository` 守门链：直接对应缺口 #0（见 1.5）。
  - `src/staging.ts` 的 `applyLineChanges` / `toLineChanges`：行级暂存的正确做法，直接对应缺口 #1。
  - `src/git.ts` 的 `parseGitStatus`（`status -z` 的 rename/unmerged 处理）：直接对应缺口 #2。
  - `src/git.ts` 的 `getMergeBase` / `diffBetween` 等 ref 对比命令参数：对应缺口 #5。
  - `src/watch.ts` 的 `.git` 监听过滤（跳过 `index.lock` 抖动）：可对照现有 notify 实现查漏。
- **禁止参考 Zed 的 git panel：GPL-3.0，与本项目 Apache-2.0 不兼容。**

## 3. 路线选择：为什么不重写成 TS 插件

平台层面是可行的：`plugins/browser` 已经证明 `tool_panel` + webview 面板跑得通，
第一方 bun 插件可以直接 spawn `git`。但不划算：

1. **痛点是功能缺失，换栈不会自动补上**。行级暂存、冲突处理、amend 在任何语言里都是同一份工作量，
   TS 版本还要从零重做 4451 行已经跑通的面板逻辑（含 worktree 管理、子仓展开、与 `session_list` 的耦合）。
2. **「三端复用一套 TS」的前提不成立**。remote-web 已下线；移动端是 Flutter，
   要复用 TS UI 只能内嵌 webview，那是另一个体验决策，与 git 无关。
   而 `simple-git` / `isomorphic-git` 这类库在手机上毫无用处——手机上没有仓库。
3. **真正可复用的边界是协议，不是 UI**。桌面直接调 `smelt-core`，移动端走网关 RPC，
   共享的是同一份领域逻辑 + 同一份接口契约，这已经是 `smelt-core/git.rs` 的现有形状。

**插件化的正确落点**：把 git 能力注册成网关/daemon 的一组 Action（见 4.2），
而不是把变更栏整个搬进插件。这样第三方将来要接别的 SCM，走的是同一个注册点。

## 4. 方案

### 阶段 A0：Repository 成为一等公民（先做，解 1.4 的根因）

对齐 1.5 的 VS Code 模型：**发现与展示解耦**。这一步会重排数据结构，
后面所有功能都建在它上面，必须排在最前，否则全要返工。

1. **仓库发现器**（`smelt-core` 新模块），不再从父仓 status 推导：
   - 项目根本身；
   - 子目录 BFS 扫描，深度默认 1，跳过 `.git` 与忽略目录（`node_modules`、`target`、`.venv` …）；
   - `.gitmodules` 列出的 submodule（含未初始化的，标记状态而不是隐藏）；
   - 关联 worktree（复用现有 `list_worktrees`）；
   - 统一收口到 `open_repository(path)`：`git rev-parse --show-toplevel` 求真根 → 去重 →
     不往上越过项目根 → 发现上限（参考 10）超限提示而不扫穿。
   - 支持手动关闭 / 重新打开某个仓库，选择持久化到 SQLite。
2. `RepoStatus { root, rel_prefix, kind, branch, ahead, behind, files, stash_count, ... }`，
   工作区状态变成 `Vec<RepoStatus>`。每个仓库独立跑自己的 `git status`，
   **删除 `expand_nested_git_status` 的摊平逻辑**；父仓里的 gitlink 保留为一条指针改动。
3. 变更列表按仓库分组：每组 header 显示仓库名 + kind + 自己的分支 + ahead/behind，
   **提交框与提交/推送按钮归属于组**，不再是全局唯一。
4. `commit` / `push_only` / `stash` / `discard_all` / 分支菜单一律接收 repo root 参数，
   删除对 `active_project_root` 的隐式依赖。
5. 文件监听改成每仓一份（现在只监听项目根）。
6. 回归用例（必须有）：子仓文件 stage → commit → push 全链路；
   父仓 gitlink 指针提交与子仓提交互不串台；`ignore = all` 子仓可见；
   **被 `.gitignore` 忽略的独立仓库可见**；**干净的子仓也列出**；未初始化 submodule 不误报。

### 阶段 A：修正确性 + 补高价值交互（桌面，不动架构）

按顺序，每步独立可验收，都在 `smelt-core/src/git.rs` 先补带测试的纯逻辑，再接 UI：

1. **合并冲突**（这是正确性问题）
   - `parse_git_status` 识别 unmerged（`DD/AU/UD/UA/DU/AA/UU`），`GitStatusData` 新增冲突分组。
   - 面板单列「冲突」区；提供 `--ours` / `--theirs` / 标记已解决（`git add`）。
   - 识别仓库中断态：`.git/MERGE_HEAD` / `rebase-merge` / `rebase-apply` / `CHERRY_PICK_HEAD`，
     顶部显示状态条与 continue/abort。
2. **行级 / 选区暂存与丢弃**
   - 移植 `applyLineChanges` 语义：从已解析的 `DiffLine` 选区生成补丁，复用现有 `apply_patch` 通道。
   - diff 视图支持行选区（点选 + shift 连选 + 键盘）。
3. **提交增强**：amend（`--amend`，带 HEAD 提交信息回填）、提交信息历史下拉、`--no-verify` 开关。
4. **列表过滤 + 键盘流**：文件名过滤框；`space` 切换 stage、`enter` 打开 diff、`cmd+enter` 提交。
5. **diff 视图**：side-by-side 模式、忽略空白、上下文行数。

### 阶段 B：领域层收口（为移动端铺路）

- 把 `git_panel/workspace.rs` 里散在 `Workspace` 上的字段收成 `GitPanelState` 子结构体，
  面板逻辑与 GPUI 解耦；纯状态迁移，无行为变更。
- 在 `smelt-core` 定义 git 请求/响应 DTO（状态、diff、stage/unstage、commit、行级补丁），
  桌面与网关共用，避免两套解析。

### 阶段 C：移动端变更栏

- 网关新增 `/git/*`（沿用现有 token 鉴权与 `write_enabled` 写开关；
  **所有写操作必须受 `write_enabled` 门控**，与终端/ACP 一致）。
- Flutter 端做只读优先的变更栏：文件列表 + diff 查看 + stage/commit/push。
  行级暂存等重交互在手机上不做。

### 阶段 D：清理

- 删除 `remote-web/` 残留目录（已下线，构建产物 + node_modules）。

## 5. 风险与边界

- 按路径推导仓库的实现（`git_op_target`、`nested_git_target`）在 A0 后应该消失：
  文件天生属于某个 `RepoStatus`，不需要再猜。保留它等于保留两套真相。
- 扫描成本：深度默认 1 + 忽略目录列表是必须的，否则会在 `node_modules` 里扫到天荒地老。
- 行级暂存与子仓路径前缀（`prefix_diff_paths`）叠加时补丁路径易错，必须写专门用例。
- A0 会改动 `GitStatusData` 的形状，`session_list`、worktree 管理、状态徽章等调用方都要跟着走查；
  这是本方案影响面最大的一步，但也是唯一能根治 1.4 的做法——按路径给 commit 打补丁只会把断层藏得更深。
- 冲突态下 `discard_all` / `stash` 的语义与普通态不同，改动时要覆盖中断态分支。
- 网关开放 git 写操作会扩大远程攻击面：路径必须限制在已授权的 workspace 根内，
  不接受客户端传入任意 `root`。

## 6. 不做的事

- 不重写为 TS/webview 面板（理由见第 3 节）。
- 不引入 `isomorphic-git` / `simple-git`：本地 `git` CLI 已是唯一执行点，多一层无收益。
- 不做交互式 rebase / 提交图编辑：投入产出不成比例，终端更合适。
