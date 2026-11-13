# 远程机器与手机端查看项目文件

状态：设计中，未开工。对应 issue #77（跟踪 issue #76）。

两件事：

1. **桌面**：打开远程机器上的项目时，工具面板（文件树、搜索、编辑器、变更）跟着项目所在的机器走，和本机项目体验一致。
2. **手机**：在对话里能浏览当前项目的文件、预览内容，从对话里点路径直接打开。第一版只读。

---

## 1 · 现状

### 运行时：方法都在，不需要改协议

控制方法定义在 `crates/smelt-runtime-client/src/protocol/files.rs` 和 `git.rs`，运行时实现在
`crates/smeltd/src/files_control.rs`、`git_control.rs`，规则在 `crates/smelt-core/src/project_files.rs`。

| 方法 | 要点 |
|---|---|
| `project.tree{root,dir}` | 只列一层；目录在前、按名字排序；固定隐藏 `.git`、`node_modules`、`target`、`.DS_Store`，不读 `.gitignore`；`dir` 必须在 `root` 里 |
| `project.search{root,query}` | 文件名 + 内容一起搜，遵守 `.gitignore`；最多 200 条，跳过 512KB 以上和二进制文件 |
| `file.open{path}` | 一次读完整个文件，返回 `kind` 四选一：`text{text,version}`（version 是内容 sha256）、`image{data(base64)}`（按扩展名判断，含 svg）、`binary`、`too_large{size,limit}`；文本上限 8MB，图片 20MB |
| `file.save{path,text,base_version}` | 带 `base_version` 时先比对磁盘内容的 sha256，不一致返回 `Conflict`；不带表示确认覆盖 |
| `project.watch{root}` | 登记监听，变化经事件总线 `project.changed` 推送，只带 `root`，200ms 合并一次；丢弃 `target`、`node_modules`、`dist`、`build`、`.next`、`.cache` 里的变化 |
| `git.watch_head{root}` | 只看 `root` 所在仓库的 HEAD，切分支时同样推 `project.changed{root}`；侧栏各项目的分支名靠它，不必递归监听每个项目。旧运行时没有，分支名只在窗口回到前台、重连时更新 |
| `git.*` | `status`、`diff.file`、`discover`、`repo_info` 等读写方法齐全 |

`file.open` / `file.save` 有意不限制路径（文件可能来自 `smelt open` 或对话里的链接）。远程连接只放行配对过的设备，
配对后拥有全部能力，事件订阅也包括 `project.changed`。

### 桌面：调用都只发本机

按机器调用的底座随 #78 已经落地：主线程 `runtime_call::target(&key, cx)` 取 `RuntimeClient`，
后台 `call_on::<C>(&client, params, timeout)`。智能体模块（`crates/smelt/src/agents/`）已经这样用。

文件相关的模块还没接上：

- `file_tree/workspace.rs` 的 `open_file_now`、`save_open_file`、`ensure_search`、`ensure_dir_listing`、
  `perform_delete_file` 都用本机的 `call`。
- `git_panel/mod.rs` 的 `git_call`、`git_network_call` 包的也是 `call`，Git 面板所有调用都经过它们。
- 「只支持本机」的源头是 `session_list.rs` 的 `active_project_root`：它只返回本机项目，文件树根、搜索、
  Git 面板、插件、项目抽屉的 key 都靠它。其它单点：
  - `workspace_sessions.rs` 的 `current_local_cwd`（Git 角标、diff 范围、日志刷新用）、`remote_project_notice`；
  - `tool_panel.rs` 里插件面板和文件树遇到远程项目显示提示；`git_panel/view.rs` 的「远程项目」占位；
  - `workspace_prepare.rs` 只给本机会话取仓库信息；`session_list.rs` 侧栏分支名只用本机仓库信息；
  - 远程项目的右键菜单换成 `remote_project_menu`；关闭项目时只清本机项目的 `project_ui`；
  - `main.rs` 事件循环里 `ProjectChanged` 只转给本机。
- 缓存只用路径当 key：`dir_cache`、`dir_inflight`、`git_status`、`git_repos`、`repo_info`、`watched_projects`、`project_ui`。
- 路径计算写死 `PathStyle::LOCAL`，远程应该用 `runtimes::path_style(key)`。
- 对话里的远程文件链接 `smelt-file://<机器 id>/<path>` 现在点了只报错（`main.rs` 处理 `InternalFileLink` 处）。
- 真正只能在本机做的：在浏览器打开、在 Finder 中显示、用 IDE 打开项目、插件（`plugins/host_fs.rs` 直接读写本机文件）。

### 手机：Rust 层没有封装，界面没有入口

