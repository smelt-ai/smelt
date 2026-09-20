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
- agent 是否需要 bridge 由**能力表驱动，不写死在代码里**：每个 agent 一条
  `{ label, home, user_dir, project_dir, reads_agents_dir }`，放在 `assets/agents.json`，
  用户可用 `~/.smelt/skills-agents.json` 整体覆盖。某个 agent 哪天原生支持了，
  改一行配置，bridge 自动收敛掉，不用发版。
- 三个字段各有各的必要，都是被真实数据逼出来的，不是提前抽象：
  - `home` 单列一列，不从 `user_dir` 截首段——`~/.config/opencode`、`~/.codeium/windsurf`
    这类根本截不出来，而且有的 agent 没有 skills 目录却有配置目录。
  - `user_dir` / `project_dir` 允许为 `null`：**Codex、Grok 压根没有自己的 skills 目录**，
    `.agents/skills` 就是它们的原生位置。给它们打 `~/.codex/skills → ~/.agents/skills`
    是纯无用功（Codex 永远不会去扫那里），所以 `null` 必须照实处理，不能兜底造路径。
  - `reads_agents_dir` 支持按作用范围分写 `{ user, project }`：Antigravity 项目级读
    `.agents/skills`，用户级只读 `~/.gemini/config/skills`，一个 bool 表达不了。
- `reads_agents_dir` 拿不准时填 `false`：多一条软链是安全的，漏了则 skill 不生效。
  按目前证据，**15 个支持 skill 的 agent 里只有 Claude Code、Kiro（以及用户级的
  Antigravity）需要 bridge**，其余都原生读 `.agents/skills`。反过来说，给原生读的 agent
  多打链接不是「保险」而是有害：它会把同一个 skill 数两遍。
- **只给「在用」的 agent 自动打桥**，判定 = `~/<agent 配置目录>`（如 `~/.claude`）存在。
  这是能力表敢写长的前提：没有这道闸，能力表一长，建一个通用 skill 就会在家目录里
  凭空造出十几个没人读的 `~/.xxx/skills`。判定只看用户主目录——项目里的 `.claude/`
  往往是用过之后才出现，拿项目判断会让第一个项目级 skill 一条桥都打不出来。
- 这道闸只管**自动**行为。用户在弹窗里显式勾选某个 agent 时不受限制：那是明确意图，
  该建目录就建（未安装的 agent 在弹窗里排后面并标注「未安装」）。
- 但 `*_dir` 为 `null` 的 agent 连显式勾选都不给：「只给 Codex 用」这件事在物理上
  不成立，弹窗里直接不列它，后端也会拒绝并说明原因，而不是建个没人读的目录假装成功。
- **软链一律是「引用」，永远不是本体**——不管它躺在哪个目录。这条不能只在 agent
  目录里成立：通用目录和旧目录里同样可能有软链（早期版本就往 `.agents/skills` 里
  放过）。漏了这条，`isDirectory` 会跟随软链把它当成实体副本，于是同名处理时用户
  选中软链保留、真身被移进回收目录，链接当场悬空，**skill 直接消失**。
- 同理，删除本体时要清理**所有**根目录里指向它的软链，不能只扫 agent 目录。
- UI 默认不展示 bridge 链接，只在异常时显示「缺 N 条兼容链接 → 修复」。
- 悬空链接不属于任何 skill，面板够不着，因此单独报成 `broken_links` 并给一个
  「查看并清理」入口——否则它们会永远躺在 agent 目录里。
- **磁盘随时会被 smelt 之外的人改**：手动 clone、别的工具安装、直接 rm。面板不做
  文件监听（跨平台的目录递归监听成本与踩坑都不小），改为表头一个「刷新」按钮
  ＋面板重新可见时补扫一次。只在启动时扫一次的话，手动装的 skill 要重启才看得到。

## 四、交互设计

**列表**：一级分组是 `用户级 / 项目级`；每行是「名字 + 描述 + 一枚作用范围 chip」。

chip 三态：

- `通用` —— 在 `.agents/skills`
- `Claude` / `Claude · Codex` —— 只在特定 agent 目录
- `冲突` —— 同名多份且不在同一真身下（红）

**「作用范围」弹窗**（取代原先的 迁移 / 合并 / 链接 / 处理副本 四个入口）：

```
作用范围
 ( • ) 通用（.agents/skills，所有 agent 可见）
 (   ) 仅指定 agent   [ ] Claude  [ ] Copilot  [ ] Cursor  [ ] Gemini CLI
                     （Codex、Grok 没有自己的 skills 目录，不出现在这里）

层级
 ( • ) 用户级        (   ) 项目级
--------------------------------------------
将执行：
  移动 ~/.claude/skills/foo → ~/.agents/skills/foo
  保留 Claude 兼容链接
                                [取消] [应用]
```

要点：**先给结论，再列将执行的文件操作**，用户点「应用」前知道磁盘会发生什么；
移动一律先建临时目录再原子 rename（沿用既有 staging 逻辑）。

