# 技能插件设计：以 `.agents/skills` 为准

## 一、核心判断

早期实现把 `~/.smelt/skills` 当作私有 canonical 目录，再向各 agent 目录打软链。
这个私有目录本身没有价值——没有任何 agent 读它，它只是个中转站，却要求用户接受一个
smelt 专属概念，并且卸载 smelt 之后这些 skill 会失去入口。

现在 `.agents/skills` 已是跨 agent 的事实标准：**它不在 Agent Skills 规范里**——规范
（原 Anthropic，现 `agentskills/agentskills`）只定义 skill 目录**内部**长什么样，从不规定
skill 放在哪；`.agents/skills` 是官方 client 实现指南里记下的**约定**，被十余家独立采用。
但支持度并不整齐（Claude Code、Kiro 至今不读），所以「完全不做中间层」还做不到。

结论：**canonical 目录直接用 `.agents/skills`**。它同时是存放处和通用生效处，中间层消失。
smelt 从「skills 的所有者」退成「skills 的整理者」——卸载 smelt，一切照常工作。

## 二、目录语义（不引入任何元数据文件）

作用范围由「它躺在哪个目录」表达，磁盘仍是唯一真相：

| 位置 | 含义 |
| --- | --- |
| `~/.agents/skills/<name>` | 用户级 · 通用 |
| `<project>/.agents/skills/<name>` | 项目级 · 通用 |
| `~/.claude/skills/<name>`、`<project>/.github/skills/<name>` … | 对应 agent 专属 |

「通用 / 特定 agent」不是新增字段，而是 `.agents/` 与 `.claude/` 之类的目录区别。
好处：不必维护 sidecar json，不会出现元数据与磁盘不一致。

## 三、兼容桥（bridge）

对尚未原生支持 `.agents/skills` 的 agent，从其自有目录软链回 `.agents/skills/<name>`。

- bridge 是**派生态**，不是用户数据。每次扫描重算、可一键修复，永远指回 `.agents`。
- agent 是否需要 bridge 由**能力表驱动，不写死在逻辑里**：每个 agent 一条
  `{ label, home, user_dir, project_dir, reads_agents_dir }`，在 `agents.js`。某个 agent
  哪天原生支持了，改一行，bridge 自动收敛掉。
- 这几个字段各有各的必要，都是被真实数据逼出来的，不是提前抽象：
  - `home` 单列一列，不从 `user_dir` 截首段——`~/.config/opencode` 这类截不出来，而且
    有的 agent 没有 skills 目录却有配置目录。
  - `user_dir` / `project_dir` 允许为 `null`：**Codex、Grok 压根没有自己的 skills 目录**，
    `.agents/skills` 就是它们的原生位置。给它们打 `~/.codex/skills → ~/.agents/skills`
    是纯无用功（Codex 永远不会去扫那里），所以 `null` 必须照实处理，不能兜底造路径。
- **目前只有 Claude Code 需要 bridge**，其余都按读 `.agents/skills` 处理。给原生读的 agent
  多打链接不是「保险」而是有害：它会把同一个 skill 数两遍。
- Kiro、Antigravity 是取舍，不是事实（2026-10-17 定）：官方文档写的是它们自家目录（见附录），
  产品上先按读 `.agents/skills` 处理，不替它们建链接。这两家读不到 `.agents/skills` 时，
  skill 对它们不生效；要支持，把 `reads_agents_dir` 改回 `false` 即可。
- **只认「在用」的 agent**，判定 = `~/<agent 配置目录>`（如 `~/.claude`）存在。自动打桥只给
  在用的 agent；作用范围里能勾选的也只列在用的（唯一例外：skill 现在就放在某个没装的
  agent 目录里，那一项照列，否则会被悄悄取消）。判定只看用户主目录——项目里的
  `.claude/` 往往是用过之后才出现，拿项目判断会让第一个项目级 skill 一条桥都打不出来。
- `*_dir` 为 `null` 的 agent 不能被单独勾选：「只给 Codex 用」这件事在物理上不成立，
  界面里不列它，逻辑层也会拒绝并说明原因，而不是建个没人读的目录假装成功。
- **软链一律是「引用」，永远不是本体**——不管它躺在哪个目录。通用目录和旧目录里同样
  可能有软链（早期版本就往 `.agents/skills` 里放过）。漏了这条，跟随软链的判断会把它
  当成实体副本，于是同名处理时用户选中软链保留、真身被移进回收目录，链接当场悬空，
  **skill 直接消失**。
- 同理，删除本体时要清理**所有**根目录里指向它的软链，不能只扫 agent 目录。
- 悬空链接不属于任何 skill，因此单独报成 `broken`，列在列表最后的「失效链接」一组——否则它们会永远
  躺在 agent 目录里。
- **磁盘随时会被 smelt 之外的人改**：手动 clone、别的工具安装、直接 rm。面板不做文件
  监听（跨平台的目录递归监听成本与踩坑都不小），在这些时机重扫：打开面板、换项目、
  每次写操作之后、「⋯」菜单里的「刷新」。

## 四、交互设计

面板窄，**只有一层列表，不切页**：点一行就地展开（`Collapsible`），看到什么就在什么旁边改。
「导入」展开在列表顶部。只有删除、清理这类不可轻易撤回的确认用居中的
`AlertDialog`。表单不用 `Dialog`：gpui-shell 给脚本的 `Dialog` 内容在打开时定格，不画按钮，
也不能指定位置。代码分三个文件：`main.js` 列表，`editors.js` 表单和会话，`ui.js` 小部件。