- `crates/smelt-mobile` 没有调用任何 `project.*`、`file.*`、`git.*`；事件订阅用 `SubscriptionShape::CATALOG`，`project_changes` 关着。
- 已有目录浏览组件 `mobile/lib/features/dirs/dir_browser.dart`（`DirBrowser` + `DirBrowserView`），新建会话和
  智能体绑定目录在用，背后是 `workspace.browse_dirs`（写死 `include_files: false`，并隐藏所有点开头的项）。
- 会话摘要带 `cwd` 和 `project_root`。
- 对话里只有 diff 卡片（`DiffView.path`）带了文件路径；ACP 工具调用的 `locations` 在 `AcpEntry::ToolCall` 投影时就丢了，
  读写文件的工具调用只有标题字符串。
- Markdown 用 `flutter_markdown_plus`，`onTapLink` 只会外部打开；`flutter_svg` 已有；没有语法高亮。

---

## 2 · 桌面：工具面板按机器走（PR1）

### 2.1 项目根带上机器

- `active_project_root() -> Option<String>` 换成 `active_project() -> Option<ProjectRef>`
  （`runtime_space.rs` 已有 `ProjectRef{runtime, root}`），去掉本机过滤。
- `current_local_cwd` 换成返回 `(RuntimeKey, cwd)` 的 `current_cwd`，调用方一并改。
- 仓库信息、侧栏分支名、项目抽屉、关闭项目时的清理，都按 `ProjectRef` 处理，不再区分本机远程。
- 远程项目的右键菜单补上「钉进文件树」等不依赖本机的项；「用 IDE 打开」「在 Finder 中显示」仍只给本机。

### 2.2 缓存 key 带上机器

上面列的所有缓存改成以 `ProjectRef`（或 `(RuntimeKey, 路径)`）为 key；打开的文件状态也记下 `RuntimeKey`。
两台机器上同一路径的项目不会串。

### 2.3 调用按机器发

- 文件树的 5 个调用点、`git_call`、`git_network_call` 改成：主线程用 `runtime_call::target` 取客户端，
  带进后台 `call_on`。机器已移除时报「这台机器已经移除」，和智能体模块一致。
- 删除这些模块里对本机 `call` 的使用，本机也走同一条路（本机就是 `RuntimeKey::Local`），不留两套。
- 路径拼接、相对路径计算改用 `runtimes::path_style(key)`。

### 2.4 缓存失效靠 `project.watch` 推送

- 事件循环里 `ProjectChanged` 不再判断本机，带着来源机器转给 `on_project_changed(runtime, root)`。
- 重新订阅上（`Snapshot`）时清掉那台机器的监听记录、把它的缓存全部标旧，再重新登记，做法同智能体记忆的监听。
  登记失败的记下来，直到下一次重新订阅都不再试。
- 项目被激活时登记监听（现在已有，改成按机器）。
- 缓存（git status、分支列表、仓库发现、侧栏分支名、目录列表）都不按时间过期，只在收到事件时标旧：
  `ProjectChanged`、自己做完的 git 操作、重新订阅、窗口回到前台（兜住 inotify 用完、旧运行时这类收不到事件的情况）。
  标旧的数据在看得见时才重取。仓库发现开销大，两次之间至少隔 15 秒。Git 页开着且窗口在前台时每 60 秒自动 fetch 一次，
  远端变化没有事件可等。
- 目录缓存去掉「2 秒内算新鲜」，改成收到这个项目的 `ProjectChanged` 才失效。
  展开目录时先显示缓存，同时后台重取这一层，补上 watch 丢掉的那些目录（`target`、`node_modules` 等）里的变化。
  这对本机同样适用，两边一套逻辑。

### 2.5 编辑与冲突

- 保存沿用 `base_version` + `Conflict`：冲突时提示，再按一次保存强制覆盖。
- 收到打开文件所在项目的 `ProjectChanged` 时：文件没有未保存的修改，就后台重读并静默替换；
  有未保存的修改，就标出「这个文件在那台机器上被改过了」，保存时照常走冲突流程。
- 图片预览 `file.open` 已经返回 base64，远程无需额外处理。

### 2.6 只能本机做的操作

远程项目下隐藏：在浏览器打开、在 Finder 中显示、用 IDE 打开。插件面板保留 `remote_project_notice`，
其它面板删掉这条提示。

### 2.6.1 历史面板

运行时本来就有 `history.list` / `detail` / `delete`（手机端已经按机器在用），
桌面历史面板改成发给项目所在的机器：列表缓存 key 带上机器；来源 tab、profile、智能体定义按那台
机器的设置；智能体 space 用那台机器的 `SMELT_HOME` 认；ACP 继续、CLI/TUI 继续、迁移都开在存档
所在的机器上。