**行内动作精简为**：`作用范围` / `编辑` / `访达` / `删除`。
原先的 `迁移到 .smelt`、`合并到全局`、`处理副本`、`一键清理` 全部并入前两个——
它们本质都是「改作用范围 / 改层级」。

**冲突（同名多份）**：不自动清理。展开显示每一份的路径、改动时间、描述摘要，
用户选「以哪份为准」，其余移入回收目录 `~/.smelt/trash/skills/<时间戳>/` 而非直接删，可撤销。

## 五、插件 op

| op | 作用 |
| --- | --- |
| `state` | 整份状态（agent 能力表、skill 列表、旧目录计数） |
| `create` / `update` / `delete` | 常规增删改 |
| `import.inspect` / `import` | 导入外部目录（暂存 + 原子安装，替换时链接不断） |
| `scope.plan` | 算出改作用范围要做的文件操作，供弹窗预览 |
| `scope.apply` | 执行同一份计划 |
| `bridges.repair` | 重建缺失的兼容链接 |
| `links.prune` | 清理指向已消失目标的软链（可按 `project_scope` 限定层级） |
| `conflicts.resolve` | 选定真身，其余进回收目录 |
| `legacy.migrate` | 迁 `.smelt/skills`，可带 `project_scope` 限定层级 |

`scope.plan` 与 `scope.apply` 共用 `planScopeChange`，预览里写的就是真会做的。
计划由 4 种原子动作组成：`move` / `link` / `unlink` / `trash`。

## 六、迁移

扫描时若发现 `<base>/.smelt/skills` 非空，列表顶部出现一次性横幅：

> 检测到 N 个 skill 存放在 smelt 私有目录，可迁移到通用的 `.agents/skills` [迁移]

迁移做三件事：移动目录 → 重建 bridge → 删除空的 `.smelt/skills`。
可跳过，不强制；旧路径继续被扫描并标注「旧位置」，保证不迁的人也不掉数据。

## 七、明确不做（避免过度设计）

- 不做 skill 的启用 / 停用开关（等价于移出目录，多一套状态就多一处不一致）
- 不做 skill registry / 市场 / 版本号
- 不做内容同步：同名多份**永不**自动合并内容，只让用户选主

## 附录：能力表证据（核实于 2026-02）

`.agents/skills` 是约定不是规范：`agentskills/agentskills:docs/specification.mdx` 只定义
skill 内部结构；`docs/client-implementation/adding-skills-support.mdx:45-57` 明确写着
「规范并不规定 skill 目录放在哪」，并把 `.agents/skills` 列为跨 client 互通约定。

| agent | user_dir | project_dir | 读 `.agents/skills` | 证据 |
| --- | --- | --- | --- | --- |
| Claude Code | `.claude/skills` | `.claude/skills` | **否**（已反向核实） | code.claude.com/docs/en/skills |
| Codex | 无 | 无 | 是（原生） | developers.openai.com/codex/skills/ |
| Copilot | `.copilot/skills` | `.github/skills` | 是 | docs.github.com … copilot-cli/customize-copilot/add-skills |
| Cursor | `.cursor/skills` | `.cursor/skills` | 是 | cursor.com/docs/context/skills |
| Gemini CLI | `.gemini/skills` | `.gemini/skills` | 是（优先级更高） | geminicli.com/docs/cli/skills/ |
| Grok | 无 | 无 | 是（原生） | `superagent-ai/grok-cli` README + `src/utils/skills.ts` |
| Antigravity | `.gemini/config/skills` | 无 | 项目级是，用户级否 | antigravity.google/docs/skills |
| Kiro | `.kiro/skills` | `.kiro/skills` | **否**（已反向核实） | kiro.dev/docs/skills/ |
| OpenCode | `.config/opencode/skills` | `.opencode/skills` | 是 | opencode.ai/docs/skills/ |
| Amp | `.config/amp/skills` | 无（项目级就写 `.agents/skills`） | 是 | ampcode.com/docs/customize/skills |
| Qwen Code | `.qwen/skills` | `.qwen/skills` | 是 | `QwenLM/qwen-code:packages/core/src/config/storage.ts` |
| Cline | `.cline/skills` | `.cline/skills` | 是（仅源码可证） | `cline/cline:apps/vscode/src/core/storage/skill-directories.ts` |
| Roo Code | `.roo/skills` | `.roo/skills` | 是 | docs.roocode.com/features/skills |
| Windsurf | `.codeium/windsurf/skills` | `.windsurf/skills` | 是 | docs.windsurf.com/windsurf/cascade/skills |
| Trae | `.trae/skills` | `.trae/skills` | 是（可能需手动启用） | docs.trae.ai/ide/skills |

未收录：**iFlow CLI**（仓库里搜不到 `SKILL.md`，用的是 `IFLOW.md` 上下文文件模型）、
**Aider**（不支持 skill，用 `CONVENTIONS.md`）。宁可不列，也不猜一个 `.iflow/skills` 出来。