视觉跟 smelt 自己的面板走：字号只用 `text_xs` / `text_sm`（随界面字号缩放），按钮用紧凑档，
插件不画页面底色（宿主铺和文件树一样的实底），悬停、展开行用 `accent`。

- **列表**：用户级 / 项目级两组。每行是名字、作用范围标签、一行描述；右侧固定一句话说清
  「几个 agent 能用」或「缺 N 个链接」，悬停只改底色、不换内容。一次只展开一行。
- **展开区**，从上到下：完整描述 → 可用 agent（缺链接时「补齐」就在这一行标题右边）→
  作用范围（一句话现状，右边「修改」，表单就在这一行下面展开）→ 路径和修改时间（点路径在
  访达里打开），右边「删除…」。
- **界面按「能不能用」说话，不出现「链接」**：agent 标签只分能用（只写名字）和「还不能用」；
  行右侧写「Claude Code 还不能用」或「N 个 agent 还不能用」，按钮是「让 … 也能用」；预览
  写「让 Claude Code 能读到：<路径>」。悬空链接对外叫「已失效」。
- **问题就在出问题的那一行上**，不另设汇总横幅：缺链接、同名冲突、旧位置都在行上标出、
  展开后处理；一次补齐所有缺链接在头部「⋯」菜单里（有缺链接时才出现）。失效链接不属于
  任何 skill，列在列表最后一组，组标题右边「清理…」。
- **作用范围**：先给结论，再列将执行的文件操作，用户点「应用」前知道磁盘会发生什么。
  预览和执行共用 `planScopeChange`，预览里写的就是真会做的。计划由 4 种原子动作组成：
  `move` / `link` / `unlink` / `trash`。
- **冲突（同名多份）**：不自动清理。列出每一份的位置和改动时间，用户选「以哪份为准」，
  其余移入回收目录 `~/.smelt/trash/skills/<时间戳>/` 而非直接删，可手动找回。
- **导入**：头部「导入」在列表顶部展开面板：来源 → 装到哪里 → 将执行。先复制到目标旁的暂存目录（点开头，扫描会跳过；
  同卷，交换才是原子的），目标已有同名目录时原子交换两边，已经指着目标路径的兼容链接
  全程不悬空，换下来的旧版本进回收目录。复制时跳过软链：软链是别人的引用，不是 skill
  的内容。

## 五、迁移

扫描时若发现 `<base>/.smelt/skills` 非空，这些 skill 的行上标「旧位置」，展开后：

> 放在 smelt 私有目录，没有 agent 读得到 [迁移到 .agents/skills]

迁移做三件事：移动目录 → 重建 bridge → 删除空的 `.smelt/skills`。可跳过，不强制；旧路径
继续被扫描并标注「旧位置」，保证不迁的人也不掉数据。

## 六、明确不做（避免过度设计）

- 不做 skill 的启用 / 停用开关（等价于移出目录，多一套状态就多一处不一致）
- 不做 skill registry / 市场 / 版本号
- 不做内容同步：同名多份**永不**自动合并内容，只让用户选主
- 不收非主流 agent：能力表只收 smelt 已集成通知 hook 的几家，加上 Gemini CLI

## 附录：能力表证据（核实于 2026-02）

`.agents/skills` 是约定不是规范：`agentskills/agentskills:docs/specification.mdx` 只定义
skill 内部结构；`docs/client-implementation/adding-skills-support.mdx:45-57` 明确写着
「规范并不规定 skill 目录放在哪」，并把 `.agents/skills` 列为跨 client 互通约定。

| agent | user_dir | project_dir | 读 `.agents/skills` | 证据 |
| --- | --- | --- | --- | --- |
| Claude Code | `.claude/skills` | `.claude/skills` | **否**（文档只列 `.claude/skills`；2.1.302 二进制里的 `.agents/skills` 只用于把它复制进 `.claude/skills` 的导入工具） | code.claude.com/docs/en/skills |
| Codex | 无 | 无 | 是（原生） | developers.openai.com/codex/skills/ |
| Copilot | `.copilot/skills` | `.github/skills` | 是 | docs.github.com … copilot-cli/customize-copilot/add-skills |
| Cursor | `.cursor/skills` | `.cursor/skills` | 是 | cursor.com/docs/context/skills |
| Gemini CLI | `.gemini/skills` | `.gemini/skills` | 是（优先级更高） | geminicli.com/docs/cli/skills/ |
| Grok | 无 | 无 | 是（原生） | `superagent-ai/grok-cli` README + `src/utils/skills.ts` |
| Antigravity | `.gemini/config/skills` | 无 | 按是处理（文档：项目级是；用户级否，App/IDE 读 `~/.gemini/config/skills`、CLI 读 `~/.gemini/antigravity-cli/skills`） | antigravity.google/docs/skills |
| Kiro | `.kiro/skills` | `.kiro/skills` | 按是处理（文档只写 `.kiro/skills`，没提 `.agents/skills`） | kiro.dev/docs/skills/ |
| OpenCode | `.config/opencode/skills` | `.opencode/skills` | 是 | opencode.ai/docs/skills/ |

未收录（非主流，2026-10 起不再支持）：Amp、Qwen Code、Cline、Roo Code、Windsurf、Trae。
要加回某一家，照上表格式在 `agents.js` 里补一条即可。另有 **iFlow CLI**（用 `IFLOW.md`
上下文文件模型）、**Aider**（不支持 skill，用 `CONVENTIONS.md`）从来没有收录。