「技能」面板是 JS 插件，直接读写本机磁盘（软链接、改名、目录互换），运行时没有对应方法，
不在本 issue 范围，单独开 issue。

### 2.7 对话里的远程文件链接

`smelt-file://<机器 id>/<path>` 改为在那台机器上打开到内置编辑器，删掉现在的报错分支。

### 2.8 测试

- 缓存 key：两台机器同路径的项目互不影响。
- `ProjectChanged` 按机器分发；机器断开只清自己的监听。
- 远程文件链接路由到对应机器（扩充 `main_tests/internal_file_url_tests.rs`）。

---

## 3 · 手机：Rust 层与 Dart 接口（PR2）

### 3.1 新模块 `crates/smelt-mobile/src/files.rs`、`api/files.rs`

写法同 `agents.rs`：第一个参数 `runtime_id`，`fleet().node(id)?` 取当前连接，`client.call::<C>()`。

| 接口 | 背后的方法 | 返回 |
|---|---|---|
| `list_project_dir(runtime_id, root, dir)` | `project.tree` | `Vec<FileEntry{name, path, is_dir}>`，子项路径按那台机器的 `PathStyle` 拼 |
| `search_project(runtime_id, root, query)` | `project.search` | `FileSearch{hits: Vec<{path, rel, line, preview}>, truncated}` |
| `open_file(runtime_id, path)` | `file.open` | `FilePreview`，见下 |
| `resolve_file_link(runtime_id, cwd, target)` | 无，同步纯计算 | `Option<FileLinkTarget{path, line}>` |

`FilePreview` 是「`kind` 枚举 + 可选字段」的结构体（`document` / `image` + `svg` / `size` + `limit`）：
FRB 没开 freezed，带数据的 enum 生成不了 Dart 的 sealed class。文本的 `document` 是不透明的 `TextDocument`
（见第 5 节），Dart 侧再包一层接口 `TextSource`（`mobile/lib/data/files.dart`），演示和测试用纯 Dart 实现，
不加载 Rust 库。

- 目录浏览用 `project.tree`，不用 `workspace.browse_dirs`：后者隐藏所有点开头的项，项目里的 `.github`、`.env` 会看不到；
  前者只藏 `.git` 等四项，并保证不出项目根。
- `resolve_file_link` 的规则和桌面一致：`crates/smelt-ui/src/markdown_links.rs` 里不依赖 GPUI 的路径解析
  挪到 `smelt-model::file_link`，桌面改为调用它，手机按那台机器的 `PathStyle` 补全。
- 第一版不订阅 `project.changed`：手机上看文件是短时、主动的行为，进页面和下拉刷新时重取即可，也不做定时刷新。

### 3.2 Dart 侧

`mobile/lib/data/backend.dart` 的 `Backend`、`FrbBackend`、`data/demo_backend.dart` 的 `DemoBackend` 同步加上。
演示模式要有一棵示例项目树，覆盖文本、Markdown、图片、二进制、过大五种文件。

### 3.3 测试

`files/tests.rs` 用 `FakeRuntime` 覆盖四种 `kind`、搜索截断、方法不存在（旧运行时）的报错文案。

---

## 4 · 手机：界面（PR2）

目录 `mobile/lib/features/files/`。

### 4.1 入口

对话页标题栏加「文件」按钮：项目根取 `project_root`，没有就取 `cwd`，都没有不显示。

### 4.2 浏览

- 单独一页 `ProjectFilesPage`，不改 `DirBrowser`：后者是「选一个目录」的流程（快捷位置、已打开标记、
  底部动作按钮），#78 选绑定目录已经在用；看项目文件是「限定在项目根里、点文件打开」，硬并成一个组件
  两边都要绕。共用的是行组件（`ShelfRow`、`HairlineRows`）。
- 返回键先退目录，退到项目根才离开页面。
- 顶部搜索框走 `search_project`，停手 350 ms 才发；内容命中带行号，点了跳到那一行。
- 下拉刷新。

### 4.3 预览

| 类型 | 展示 |
|---|---|
| 代码 / 文本 | 等宽字体、行号、语法高亮；按行懒加载；可切换自动换行；长按选择复制 |
| Markdown | `flutter_markdown_plus` 渲染，可切到源码（源码视图同上） |
| 图片 | `Image.memory`；svg 用 `SvgPicture.memory`；可缩放 |
| 二进制 / 过大 | 信息卡：文件名、路径、大小（过大时）、原因 |

### 4.4 从对话跳转

- 对话页给子树挂 `FileLinkScope`（机器 + 对话的 cwd）。Markdown 链接先调 `resolve_file_link`，是文件就在
  App 里预览，否则照旧外部打开；文件预览里的 Markdown 也挂一层，cwd 是文件所在目录。
- diff 卡片的文件头可点（有 `FileLinkScope` 时显示成链接色）。
- 读写文件的工具调用暂不可点：要把 ACP 的 `locations` 带进 `AcpEntry::ToolCall`，会改动两端共用的数据模型，单独做。

### 4.5 不做

- 编辑和保存（第一版只读）。
- git 变更列表和 diff（issue 标为后续）。

### 4.6 测试

`test/features/files/files_test.dart`，演示后端的子类记录调用：逐层浏览与返回、搜索防抖与跳行、文本预览离开时
释放、Markdown 渲染 / 源码切换与相对链接、二进制、过大、对话链接带行号。代码高亮配色进
`test/theme/contrast_test.dart`，亮暗两套在搁板和钢底上都 ≥ 4.5。

---

## 5 · 手机端语法高亮

### 5.1 业界怎么做

| 做法 | 代表 | 说明 |
|---|---|---|
| 服务端高亮，客户端只画 | GitHub（网页和 App 的文件视图）、GitLab | 文件在服务端，高亮结果随内容下发；客户端零解析成本，各端一致 |
| Tree-sitter 增量解析 | Zed、Helix、Neovim、GitHub 代码导航；本仓库桌面（gpui-component） | 精度最好、支持增量编辑；代价是每种语言一份 C 解析器，体积大 |
| TextMate / Sublime 语法 | VS Code、Shiki、Sublime、bat（syntect） | 正则逐行解析，语言覆盖最广、配色生态最全；只读展示足够准确 |
| highlight.js 类正则 | highlight.js、Flutter 的 `flutter_highlight` / `re_highlight` | 实现简单，精度最低，大文件慢 |

几条共识：

- **不在 UI 线程解析**，结果缓存，只渲染可见部分。
- **有上限**：大文件、超长行（压缩过的 js/json）直接不高亮。VS Code 默认超过 20000 字符的行不做 tokenization，
  超大文件整体关闭语法功能。
- **颜色走主题**：解析器输出语义类别（keyword、string、comment…），由主题映射成颜色，不在解析结果里写死颜色。

### 5.2 Flutter 生态现状（pub.dev，2026-11）

| 包 | 最新版本 | 情况 |
|---|---|---|
| `flutter_highlight` / `highlight` | 0.7.0，2021-03 | 下载量最大但已停更五年，基于 highlight.js 10 |
| `re_highlight` | 0.0.3，2024-02 | highlight.js 11 的移植，Reqable 维护，`re_editor` 依赖它；独立版本已两年没发 |
| `syntax_highlight`（Serverpod） | 0.5.0，2025-08 | TextMate 语法 + VS Code 主题，内置语言少（十几种） |

纯 Dart 方案都在 Dart isolate 里跑正则：要么卡 UI 线程，要么得自己搬到后台 isolate 并在 isolate 间传整份文本。
语言覆盖和精度也不如 Rust 生态。

### 5.3 Rust 生态（crates.io，2026-11）

| crate | 情况 |
|---|---|
| `syntect` 5.3 | Sublime 语法，bat、delta、zola 在用；正则引擎可选 oniguruma（C）或 fancy-regex（纯 Rust） |
| `two-face` 0.5（跟随 bat 0.26） | 给 syntect 补上 bat 那套语法（TypeScript、TOML、Dockerfile、Kotlin、Swift 等）和主题；少数语法在 fancy-regex 下不可用，需按其文档核对 |
| `tree-sitter-highlight` / `arborium` / `lumis` | Tree-sitter 方案；每种语言一份 C 解析器，几十种语言会让手机库多出数十 MB |

### 5.4 方案：在手机端的 Rust 层用 syntect 高亮，Dart 只画

服务端高亮是 GitHub 的做法，但放在我们这里要给运行时加协议、要求运行时升级，还会给 smeltd 加一套语法库；
而手机本身就有 Rust 内核，离数据最近又不占 UI 线程，取舍下来放在 `smelt-mobile`：

- **依赖**：`syntect`（`default-features = false`，开 `parsing`、`default-syntaxes`、`regex-onig`）+ `two-face`
  （`syntect-onig`）。正则引擎选 oniguruma，理由见 5.5 的实测：比 fancy-regex 快 4.6 倍、包还小、语法多 7 种。
  手机库本来就在交叉编译 C 代码（`libsqlite3-sys` 的内置 SQLite、`ring`），不增加新的工具链要求。
  不用 syntect 的主题。
- **语义类别**：把 TextMate scope 归并成十几个类别（comment、string、number、constant、keyword、operator、type、
  function、variable、tag、attribute、punctuation、markup.heading、markup.link、diff.added、diff.removed…），
  Dart 在 `lib/theme/` 里映射成颜色，亮暗两套主题各自定义，`lib/theme/` 之外不出现颜色值。
- **文档句柄**：`file.open` 的文本留在 Rust，`FilePreview::Text` 返回 FRB 不透明类型 `TextDocument`
  （行数、字节数、语言名），Dart 按需取：
  - `lines(start, end) -> Vec<HighlightedLine{text, spans: Vec<(起始, 长度, 类别)>}>`：只取可见窗口；
  - `full_text()`：Markdown 渲染和复制全文时用。
  8MB 的文本不必整份进 Dart，列表滚到哪取到哪。
- **增量解析**：syntect 逐行解析、状态可克隆。每 1000 行存一个解析状态检查点，取窗口时从最近的检查点往后解析；
  顺序滚动只往前走，不重复解析。
- **上限**：超过 2MB 或含超过 10000 字符的行，整篇不高亮，只显示带行号的纯文本（数值实现时用真文件校准）。
- **语言识别**：先按文件名（`Dockerfile`、`Makefile`）再按扩展名，最后看首行（shebang）。认不出就纯文本。
- **线程**：FRB 的普通 `pub fn` 本来就跑在线程池，`lines()` 不阻塞 UI。
- **后续可复用**：对话里 Markdown 代码块、diff 卡片也能接同一个高亮器（`flutter_markdown_plus` 支持自定义代码块构建）。

### 5.5 实测（2026-11）

量法：单独一个 cdylib，`opt-level = "z"`、LTO、`codegen-units = 1`、strip，`aarch64-apple-ios` release；
速度在 M 系列 Mac 上跑 release。

| 组合 | iOS 动态库增量 | 语法数 | 加载语法集 | 首次编译全部语法的正则 | 2 万行 Rust 逐行解析 |
|---|---|---|---|---|---|
| syntect 默认语法 + fancy | 0.63 MB | 75 | 2 ms | — | — |
| syntect + two-face + fancy | 2.2 MB | 213 | 5 ms | 2.5 s | 3.7 s |
| syntect + two-face + onig | 1.7 MB | 220 | 5 ms | 0.17 s | 0.8 s |

- syntect 自带的 75 种缺 TypeScript、TSX、TOML、Kotlin、Swift、Dart、Vue、Svelte、Dockerfile、Zig、Nix、HCL 等，
  必须配 two-face。
- fancy 版少的 7 种里有 JSX（Babel 语法），onig 版都有。
- 正则是第一次用到某种语法时才编译；fancy 平均每种约 12 ms，onig 不到 1 ms。
- 按手机比 Mac 慢 2–3 倍估：onig 解析约 100 µs/行，2 万行要两秒左右。所以不能整篇一次高亮，
  只按可见窗口取、靠检查点续着解析；跳到文件末尾时先显示纯文本，解析到了再上色。
- iOS 交叉编译 onig 要设 `IPHONEOS_DEPLOYMENT_TARGET`，否则链接报目标版本不符；手机构建脚本要确认带着它。
  Android（NDK）还没实测，开工第一步先在两端各构建一次。
- 上限和检查点间隔（暂定 2 MB、1000 行）到真机上再校准。

---

## 6 · 拆分与顺序

| PR | 内容 | 依赖 |
|---|---|---|
| PR1 | 桌面工具面板按机器（第 2 节） | 无 |
| PR2 | 手机端：Rust 层、高亮、Dart 接口和界面（第 3、4、5 节） | 无，可与 PR1 并行 |

`resolve_file_link` 挪到 `smelt-model` 放在 PR2，桌面改为调用新位置也在 PR2 一并完成。

## 7 · 风险

- `project.watch` 丢弃构建产物目录的变化，去掉目录缓存的时间过期后，这些目录要靠展开时后台重取来更新。
- git status、分支列表、仓库发现、侧栏分支名都已改成纯事件驱动（见 2.4），本机远程一套逻辑，
  没有定时重取。监听登记失败或 inotify 用完时收不到变化，只能等窗口回到前台、重连时补上。
- `file.open` / `file.save` 不限路径，配对设备能读写那台机器上的任意文件。这是现有设计，本次不改，但远程编辑放开后暴露面变大，值得在 #76 收尾时复查。
- 旧运行时：这些方法早已存在，不涉及新协议；`runtime_call::error_text` 对 `MethodNotFound` 已有升级提示。
