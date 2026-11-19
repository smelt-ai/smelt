// 生成的，不要手改：见 crates/smelt-plugin-api/src/js/control_types.rs。
/**
 * 可开对话的 agent 选项。出线（`new_session.catalog`）只带展示与回传用的字段；
 * 启动规格和历史目录只在运行时那台机器上有意义，不出线，客户端解出来是空的。
 */
export type AcpAgentOption = {
  id: string;
  kind: string;
  label: string;
  profile: boolean;
  /**
   * 产品智能体选项：会话归属的定义 id。由 option id 的 `agent:` 前缀派生，
   * 见 [`agent_definition_id_from_option_id`]。
   */
  agentDefinitionId?: string | null;
};
/** 消息流里的一条。由 agent 会话历史重放，也可随 smeltd 热升级快照交接。 */
export type AcpEntry = {
  User: string;
} | {
  UserWithImages: {
    text: string;
    images: AcpImage[];
  };
} | {
  Assistant: {
    text: string;
    thought: boolean;
  };
} | {
  ToolCall: {
    id: string;
    title: string;
    kind: ToolKind;
    status: ToolCallStatus;
    /**
     * 保留结构（不压扁成一行文本）——diff 要能逐行渲染红/绿，压扁了就
     * 回不去了。
     */
    output: ToolOutputPart[];
    /**
     * 嵌套 transcript：子代理的正文/工具挂在发起它的 Task/Agent 调用下。
     * 旧快照没有这个字段，缺省为空。
     */
    children?: AcpEntry[];
    /**
     * Pi 子智能体派发：这次 `subagent` 调用派出的子智能体，来自工具结果。非空就画成
     * 子智能体卡片，过程在各自的子会话里，不挂 `children`。旧快照没有这个字段。
     */
    subagents?: SubagentRun[];
    /**
     * Pi `session_send`：发给哪个对话、什么类别、结果。有值就画成事件行，不画工具卡片。
     * 旧快照没有这个字段。
     */
    peer?: PeerSend | null;
    /**
     * Pi `smelt_ask_user`：问了用户什么、怎么答的。有值就画成问答行，不画工具卡片。
     * 旧快照没有这个字段。
     */
    asked?: AskRecord | null;
  };
} | {
  Divider: string;
} | {
  NewSession: {
    at_ms: number;
  };
} | {
  TurnFailed: {
    detail: string;
    hint?: TurnHint | null;
  };
} | {
  TaskNote: TaskNote;
};
/**
 * ACP/app-server 图片的传输与热升级快照表示。长期历史仍以 agent
 * 自己的 transcript 为准，Smelt 不把图片重复写入工作区快照。
 *
 * 只能用 [`AcpImage::new`] 构造：摘要缓存是私有字段。构造之后不要原地改
 * `data_b64`，换图就新建一张，否则缓存的摘要对不上数据。
 */
export type AcpImage = {
  mime: string;
  data_b64: string;
  /**
   * 只出现在要图片引用的连接上（`acp_watch` / `acp_snapshot` 带 `image_refs`）：
   * 大图不内联，`data_b64` 为空，客户端按这个引用用 `conversation.image` 去取。
   */
  original?: AcpImageRef | null;
};
/** 一张没有内联的图在运行时那边的位置。 */
export type AcpImageRef = {
  /** 图片所在顶层条目的全局下标。子代理嵌套条目里的图算在外层那条工具调用上。 */
  entry: number;
  /** 原图 [`AcpImage::digest`]。条目被回退或压缩改写后对不上，取图报 not_found。 */
  digest: string;
};
/**
 * 用户添加的一个自定义 Agent。两种用法：
 *
 * - **基于内置 agent**（`kind_id` 是内置 id）：沿用那一家的适配器、历史读取、续接和
 *   图标，只换启动命令、加环境变量或换数据目录（比如把 Claude 适配器指到另一个
 *   兼容 CLI：`CLAUDE_CODE_EXECUTABLE=…`）。
 * - **通用 ACP**（`kind_id` 是 `custom`）：任意 ACP stdio 命令，必须填 `command`。
 */
export type AcpProfile = {
  /**
   * 稳定 id（新建时生成，不随 label/kind 改变），会话存档和历史页 tab 都用它
   * 区分「同一个 kind 下的哪一个自定义 Agent」。
   */
  id: string;
  /**
   * 底层 agent 种类，存 `ConversationAgentKind::id()`（跟 `AcpSaved.agent` 同一份规则）。
   * 未知 id 原样保留；消费方不得改认成另一种 Agent。
   */
  kind_id: string;
  /** 新建菜单、会话标题、历史页 tab 上显示的名字，比如「Til」。 */
  label: string;
  /** 启动命令。空 = 底层 agent 的出厂命令；通用 ACP 必填。旧运行时不报，读成空。 */
  command: string;
  /** 额外注入的环境变量，覆盖底层 agent 的同名变量。旧运行时不报，读成空。 */
  env: { [key: string]: string };
  /**
   * 数据目录，允许 `~` 开头（展开逻辑见 `crate::workspace_override`）。空 = 不覆盖；
   * 只有底层 agent 有数据目录变量（如 `CLAUDE_CONFIG_DIR`）时才能填。
   */
  workspace_dir: string;
};
/**
 * 用户添加的一个自定义 Agent。两种用法：
 *
 * - **基于内置 agent**（`kind_id` 是内置 id）：沿用那一家的适配器、历史读取、续接和
 *   图标，只换启动命令、加环境变量或换数据目录（比如把 Claude 适配器指到另一个
 *   兼容 CLI：`CLAUDE_CODE_EXECUTABLE=…`）。
 * - **通用 ACP**（`kind_id` 是 `custom`）：任意 ACP stdio 命令，必须填 `command`。
 */
export type AcpProfileInput = {
  /**
   * 稳定 id（新建时生成，不随 label/kind 改变），会话存档和历史页 tab 都用它
   * 区分「同一个 kind 下的哪一个自定义 Agent」。
   */
  id: string;
  /**
   * 底层 agent 种类，存 `ConversationAgentKind::id()`（跟 `AcpSaved.agent` 同一份规则）。
   * 未知 id 原样保留；消费方不得改认成另一种 Agent。
   */
  kind_id: string;
  /** 新建菜单、会话标题、历史页 tab 上显示的名字，比如「Til」。 */
  label: string;
  /** 启动命令。空 = 底层 agent 的出厂命令；通用 ACP 必填。旧运行时不报，读成空。 */
  command?: string;
  /** 额外注入的环境变量，覆盖底层 agent 的同名变量。旧运行时不报，读成空。 */
  env?: { [key: string]: string };
  /**
   * 数据目录，允许 `~` 开头（展开逻辑见 `crate::workspace_override`）。空 = 不覆盖；
   * 只有底层 agent 有数据目录变量（如 `CLAUDE_CONFIG_DIR`）时才能填。
   */
  workspace_dir: string;
};
/**
 * Agent CLI 运行环境的只读诊断快照。探测在运行时那台机器上做（`agent.cli.inspect`），
 * 线上按各家的稳定 `id` 传；认不出的 id（对端版本更新）丢掉。
 */
export type AcpRuntimeDiagnostics = {
  terminal: { [key: string]: RuntimeExecutable };
  acp: { [key: string]: RuntimeExecutable };
};
/**
 * ACP 回合终结原因的稳定投影。`DaemonPhase::Idle` 只描述当前没有运行中的回合，
 * 不能同时承担“上一轮成功/取消/被拒绝”的结果语义；否则 daemon 只能把所有
 * `TurnEnded` 猜成成功。
 */
export type AcpTurnOutcome = "succeeded" | "max_tokens" | "max_turn_requests" | "refused" | "failed" | "cancelled" | "interrupted";
/** 同名几份里以哪份为准。`path` 是运行时那台机器上的路径，必须是这个技能的一份。 */
export type AdoptPluginParams = {
  id: string;
  path: string;
};
export type AgentCliInstallParams = {
  /** `ConversationAgentKind::id`。 */
  agent: string;
};
export type AgentCliInstallResult = {
  /** 用的包管理器（`brew` / `npm`）。 */
  installer: string;
  /** 装完重新探测的结果。 */
  diagnostics: AcpRuntimeDiagnostics;
};
export type AgentDefinition = {
  id: string;
  name: string;
  /** 旧版「简介」字段，仅兼容已有存档，界面不再展示或编辑。 */
  description: string;
  /** 执行该产品智能体的引擎类型。旧配置曾将它写成含义不清的 `agent_id`。 */
  engine_kind: string;
  /** 长期工作方式，启动时写入引擎 system prompt。 */
  prompt: string;
  /**
   * 这个智能体要加载的 Pi 插件 id（`skill:<名字>` / `extension:<名字>`）。
   * 空表示不加载任何插件：会话启动时会关掉 Pi 的自动发现，不做「空则全量」
   * 的回退，否则用户勾掉最后一个反而会拿回全量。
   */
  plugins: string[];
  /**
   * 这个智能体要连接的 MCP 服务器名（`~/.pi/agent/mcp.json` 里 `mcpServers` 的键）。
   * 空表示不连任何 MCP 服务器；和插件一样不做「空则全量」的回退，否则用户
   * 勾掉最后一个反而会连上全部服务器。
   */
  mcp_servers: string[];
  /**
   * 绑定到这个智能体的本地目录。会话的工作目录是智能体自己的 space，业务
   * 素材靠这里挂进来：Pi 只接受单个 cwd，没有 `--add-dir`，所以只能把绝对
   * 路径写进 system prompt，让它用 read/bash 去访问。
   */
  context_folders: string[];
  /** 绑定到这个智能体的参考链接，同样写进 system prompt。 */
  context_links: string[];
  /**
   * 这个智能体新对话和自动化用的 Pi provider。空 = 跟随 Pi 全局默认。
   * 对话里仍可再换模型，只影响这一段会话。
   */
  model_provider?: string;
  /** 对应的模型 ID。必须和 `model_provider` 成对出现，否则视为未设置。 */
  model_id?: string;
  /** 关掉后这个智能体不注入、不写入记忆，已有记忆文件保留。默认开。 */
  memory_enabled: boolean;
};
export type AgentDefinitionCreate = {
  id?: string | null;
  name?: string;
  prompt?: string;
  engine_kind_id?: string | null;
  plugins?: string[];
  mcp_servers?: string[];
  context_folders?: string[];
  context_links?: string[];
  model_provider?: string | null;
  model_id?: string | null;
  memory_enabled?: boolean | null;
};
export type AgentDefinitionGetResult = {
  agent: AgentDefinition;
};
export type AgentDefinitionIdParams = {
  id: string;
};
export type AgentDefinitionInput = {
  id: string;
  name: string;
  /** 旧版「简介」字段，仅兼容已有存档，界面不再展示或编辑。 */
  description?: string;
  /** 执行该产品智能体的引擎类型。旧配置曾将它写成含义不清的 `agent_id`。 */
  engine_kind: string;
  /** 长期工作方式，启动时写入引擎 system prompt。 */
  prompt: string;
  /**
   * 这个智能体要加载的 Pi 插件 id（`skill:<名字>` / `extension:<名字>`）。
   * 空表示不加载任何插件：会话启动时会关掉 Pi 的自动发现，不做「空则全量」
   * 的回退，否则用户勾掉最后一个反而会拿回全量。
   */
  plugins?: string[];
  /**
   * 这个智能体要连接的 MCP 服务器名（`~/.pi/agent/mcp.json` 里 `mcpServers` 的键）。
   * 空表示不连任何 MCP 服务器；和插件一样不做「空则全量」的回退，否则用户
   * 勾掉最后一个反而会连上全部服务器。
   */
  mcp_servers?: string[];
  /**
   * 绑定到这个智能体的本地目录。会话的工作目录是智能体自己的 space，业务
   * 素材靠这里挂进来：Pi 只接受单个 cwd，没有 `--add-dir`，所以只能把绝对
   * 路径写进 system prompt，让它用 read/bash 去访问。
   */
  context_folders?: string[];
  /** 绑定到这个智能体的参考链接，同样写进 system prompt。 */
  context_links?: string[];
  /**
   * 这个智能体新对话和自动化用的 Pi provider。空 = 跟随 Pi 全局默认。
   * 对话里仍可再换模型，只影响这一段会话。
   */
  model_provider?: string;
  /** 对应的模型 ID。必须和 `model_provider` 成对出现，否则视为未设置。 */
  model_id?: string;
  /** 关掉后这个智能体不注入、不写入记忆，已有记忆文件保留。默认开。 */
  memory_enabled?: boolean;
};
export type AgentDefinitionListResult = {
  agents: AgentDefinition[];
};
export type AgentDefinitionUpdateParams = {
  id: string;
  name?: string | null;
  prompt?: string | null;
  engine_kind_id?: string | null;
  plugins?: string[] | null;
  mcp_servers?: string[] | null;
  context_folders?: string[] | null;
  context_links?: string[] | null;
  model_provider?: string | null;
  model_id?: string | null;
  memory_enabled?: boolean | null;
};
export type AgentHooksStatus = {
  providers: HookProviderStatus[];
  /** hooks 调用的 `smelt-notify` 在运行时那台机器上的路径。 */
  notify_path: string;
};
/** 客户端一次修改：它看到的那份和它改成的那份。只有两者不同的字段会被写入。 */
export type AgentHostEdit = {
  before: AgentHostSettingsInput;
  after: AgentHostSettingsInput;
};
export type AgentHostSettings = {
  /** 是否由 Smelt 管理各 CLI agent 的结构化 hooks。 */
  hooks_enabled: boolean;
  /**
   * 已删掉的「按 agent 改启动命令」。控制协议 v3 的运行时要求配置里有这个字段，所以线上
   * 恒写空表（它按改前改后三方合并，两边都空就什么都不改），读到的丢弃。
   */
  commands: Record<string, never>;
  /** agent id → 额外注入的环境变量。 */
  env: { [key: string]: { [key: string]: string } };
  /** agent id → 用户最近显式选择的配置 `(config_id, value_id)`。 */
  config_memory: { [key: string]: [string, string][] };
  /** 用户添加的自定义 Agent。 */
  profiles: AcpProfile[];
  agents: AgentDefinition[];
  /** 侧栏「新对话」默认用哪个智能体。指向这台运行时上的定义，所以跟着运行时走。 */
  default_conversation_agent: string | null;
  /**
   * 这台运行时上智能体 space 的根（`~/.smelt/agents`，按那台机器的路径写）。运行时读出
   * 配置时填上，不存库、不经编辑合并；旧运行时不报，那时认不出哪个目录是智能体的 space。
   */
  agent_spaces_root?: string | null;
};
export type AgentHostSettingsInput = {
  /** 是否由 Smelt 管理各 CLI agent 的结构化 hooks。 */
  hooks_enabled: boolean;
  /** agent id → 额外注入的环境变量。 */
  env: { [key: string]: { [key: string]: string } };
  /** agent id → 用户最近显式选择的配置 `(config_id, value_id)`。 */
  config_memory: { [key: string]: [string, string][] };
  /** 用户添加的自定义 Agent。 */
  profiles: AcpProfileInput[];
  agents: AgentDefinitionInput[];
  /** 侧栏「新对话」默认用哪个智能体。指向这台运行时上的定义，所以跟着运行时走。 */
  default_conversation_agent?: string | null;
  /**
   * 这台运行时上智能体 space 的根（`~/.smelt/agents`，按那台机器的路径写）。运行时读出
   * 配置时填上，不存库、不经编辑合并；旧运行时不报，那时认不出哪个目录是智能体的 space。
   */
  agent_spaces_root?: string | null;
};
/**
 * 用户采纳一条待确认的记忆：之后照常注入和召回。
 * 设置页看的两层记忆：智能体自己的，和所有智能体共享的用户全局层。
 */
export type AgentMemories = {
  agent: MemoryListing;
  user: MemoryListing;
};
export type AgentMemoryDirParams = {
  agent_id: string;
  layer: AgentMemoryLayer;
};
/** OpenAI 兼容的 `/embeddings` 服务。只发给通过认证的会话。 */
export type AgentMemoryEmbedding = {
  base_url: string;
  model: string;
  api_key?: string;
};
export type AgentMemoryFileParams = {
  agent_id: string;
  layer: AgentMemoryLayer;
  file_name: string;
};
export type AgentMemoryLayer = "agent" | "user";
export type AgentMemoryLayerMeta = {
  usage: AgentMemoryUsageEntry[];
  /** 待确认的文件名。 */
  pending: string[];
};
/** 会话没有某一层记忆（普通会话）时，那一层为空。 */
export type AgentMemoryMetaResult = {
  /** 智能体当前是否开着记忆。会话启动后用户可能关掉，关掉后不注入、不写入。 */
  enabled: boolean;
  agent: AgentMemoryLayerMeta;
  user: AgentMemoryLayerMeta;
  /** 用户配置的向量检索服务；没配为空，Pi 退回关键词 + 模型重排。 */
  embedding?: AgentMemoryEmbedding | null;
  /** 用户指定的重排模型；没指定为空，Pi 自动挑。 */
  rerank_model?: AgentMemoryRerankModel | null;
};
export type AgentMemoryRecordReadParams = {
  session_id: string;
  token: string;
  layer: AgentMemoryLayer;
  file: string;
};
export type AgentMemoryRecordWriteParams = {
  session_id: string;
  token: string;
  layer: AgentMemoryLayer;
  file: string;
  trust: AgentMemoryTrust;
  reason: string;
  /** 客户端声称这次覆盖了已有文件。服务端仍会查库，不能只信这个字段。 */
  overwrote_existing?: boolean;
};
export type AgentMemoryRerankModel = {
  provider: string;
  id: string;
};
export type AgentMemorySessionParams = {
  session_id: string;
  token: string;
};
export type AgentMemoryTrust = "trusted" | "pending";
export type AgentMemoryUsageEntry = {
  file: string;
  read_count: number;
  last_read_at_ms: number;
};
export type AgentSpaceResult = {
  path: string;
};
/** 一条开给命令行调用的 App 命令。 */
export type AppCommandInfo = {
  /** `<来源>/<名字>`。 */
  id: string;
  /** 给人看的名字，按 App 当前的界面语言。 */
  title: string;
  /** 来源给人看的名字。 */
  group: string;
  /** 认的参数字段（参数对象顶层的键），空表是不带参数。 */
  args: string[];
};
export type AppCommandListResult = {
  commands: AppCommandInfo[];
};
export type AppCommandRunParams = {
  command: string;
  /** 参数对象；不带参数是 `null`。 */
  args?: unknown;
};
export type AppCommandRunResult = {
  /** 命令交回的值，没有就是 `null`。 */
  result: unknown;
};
export type AskAnswer = {
  question: string;
  /** 单选一项，多选若干项，自己写的答案也在里面。 */
  answer: string[];
};
/**
 * Pi 对话里的一次 `smelt_ask_user`：问了哪些题、人怎么答的。工具还在跑（卡片等人答）时
 * `answers` 为空。
 */
export type AskRecord = {
  /** 每道题和答案，按提问顺序。跳过时也列出题面，答案为空。 */
  answers: AskAnswer[];
  /** 人跳过了，没有作答。 */
  skipped: boolean;
};
export type AttentionMarkReadParams = {
  session_id: string;
};
export type Automation = {
  id: string;
  name: string;
  enabled: boolean;
  /**
   * 在哪个目录里跑。用户选的原样用；没选（`None`）或是托管目录（见
   * [`is_managed_workspace`]）时由运行时按 id 分配，换了智能体也跟着换。
   */
  workspace_dir?: string | null;
  trigger: AutomationTrigger;
  action: AutomationAction;
  sinks: AutomationSink[];
};
export type AutomationAction = {
  type: "agent";
  agent_definition_id: string;
  prompt?: string | null;
} | {
  type: "shell";
  command: string;
  args: string[];
};
export type AutomationActionInput = {
  type: "agent";
  agent_definition_id: string;
  prompt?: string | null;
} | {
  type: "shell";
  command: string;
  args?: string[];
};
export type AutomationCommandResult = {
  result: "applied";
  value: boolean;
} | {
  result: "run";
  value: AutomationRun;
};
export type AutomationGetResult = {
  automation: Automation;
};
export type AutomationInput = {
  id: string;
  name: string;
  enabled: boolean;
  /**
   * 在哪个目录里跑。用户选的原样用；没选（`None`）或是托管目录（见
   * [`is_managed_workspace`]）时由运行时按 id 分配，换了智能体也跟着换。
   */
  workspace_dir?: string | null;
  trigger: AutomationTriggerInput;
  action: AutomationActionInput;
  sinks: AutomationSink[];
};
export type AutomationListResult = {
  automations: Automation[];
};
export type AutomationRun = {
  id: string;
  automation_id: string;
  context: AutomationRunContext;
  source: AutomationRunSource;
  scheduled_for: number | null;
  status: AutomationRunStatus;
  created_at: number;
  started_at: number | null;
  delivery_attempt_at: number | null;
  delivery_attempts: number;
  finished_at: number | null;
  session_id: string | null;
  provider_session_id: string | null;
  output: string | null;
  /** 没成的原因种类，客户端按语言说；运行时记的旧记录没有。 */
  error_code?: AutomationRunFailure | null;
  /** 有种类时是底层报错的细节；没有种类时是整句（旧记录，或成功了但 stderr 有输出）。 */
  error: string | null;
  runtime_released_at: number | null;
};
export type AutomationRunContext = {
  automation_name: string;
  action: AutomationAction;
  cwd: string;
  trigger_payload?: unknown;
  prompt?: string | null;
  agent_definition_name?: string | null;
  engine_kind_id?: string | null;
  agent_instructions?: string | null;
};
export type AutomationRunDetailParams = {
  run_id: string;
  /** 客户端手上的运行缺输出或指令时才要归档那份。 */
  include_archived_run: boolean;
  /**
   * 客户端认得的消息流条目版本（[`crate::acp_chat::ACP_ENTRY_VERSION`]）。旧客户端不带，按 1
   * 算，比它新的条目换成它认得的形状。
   */
  entry_version?: number;
};
export type AutomationRunDetailResult = {
  archived_run: AutomationRun | null;
  transcript: AcpEntry[];
};
/**
 * 一次自动化运行没成的原因种类。运行记录里存这个种类（`error_code`），底层报错原样存在
 * `error` 里当细节；客户端用 [`AutomationRunFailure::describe`] 按自己的语言说。
 * 没有种类的旧记录只有 `error`，原样显示。
 */
export type AutomationRunFailure = "previous_run_active" | "nothing_to_send" | "workspace_not_ready" | "workspace_missing" | "automation_invalid" | "agent_unavailable" | "session_missing" | "session_lost_on_restart" | "session_conflict" | "restart_during_delivery" | "context_not_saved" | "engine_changed" | "start_failed" | "delivery_failed" | "exited_before_delivery" | "session_lost" | "agent_exited" | "turn_failed" | "turn_interrupted" | "turn_max_tokens" | "turn_max_turn_requests" | "turn_refused" | "turn_without_outcome" | "not_shell_action" | "command_start_failed" | "command_wait_failed" | "command_failed" | "other";
export type AutomationRunOnceResult = {
  result: AutomationCommandResult;
};
export type AutomationRunSource = "manual" | "scheduled" | "webhook" | "event";
export type AutomationRunStatus = "starting" | "queued" | "dispatching" | "running" | "awaiting_approval" | "waiting_for_user" | "completed" | "failed" | "cancelled" | "skipped";
export type AutomationSchedule = {
  type: "daily";
  hour: number;
  minute: number;
} | {
  type: "every_minutes";
  minutes: number;
} | {
  type: "every_hours";
  hours: number;
} | {
  type: "weekly";
  days: number;
  hour: number;
  minute: number;
} | {
  type: "during";
  minutes: number;
  days: number;
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
};
export type AutomationSetEnabledParams = {
  id: string;
  enabled: boolean;
};
export type AutomationSink = {
  type: "local";
  /**
   * 只在没跑成（失败、跳过）时弹。默认不写：线上还是 `{"type": "local"}`，不认识这个
   * 字段的旧版本读成「每次都弹」。
   */
  failures_only?: boolean;
} | {
  type: "off";
};
/** 一条自动化是一份时机列表：到点、间隔、HTTP、内部事件可混排，共享同一个忙时槽位。 */
export type AutomationTrigger = {
  schedules?: AutomationSchedule[];
  webhooks?: WebhookIngress[];
  events?: EventIngress[];
};
/** 一条自动化是一份时机列表：到点、间隔、HTTP、内部事件可混排，共享同一个忙时槽位。 */
export type AutomationTriggerInput = {
  schedules?: AutomationSchedule[];
  webhooks?: WebhookIngressInput[];
  events?: EventIngress[];
};
export type BranchList = {
  local: string[];
  remote: string[];
};
export type BrowseDirsParams = {
  /** 要列的目录，绝对路径。为空时列用户主目录。 */
  path?: string | null;
  /** 连文件一起列（导入单文件插件时要选文件）。 */
  include_files?: boolean;
};
export type BrowseDirsResult = {
  /** 实际列出的目录（规范化后的绝对路径）。 */
  path: string;
  /** 上一级；已经在根目录时为空。 */
  parent: string | null;
  /** 用户主目录，给「回到主目录」用。 */
  home: string;
  /** 目录在前，各自按名字排序。 */
  entries: BrowseEntry[];
};
export type BrowseEntry = {
  name: string;
  path: string;
  is_dir: boolean;
};
/** 一条提交记录。 */
export type CommitNode = {
  /** 完整 sha，父子关系全靠它对齐。 */
  hash: string;
  /** 短 sha，显示用。 */
  short: string;
  author: string;
  /** author date，Unix 秒。 */
  time: number;
  /** 提交标题（第一行）。 */
  subject: string;
  /** 父提交完整 sha。第一个是主线父，其余是被合并进来的。 */
  parents: string[];
  /** 指向本提交的引用名（分支 / tag），已剥掉 `HEAD -> ` 之类前缀。 */
  refs: string[];
};
export type CompleteFilesParams = {
  /** 会话的工作目录。 */
  cwd: string;
};
export type CompleteFilesResult = {
  /** 相对 `cwd` 的文件，以及它们的上级目录（目录以 `/` 结尾），已排序。 */
  entries: string[];
  /** 超过上限被截断。 */
  truncated: boolean;
};
export type ControlProtocolDescription = {
  current_version: number;
  min_supported_version: number;
  max_supported_version: number;
  /** 字符串而非枚举：旧客户端看到未来新增方法时仍能解析 describe 响应。 */
  methods: string[];
};
/**
 * 对话为什么结束了。会话快照里存这个种类（`end_code`），底层报错放在 `end_reason` 当细节；
 * 智能体自己报的错没有种类，`end_reason` 就是它的原话。
 */
export type ConversationEndReason = {
  kind: "connection_lost";
} | {
  kind: "reconnect_exhausted";
  attempts: number;
} | {
  kind: "reconnect_failed";
} | {
  kind: "previous_agent_still_running";
} | {
  kind: "driver_crashed";
} | {
  kind: "deleted";
} | {
  kind: "session_in_use";
} | {
  kind: "history_missing";
} | {
  kind: "restore_unsupported";
} | {
  kind: "restore_failed";
} | {
  kind: "invalid_open_request";
} | {
  kind: "archived";
} | {
  kind: "open_failed";
} | {
  kind: "sign_in_required";
  agent: SignInAgent;
} | {
  kind: "other";
};
export type ConversationImageParams = {
  session_id: string;
  /** 引用里的全局条目下标（`AcpImageRef::entry`）。 */
  entry: number;
  /** 引用里的原图摘要（`AcpImageRef::digest`）。 */
  digest: string;
  /**
   * 给了就出缩略图：等比缩到长边不超过它（不裁切），运行时会把它夹到 32..=2048。
   * 不给就出可看的原图：base64 不超过 1 MiB 原样返回，否则长边缩到 2560。
   */
  max_edge?: number | null;
};
export type ConversationImageResult = {
  mime: string;
  data_b64: string;
};
/**
 * ACP 启动规格：命令本体保留现有的空白分词语义，环境变量单独结构化存储，
 * 避免把带空格的值硬塞回 `VAR=value cmd` 这种不可靠的字符串拼接。
 */
export type ConversationLaunchSpec = {
  command: string;
  env: { [key: string]: string };
};
/**
 * ACP 启动规格：命令本体保留现有的空白分词语义，环境变量单独结构化存储，
 * 避免把带空格的值硬塞回 `VAR=value cmd` 这种不可靠的字符串拼接。
 */
export type ConversationLaunchSpecInput = {
  command: string;
  env?: { [key: string]: string };
};
export type ConversationPromptIndexParams = {
  session_id: string;
  /**
   * 只要这一条（全局条目下标）之前的提问。客户端已加载的那一截自己算，只问前面没加载的。
   * 不给就是整段。
   */
  before?: number | null;
};
export type ConversationPromptIndexResult = {
  prompts: PromptIndexItem[];
};
/** `conversation.prompt`：以一次外部投递的身份往对话里发一轮话。 */
export type ConversationPromptParams = {
  session_id: string;
  /**
   * 投递方给这一轮起的 id，在这段对话里唯一。同一个 id 再发不会再执行一次；这一轮结束后
   * 会话状态的 `completed_delivery_id` 就是它，结果用 `conversation.turn_result` 按它取。
   */
  delivery_id: string;
  text: string;
  /**
   * 投递方认定这段对话带着的工具限制（`session.create` 时给的）。必须和运行时现在的完全
   * 一样，否则回 `failed_precondition`：运行时重启后续上的对话不带限制，不能往里发无人
   * 值守的话。没要求限制就给空。
   */
  disallowed_tools?: string[];
};
export type ConversationTurnResultParams = {
  session_id: string;
  delivery_id: string;
};
/** 一次外部投递那一轮现在怎样。 */
export type ConversationTurnStatus = {
  status: "pending";
} | {
  status: "finished";
  outcome: AcpTurnOutcome;
  text: string;
} | {
  status: "unknown";
};
export type CopilotQuota = {
  unit: CopilotQuotaUnit;
  remaining: number;
  limit: number | null;
  used: number | null;
};
export type CopilotQuotaUnit = "AiCredits" | "PremiumRequests";
export type CreatedAgentConversation = {
  session_id: string;
  agent_definition_id: string;
  cwd: string;
};
/** 跟 smeltd 的 `Phase`（见 `smeltd::session_state`）对应，同样 `rename_all = "snake_case"`。 */
export type DaemonPhase = "thinking" | "executing_tool" | "awaiting_approval" | "waiting_for_user" | "succeeded" | "failed" | "idle" | "dead" | "connecting";
export type DaemonRuntimeInfo = {
  version: string;
  exe_mtime: number;
  exe: string | null;
  pid: number;
  started_at: number;
  session_count: number;
  agent_event_version: number;
  /**
   * 运行时第一次启动时生成、之后永久不变的身份。和传输无关：socket 路径、iroh
   * endpoint id 都只是「怎么到达它」，客户端按它区分运行时。
   *
   * `system.describe` 是冻结契约：客户端要先读懂旧运行时的回复，才能决定推送
   * 升级。所以新字段都是 `Option`，`None` 只表示运行时早于这个字段。
   */
  runtime_id?: string | null;
  /** `std::env::consts::OS`，如 `macos`、`linux`。 */
  os?: string | null;
  /** `std::env::consts::ARCH`，如 `aarch64`、`x86_64`。 */
  arch?: string | null;
  /** 运行时所在机器的主机名，客户端添加运行时时拿它当默认名字。 */
  hostname?: string | null;
  /**
   * 这台机器装没装远程桌面（`install.sh --with-desktop`）。装了，客户端才给「远程桌面」
   * 入口。
   */
  desktop?: boolean | null;
  /**
   * 运行时的 `SMELT_HOME`。智能体的 space 等托管目录都在它下面，客户端拿它认出哪些
   * 会话是智能体对话：用自己的主目录去比，别的机器上的永远对不上。
   */
  smelt_home?: string | null;
};
/**
 * GUI 侧订阅状态通道的镜像（serde 反序列化；多出来的 JSON 字段自动忽略）。
 * 字段对齐 smeltd `SessionState` 广播——B 路线「语义面板」的数据源。
 */
export type DaemonSessionState = {
  id: string;
  /** daemon runtime generation. A recreated session may restart its revision at a lower value. */
  generation: number;
  /** 当前 daemon 进程内的单调状态版本；同一订阅连接上只接受递增值。 */
  revision: number;
  phase: DaemonPhase;
  /** hook 上报的问句 / 当前工具名（PreToolUse 时 question 常是 tool_name）。 */
  pending_question: string | null;
  /** `pending_question` 里运行时自己说的那句的种类，给人看用 [`Self::pending_text`]。 */
  pending_note?: SessionNote | null;
  /** 对话会话正在等的那个审批或选择题的身份，变了就是换了一个请求。终端会话没有。 */
  pending_request: string | null;
  /**
   * 最近结束的那轮对应的外部投递（`conversation.prompt` 的 `delivery_id`）；用户自己说的
   * 那轮是 None。投递方看到自己的 id 就去 `conversation.turn_result` 取结果。
   */
  completed_delivery_id: string | null;
  title: string | null;
  /**
   * 这段对话的展示名，运行时算好（见 `SessionStateChanged::conversation_name`）。侧栏
   * 的对话行、菜单栏、资源页直接显示它，没有就显示通用名。
   */
  conversation_name: string | null;
  /** 这一格自己的展示名（分屏的子行、手机上的一格），运行时算好。 */
  name: string | null;
  /** 守护下发的启动命令；远程客户端在菜单快照缺少 agent 信息时用它兜底识别 provider。 */
  launch: string | null;
  /** 结构化 hook 实际检测到的 provider；明确的启动类型仍由 UI 优先采用。 */
  provider: string | null;
  /**
   * 该终端此刻正在对话的 provider 会话 id（由每条 hook 携带）。UI 用它把侧栏
   * 会话对上 provider 自己的历史存档：重命名写进历史标题覆盖层，显示名也跟着
   * 对话切换走。旧守护不报该字段时为 None，退化成纯终端标题语义。
   */
  conversation_id: string | null;
  /**
   * 用户给 `conversation_id` 那段对话起的名字（历史页、侧栏改名写的同一份），守护按
   * 身份查好推来。
   */
  custom_title: string | null;
  cwd: string | null;
  /** `cwd` 所在仓库的当前分支，运行时监听 HEAD 维护。不是 git 仓库、旧运行时不报是 None。 */
  branch: string | null;
  /** unix 秒，进入当前 phase 的时刻。 */
  phase_since: number;
  /**
   * unix 秒，这一轮从什么时候开始忙；不在一轮里（或运行时太旧不报）是 0。
   * 各端会话列表上的「本轮已运行」都按它算，一轮里换阶段不变。
   */
  turn_started_at: number;
  /** unix 秒，守护最后一次更新该会话状态的时刻。 */
  updated_at: number;
  /**
   * 该 pane 是否已经收到 hook/ACP 结构化事件。SessionStart 就会置位，用于
   * 判断低可信终端通知是否需要抑制。
   */
  structured_events: boolean;
  /**
   * 是否已收到回合级结构化事件（prompt / tool / 审批 / 终态）。SessionStart
   * 单独不会置位；旧守护缺字段时为 false，客户端必须对活动状态 fail closed。
   */
  turn_events: boolean;
  /**
   * 会话是否仍有客户端连接（list op 附带）。
   * 未连接不等于应删除：GUI 退出后终端与 ACP 会话可以继续由 daemon 托管。旧版
   * 守护不报该字段，反序列化默认 true，避免把未知状态误判为游离。
   */
  connected: boolean;
  /**
   * PTY `open` 的交互 attachment 数。ACP/旧版 daemon 没有该字段时为 0；结合
   * `connected` 与 watcher 数可区分“桌面渲染层在线”和“仅移动旁观层在线”。
   */
  interactive_connections: number;
  /** PTY `watch` 的只读订阅数；移动端终端画面目前使用这条输出通道。 */
  watcher_connections: number;
  /**
   * 本进程是否仍持有 PTY/ACP 运行时。与 `connected`（有没有客户端 attach）正交：
   * 崩溃恢复的幽灵会话 `runtime=false`，旧守护缺字段时默认 true，避免被误判成幽灵。
   */
  runtime: boolean;
};
export type DesktopTypeTextParams = {
  text: string;
};
export type DeviceIdParams = {
  endpoint_id: string;
};
export type DeviceInfo = {
  /** 设备的 iroh endpoint id，运行时按它认设备。 */
  endpoint_id: string;
  name: string;
  paired_at_ms: number;
  last_seen_at_ms: number | null;
  /** 现在有没有连着。 */
  connected: boolean;
};
export type DeviceList = {
  devices: DeviceInfo[];
};
export type DiffScope = "all" | "staged" | "unstaged";
export type DiscoverModelsParams = {
  base_url: string;
  api: string;
  /** 表单里现填的 key。空时用 `saved_provider_id` 那份已存的。 */
  api_key: string;
  /** 正在编辑的已有 provider：没现填 key 时用它已存的凭据，客户端看不到这份凭据。 */
  saved_provider_id?: string | null;
};
export type DiscoverModelsResult = {
  models: DiscoveredModel[];
};
/**
 * 端点报告的一个模型。
 *
 * Pi 的直连发现产出它：字段是配置里真正要落盘的那几项。
 */
export type DiscoveredModel = {
  id: string;
  name: string | null;
  contextWindow: number | null;
  maxTokens: number | null;
};
/** 一个已确认的仓库。 */
export type DiscoveredRepo = {
  /**
   * 仓库真实根（`git rev-parse --show-toplevel` 的结果，已 canonicalize）。
   * 运行时那台机器的路径，分隔符按那台机器，不用 `PathBuf`。
   */
  root: string;
  /** 相对项目根的路径。项目根自己是空串。仅用于展示与排序。 */
  rel_path: string;
  kind: RepoKind;
};
/** 发现结果。 */
export type Discovery = {
  /** 项目根排第一，其余按相对路径字典序。 */
  repos: DiscoveredRepo[];
  /** 是否因为 `max_repos` 截断过。UI 要据此提示用户，而不是假装列全了。 */
  truncated: boolean;
};
export type EmptyParams = { [key: string]: unknown };
export type EnsureConfigFileParams = {
  file: PiConfigFile;
};
export type EnsureConfigFileResult = {
  /** 运行时那台机器上的路径。 */
  path: string;
};
export type EventIngress = {
  topic: string;
  filter?: string | null;
};
/**
 * 事件订阅这一侧的版本。控制方法能按方法清单逐个看，订阅推来的投影不行：协议对不上，
 * 客户端订阅了也解不出来，只会断开再连。所以运行时在这里报出来，客户端订阅前先对。
 */
export type EventProtocolDescription = {
  /** `event_subscribe` 握手的事件总线协议版本。客户端只看它（见 `compat`）。 */
  protocol_version: number;
  /**
   * 以下三个冻结、不再检查，只给 v6 及更早的客户端判不兼容用（见
   * `smelt_event_protocol::CORE_PROJECTION_VERSION`）。缺了读成 0。
   */
  projection_version: number;
  session_projection_version: number;
  session_state_schema_version: number;
};
export type FileContent = {
  kind: "text";
  text: string;
  /** 内容版本，保存时原样带回来做冲突检测。客户端不解析它。 */
  version: string;
} | {
  kind: "image";
  data: string;
} | {
  kind: "binary";
} | {
  kind: "too_large";
  size: number;
  limit: number;
};
export type FileOpenResult = {
  content: FileContent;
};
export type FilePathParams = {
  /** 运行时那台机器上的绝对路径。 */
  path: string;
};
export type FileSaveParams = {
  path: string;
  text: string;
  /** 打开或上次保存时拿到的版本。`None` 表示用户已确认覆盖，跳过冲突检测。 */
  base_version?: string | null;
};
export type FileSaveResult = {
  /** 写入后的新版本。 */
  version: string;
};
export type GitAllDiffParams = {
  root: string;
  scope: DiffScope;
  /** 要拼进去的未跟踪路径。客户端从它手上的 status 里取，免得两次 status 对不上。 */
  untracked_paths: string[];
};
export type GitApplyPatchParams = {
  root: string;
  action: GitPatchAction;
  patch: string;
};
export type GitBranchDeleteParams = {
  root: string;
  branch: string;
  /** 远程分支（`origin/x`）走 `git push --delete`，本地分支走 `git branch -D`。 */
  remote: boolean;
};
export type GitBranchParams = {
  root: string;
  branch: string;
};
export type GitCommitFileParams = {
  root: string;
  hash: string;
  path: string;
};
export type GitCommitFilesResult = {
  files: [string, string][];
};
export type GitCommitParams = {
  root: string;
  message: string;
  push: boolean;
  /** 推送用的当前分支名；首次推送时据此设上游。 */
  branch: string;
};
export type GitCommitRefParams = {
  root: string;
  hash: string;
};
export type GitDiscardAllParams = {
  root: string;
  /** 用户看到的整份改动列表（[`GitStatusData::seen_changes`]）。 */
  seen: GitSeenChange[];
};
export type GitDiscardFileParams = {
  root: string;
  /** 未跟踪（`??`）的直接删盘，其余走 `git restore`。 */
  seen: GitSeenChange;
};
export type GitFileDiffParams = {
  root: string;
  path: string;
  untracked: boolean;
  scope: DiffScope;
};
export type GitFileDiffResult = {
  /** diff 原文。git 跑不起来时是一句说明，界面照样显示它。 */
  text: string;
  /** 这个文件是否有已暂存的部分。 */
  has_staged: boolean;
  /** 这是子模块指针的变化，不能按块操作。 */
  is_gitlink: boolean;
  /** 工作区里对应的普通文件（能在编辑器里打开）；目录、子模块、已删除时为 `None`。 */
  worktree_file: string | null;
};
export type GitLogParams = {
  root: string;
  scope: LogScope;
  limit: number;
};
export type GitLogResult = {
  commits: CommitNode[];
};
export type GitPatchAction = "stage" | "unstage" | "discard";
export type GitPathsParams = {
  root: string;
  paths: string[];
};
export type GitRepoInfoResult = {
  info: RepoInfo | null;
};
export type GitRootParams = {
  root: string;
};
/**
 * 用户发起丢弃时看到的一条改动。
 *
 * 丢弃不可撤销，而 agent 可能在同一个工作区里接着写：请求必须带上用户看到的这一份，
 * 运行时丢弃前从磁盘重算，任一字段对不上就拒绝，用户没见过的内容不跟着丢。
 */
export type GitSeenChange = {
  path: string;
  /** porcelain 的两位状态码。 */
  xy: string;
  /** 见 [`GitStatusData::worktree_hashes`]。 */
  worktree_hash?: string | null;
};
export type GitStatusData = {
  ok: boolean;
  branch: string;
  upstream: string | null;
  ahead: number;
  behind: number;
  files: [string, string][];
  /**
   * 改动文件工作区一侧的内容指纹（路径 → 十六进制 sha256），由运行时在拉 status 时
   * 从磁盘算出。没有可丢内容的路径（工作区已删除、目录、子模块）不在表里。
   */
  worktree_hashes: { [key: string]: string };
  stash_count: number;
  insertions: number;
  deletions: number;
};
export type GitTextResult = {
  text: string;
};
export type HistoryDeleteParams = {
  agent: string;
  profile_id?: string | null;
  /** 列表所在的项目目录。运行时据此重新列一遍，确认要删的存档确实在里面。 */
  cwd: string;
  resume_id: string;
  path: string;
};
export type HistoryDetail = {
  turns: HistoryTurn[];
  /** 原会话用的模型（读得到才有）。 */
  model: string | null;
};
export type HistoryDetailParams = {
  agent: string;
  path: string;
};
export type HistoryDetailResult = {
  detail: HistoryDetail | null;
};
export type HistoryListParams = {
  /** 历史来源的稳定标识（`HistorySourceKind::id`），如 `claude`、`antigravity`。 */
  agent: string;
  profile_id?: string | null;
  /** 项目目录。 */
  cwd: string;
};
export type HistoryListResult = {
  sessions: HistorySession[];
  /** resume_id → 这段会话所属的智能体定义 id。只有用产品智能体开的会话才有。 */
  agent_definition_ids: { [key: string]: string };
};
export type HistorySession = {
  /** 存档在运行时那台机器上的路径；查看和删除时原样带回来。 */
  path: string;
  /** agent 认得的 session id，续接时用它。 */
  resume_id: string;
  /** agent 自己生成的标题。 */
  title: string;
  /** 用户在 Smelt 里改的名字。 */
  custom_title: string | null;
  started_at_ms: number | null;
  last_active_at_ms: number | null;
  message_count: number;
  total_tokens: number;
};
export type HistoryTurn = {
  is_user: boolean;
  timestamp_ms: number | null;
  text: string;
  /** 这轮 assistant 调用的工具名。 */
  tools: string[];
  /** 这轮工具入参里出现的文件路径，会话迁移用。 */
  tool_paths: string[];
};
export type HookProviderStatus = {
  label: string;
  installed: boolean;
};
export type ImportPluginParams = {
  kind: PiPluginKind;
  /** 运行时那台机器上的路径。 */
  source: string;
};
/**
 * 项目行「+」下拉菜单里的一条可配置启动项：显示名 + shell 启动命令。
 * `provider` 标识这条启动项属于哪个 agent 协议族（如 "claude"/"codex"），
 * 供插件任务按 agent provider 匹配执行通道；用户自定义项可为空。
 */
export type LaunchEntry = {
  label: string;
  command: string;
  provider: string | null;
};
/**
 * 项目行「+」下拉菜单里的一条可配置启动项：显示名 + shell 启动命令。
 * `provider` 标识这条启动项属于哪个 agent 协议族（如 "claude"/"codex"），
 * 供插件任务按 agent provider 匹配执行通道；用户自定义项可为空。
 */
export type LaunchEntryInput = {
  label: string;
  command: string;
  provider?: string | null;
};
/** 客户端打开一段对话前，要运行时替它把启动规格补齐成什么样。 */
export type LaunchPrep = {
  /** 产品智能体的定义 id；裸引擎为空。 */
  agent_definition_id?: string | null;
  /**
   * 恢复存档里的会话：完好的快照原样沿用创建时的内容（见
   * [`repair_agent_definition_launch`]），只补记忆目录。否则按当前定义重新组装。
   * 带了 `restart` 时不看它。
   */
  restore?: boolean;
  /**
   * 用户点了「重新开始」：按运行时当前的设置和智能体定义重拼整份启动规格，传入的
   * 规格只作拼不出来时的退路。旧运行时不认识这一项，会按 `restore` 处理，所以重新开始
   * 时 `restore` 填 `true`：沿用原来的规格，不会拼出一份不对的。
   */
  restart?: LaunchRestart | null;
};
export type LaunchPrepareParams = {
  launch: ConversationLaunchSpecInput;
  prep: LaunchPrep;
  cwd?: string | null;
};
export type LaunchPrepareResult = {
  launch: ConversationLaunchSpec;
  /** 这场会话会加载的技能，供对话头部展示。 */
  skills: PiPlugin[];
  /** 这场会话会连的 MCP 服务器。旧运行时不回这一项，按没有处理。 */
  mcp_servers: PiMcpServer[];
};
export type LaunchReplaceParams = {
  /** 客户端看到的那份。 */
  base: LaunchEntryInput[];
  entries: LaunchEntryInput[];
};
/** 重新开始时运行时重拼启动规格要的会话身份。 */
export type LaunchRestart = {
  /** 执行引擎标识（`ConversationAgentKind::id()`）。运行时不认识就沿用原来的规格。 */
  engine_kind: string;
  /** 自定义 Agent 的 id。设置里已经删了就沿用原来的规格。 */
  profile_id?: string | null;
};
export type LaunchSettings = {
  version: number;
  /** 除固定的「新建终端」外，下拉菜单里的启动项。 */
  entries: LaunchEntry[];
};
export type LoadedSkillsParams = {
  launch: ConversationLaunchSpecInput;
  cwd?: string | null;
};
export type LoadedSkillsResult = {
  skills: PiPlugin[];
  /** 同 [`LaunchPrepareResult::mcp_servers`]。 */
  mcp_servers: PiMcpServer[];
};
/**
 * 日志看哪一段历史。
 *
 * 默认是 [`LogScope::Head`]——**当前分支**，跟 JetBrains 一致。一进来就 `--all`
 * 会把所有分支的提交混在一起按时间排，跟「我这条线上都干了啥」完全是两回事。
 */
export type LogScope = "head" | "all" | {
  branch: string;
};
export type LoginAnswerParams = {
  login_id: string;
  prompt_id: string;
  value: string;
};
export type LoginEventsParams = {
  login_id: string;
  /** 已经拿到的事件数；返回从这之后的。 */
  after: number;
};
export type LoginEventsResult = {
  events: PiLoginEvent[];
  /** 登录已经结束，这批之后不会再有事件。 */
  finished: boolean;
};
export type LoginIdParams = {
  login_id: string;
};
export type LoginStartParams = {
  provider_id: string;
  method: PiLoginMethod;
  /** 关掉助手的提问代答，每一问都交回给用户。 */
  ask_all: boolean;
};
export type McpEnabledParams = {
  name: string;
  enabled: boolean;
};
export type McpPresetParams = {
  id: string;
};
export type McpServerParams = {
  name: string;
};
export type MemoryEntry = {
  file_name: string;
  name: string;
  description: string;
  /** frontmatter 的 type；手改坏了就是 None，照样列出来让用户能删。 */
  kind: string | null;
  path: string;
  /** 模型 read 过这条记忆的次数。 */
  read_count: number;
  /** 最近一次被读或被修改的时间。 */
  last_used: SystemTime | null;
  /** 事实类且超过 [`MEMORY_STALE_AFTER`] 没用过。 */
  stale: boolean;
  /**
   * 待确认的原因：读过外部内容的那一轮或无人值守的运行写的。待确认的记忆不注入、不召回，
   * 用户采纳（[`approve_memory`]）后才生效。存的是种类（见 [`pending_reason_text`]），
   * 旧库里可能是写好的整句。
   */
  pending_reason: string | null;
  /** 去掉 frontmatter 的正文，卡片展开时看；frontmatter 坏了就是整个文件。 */
  body: string;
};
export type MemoryListing = {
  entries: MemoryEntry[];
  /** 已生效的规则正文超出常驻预算（通常是手改的），启动时只会注入一部分。 */
  core_over_budget: boolean;
};
export type MemoryTestParams = {
  base_url: string;
  model: string;
  /** `None` 表示用已存的密钥。客户端看不到已存的密钥本身。 */
  api_key?: string | null;
};
export type MemoryTestResult = {
  dimensions: number;
  elapsed_ms: number;
};
export type ModelsResult = {
  models: DiscoveredModel[];
};
/** 新建选择器里的一行。 */
export type NewSessionAction = {
  /** Pin 与跨端引用的稳定标识。终端项用 provider + 命令（改名后 Pin 仍有效）。 */
  key: string;
  /** 不含类型后缀的名称；渲染时统一显示为「名称 · 对话/终端」。 */
  label: string;
  kind: NewSessionActionKind;
  pinned: boolean;
} & ({
  target: "conversation";
  agentOptionId: string;
  /** 引擎 id（`claude`/`pi`…），供两端画图标。 */
  agentKind: string;
} | {
  target: "terminal";
  command: string;
  provider?: string | null;
} | {
  target: "blankTerminal";
});
export type NewSessionActionKind = "terminal" | "conversation";
export type NewSessionCatalogResult = {
  /**
   * 产品智能体在前，裸引擎与 profile 在后，与桌面新建菜单同序。不按探测结果过滤：
   * 历史列表等地方要靠它把 option id 还原成引擎和 profile。
   */
  agents: AcpAgentOption[];
  sections: NewSessionSections;
  /**
   * `agents` 里能机械执行工具限制的选项 id。`session.create`、`conversation.prompt` 带
   * `disallowed_tools` 时只能用这些选项，别的会被拒。启动规格不出线，调用方只能从这里知道。
   */
  restrictable_agent_ids: string[];
};
/**
 * 新建菜单的用户偏好单独存档，直接写入 SQLite；不读取旧 JSON，也不做旧格式迁移。
 * 也经控制接口回给客户端，所以宽容读取。
 */
export type NewSessionPreferences = {
  pinned: string[];
};
/** 三个分组。常用不额外占一层，和终端 / 对话同级。 */
export type NewSessionSections = {
  common: NewSessionAction[];
  terminal: NewSessionAction[];
  conversation: NewSessionAction[];
};
export type NotifySendParams = {
  title?: string | null;
  message: string;
};
export type NotifySendResult = {
  /** 有桌面客户端连着、收到了。 */
  desktop: boolean;
  /** 推给了几台手机。 */
  phones: number;
};
/** 新设备提交配对码。只有这个方法对未配对设备开放。 */
export type PairParams = {
  code: string;
  /** 设备名，显示在设备列表里。 */
  name: string;
};
/** 一次性配对串。用一次或过期就作废。 */
export type PairingOffer = {
  pairing_uri: string;
  expires_at_ms: number;
};
/** 另一个 Pi 对话发来的消息是哪一类。只是给对方看的标签，不改变投递方式。 */
export type PeerMessageKind = "message" | "task" | "completion" | "bounce";
/** 发来这条消息的对话。 */
export type PeerNote = {
  sourceSessionId: string;
  sourceTitle: string;
  kind: PeerMessageKind;
};
/** Pi 对话里的一次 `session_send`：发给谁、发的是哪类、送出去没有。 */
export type PeerSend = {
  toId: string;
  /** 门卫回的对方标题。还没回（工具在跑）或被拒时为空，界面用 id。 */
  toTitle: string;
  kind: PeerMessageKind;
  /** 发出去的正文（工具参数里的 `text`）。 */
  text: string;
  /** 门卫分的 `pm-` id。没送出去时为空。 */
  messageId: string | null;
  /** 被拒或出错时的原因，给人看的一句话。 */
  error: string | null;
  /** `session_spawn`：对方是这次新起的对话，`text` 是交代的任务。 */
  spawned: boolean;
};
export type PiAuthLink = {
  url: string;
  label: string | null;
};
/** 一个内置 provider 的登录能力与现状。 */
export type PiAuthProvider = {
  id: string;
  name: string;
  /** OAuth 那条路的按钮文案，例如「Sign in with Kimi Code」。 */
  oauthName: string | null;
  /** API key 那条路的名字，例如「Anthropic API key」。 */
  apiKeyName: string | null;
  /** OAuth 走的是订阅（Claude Pro/Max、Copilot 这类），不是按量计费的 key。 */
  subscription: boolean;
  supportsOauth: boolean;
  supportsApiKey: boolean;
  status: PiAuthStatus;
  /** 凭据从哪儿来，例如 "OAuth"、"ANTHROPIC_API_KEY"、"~/.aws/credentials"。 */
  source: string | null;
};
/** 一个 provider 当前的凭据状态。 */
export type PiAuthStatus = "none" | "oauth" | "api_key";
/** 能力库里 Pi 相关的全部展示数据。读不出的部分各自带错误，不连累其它部分。 */
export type PiConfig = {
  model_settings: Result_of_PiModelSettings_or_string;
  settings_summary: Result_of_PiSettingsSummary_or_string;
  mcp_servers: Result_of_Array_of_PiMcpServer_or_string;
  plugins: PiPlugin[];
  /** 智能体定义页、记忆重排模型下拉用的可选模型。 */
  model_choice_groups: PiModelChoiceGroup[];
  /** 模型目录由 Smelt 照端点维护的自定义 provider。 */
  auto_provider_ids: string[];
  paths: PiConfigPaths;
};
export type PiConfigFile = "settings" | "models" | "credentials" | "mcp";
/** 各配置文件在运行时那台机器上的路径，给人看。 */
export type PiConfigPaths = {
  settings: string;
  models: string;
  auth: string;
  mcp: string;
};
export type PiCustomModel = {
  id: string;
  name: string;
  contextWindow: number | null;
  maxTokens: number | null;
};
export type PiCustomModelInput = {
  id: string;
  name: string;
  contextWindow?: number | null;
  maxTokens?: number | null;
};
export type PiCustomProvider = {
  id: string;
  displayName: string;
  api: string;
  baseUrl: string;
  credentialConfigured: boolean;
  models: PiCustomModel[];
};
export type PiCustomProviderInput = {
  id: string;
  displayName: string;
  api: string;
  baseUrl?: string;
  credentialConfigured?: boolean;
  models?: PiCustomModelInput[];
};
export type PiDefaultModelConfig = {
  provider: string;
  model: string;
  baseUrl: string;
  thinkingLevel: string;
};
export type PiDefaultModelConfigInput = {
  provider: string;
  model: string;
  baseUrl?: string;
  thinkingLevel?: string;
};
export type PiDefaultModelSummary = {
  provider: string | null;
  model: string | null;
  thinking_level: string | null;
};
/** 登录流程推给宿主的一步。 */
export type PiLoginEvent = {
  event: "providers";
  providers: PiAuthProvider[];
} | {
  event: "models";
  models: DiscoveredModel[];
} | {
  event: "auth_url";
  url: string;
  instructions: string | null;
  note?: PiLoginNote | null;
} | {
  event: "device_code";
  userCode: string;
  verificationUri: string;
  intervalSeconds: number | null;
  expiresInSeconds: number | null;
} | {
  event: "info";
  message: string;
  links: PiAuthLink[];
} | {
  event: "progress";
  message: string;
  note?: PiLoginNote | null;
} | {
  event: "prompt";
  id: string;
  promptType: PiPromptKind;
  message: string;
  placeholder: string | null;
  options: PiPromptOption[];
  note?: PiLoginNote | null;
} | {
  event: "prompt_done";
  id: string;
} | {
  event: "done";
} | {
  event: "error";
  message: string;
  note?: PiLoginNote | null;
} | {
  event: "unknown";
};
/** 登录方式。`Models.login` 按它选 `provider.auth.oauth` 还是 `auth.apiKey`。 */
export type PiLoginMethod = "oauth" | "api_key";
/**
 * 登录助手自己说的话（MCP 登录的提示、替用户代答的说明）的种类，客户端按自己的语言说。
 * 同一事件的 `message` / `instructions` 是助手按英文说的同一句，给不认识种类的客户端兜底。
 * Pi 和 provider 自己的话没有种类，原样显示。
 */
export type PiLoginNote = {
  kind: "paste_callback";
} | {
  kind: "sign_in_in_browser";
} | {
  kind: "callback_received";
} | {
  kind: "callback_rejected";
  status: number;
} | {
  kind: "callback_failed";
  error: string;
} | {
  kind: "no_redirect";
} | {
  kind: "not_full_address";
} | {
  kind: "wrong_callback";
  expected: string;
} | {
  kind: "no_authorization_result";
} | {
  kind: "login_failed";
  code: number;
} | {
  kind: "answered_github_dot_com";
  question: string;
} | {
  kind: "answered_browser";
  question: string;
} | {
  kind: "helper_exited";
  detail: string | null;
} | {
  kind: "other";
};
export type PiMcpServer = {
  name: string;
  transport: PiMcpTransport;
  enabled: boolean;
  description: string | null;
  /**
   * `mcp-auth.json` 里有这台服务器的令牌。只用于显示：令牌可能已经过期，真正能不能连
   * 以会话里的连接结果为准，「登录」本身也会先试一次连接。
   */
  signed_in: boolean;
};
/**
 * 给人看的连接方式：运行时读 `mcp.json` 时已用 [`crate::redact`] 打过码（地址里的
 * `?api_key=…`、命令行里的 `--token xxx` 之类），原样显示即可，不能拿来连接。
 */
export type PiMcpTransport = {
  type: "http";
  url: string;
} | {
  type: "stdio";
  command: string;
  args: string[];
} | {
  type: "unknown";
};
/** 智能体定义页下拉用的一条模型。不依赖正在跑的 Pi 会话。 */
export type PiModelChoice = {
  provider: string;
  provider_name: string;
  id: string;
  name: string;
};
/** 下拉里按 provider 分组，避免把所有模型摊成一条长名单。 */
export type PiModelChoiceGroup = {
  provider: string;
  provider_name: string;
  models: PiModelChoice[];
};
export type PiModelSettings = {
  defaultModel: PiDefaultModelConfig;
  credentialConfigured: boolean;
  customProviders: PiCustomProvider[];
};
/** 一个可勾选的插件。`id` 是写进智能体配置的稳定标识。 */
export type PiPlugin = {
  id: string;
  kind: PiPluginKind;
  /** 展示名。skill 取 frontmatter 的 `name`，缺失时回退目录名。 */
  name: string;
  description: string;
  /** 传给 Pi 的路径，也是删除时要动的路径。 */
  path: string;
  /** 发现来源，用于在界面上说明这个插件是哪来的。 */
  origin: string;
  /** 失效原因。软链断了之类的坏条目要列出来让用户能删，而不是静默藏起来。 */
  broken: string | null;
  /** 随 smelt-agent 发布的内置技能。跟着运行时更新，不能删。 */
  builtin: boolean;
  /**
   * 技能放在旧位置（`~/.pi/agent/skills`、`~/.smelt/skills`）：只有这里的智能体用得上，
   * Claude Code 等别的 agent 读不到。可以移到通用目录 `~/.agents/skills`。
   */
  legacy_location?: boolean;
  /** 同名的另几份（各自独立的副本，不是指向它的链接）。用哪份要用户挑，其余进回收目录。 */
  duplicates?: string[];
};
/** 插件类别。决定注入时用哪个 Pi 启动参数。 */
export type PiPluginKind = "skill" | "extension";
export type PiPromptKind = "text" | "secret" | "select" | "manual_code";
export type PiPromptOption = {
  id: string;
  label: string;
  description: string | null;
};
/** 修改 Pi 配置的方法都返回改完之后的完整快照，客户端整份换掉缓存。 */
export type PiSaveCustomProviderResult = {
  /** 模型目录是否由 Smelt 照端点维护。 */
  auto: boolean;
  config: PiConfig;
};
export type PiSettingsSummary = {
  path: string;
  default_model: PiDefaultModelSummary | null;
};
export type PinParams = {
  key: string;
};
export type PluginIdParams = {
  id: string;
};
/** 会话进程树里的一个进程。 */
export type ProcessEntry = {
  pid: number;
  /** 进程名（`node`、`cargo`……）。 */
  name: string;
  /** 命令行，参数用空格连起来，过长截断；读不到是空的。 */
  command: string;
  /** 只算这一个进程，`process_count` 恒为 1。 */
  usage: ProcessUsage;
};
/** 一个进程或一棵进程树的资源占用。 */
export type ProcessUsage = {
  /** 两次采样之间的平均 CPU，按单核计，多核可以超过 100。 */
  cpu_percent: number;
  /** macOS 是 `phys_footprint`（活动监视器的「内存」），其它平台是常驻集。 */
  memory_bytes: number;
  process_count: number;
};
export type ProjectDirParams = {
  /** 项目根，绝对路径。 */
  root: string;
  /** 要列的目录，绝对路径，必须在 `root` 之内（可以就是 `root`）。 */
  dir: string;
};
export type ProjectEntry = {
  name: string;
  is_dir: boolean;
};
export type ProjectPathParams = {
  root: string;
  /** 绝对路径，必须在 `root` 之内，且不能是 `root` 本身。 */
  path: string;
};
export type ProjectRootParams = {
  /** 项目根，绝对路径。 */
  root: string;
};
export type ProjectSearchHit = {
  /** 命中文件的绝对路径。 */
  path: string;
  /** 相对项目根的展示路径。 */
  rel: string;
  /** 内容命中时的首个匹配行：(行号从 1 起, 该行预览)；文件名命中为 `None`。 */
  line: [number, string] | null;
};
export type ProjectSearchParams = {
  root: string;
  /** 大小写不敏感。 */
  query: string;
};
export type ProjectSearchResult = {
  /** 文件名命中在前、内容命中在后。 */
  hits: ProjectSearchHit[];
  /** 命中数触顶后停止，后面还有。 */
  truncated: boolean;
};
export type ProjectTreeResult = {
  entries: ProjectEntry[];
};
/**
 * 提问导航里的一问：提问压成一行，加上这一问最后一段回答的开头。对话页左侧的导航条
 * 用它画刻度和悬停预览；没加载到客户端的那一截由运行时按同一个函数算好发过来。
 */
export type PromptIndexItem = {
  /** 提问在整段对话里的条目序号（不是客户端已加载那一截里的下标）。 */
  entry_ix: number;
  /** 提问的字，换行和连续空白压成一个空格，过长截断。 */
  prompt: string;
  /** 提问带了几张图。 */
  images: number;
  /** 这一问最后一段回答的开头，同样压成一行；还没回答时为空。 */
  answer: string;
};
export type ProviderIdParams = {
  provider_id: string;
};
export type ProviderQuota = {
  windows: ProviderQuotaWindow[];
};
/**
 * 一家 provider 的额度数据。两种模型互不相容——窗口型按时间窗给百分比，
 * Copilot 按本周期剩余额度计数——所以不强行合并成一个结构，由展示层归一。
 */
export type ProviderQuotaData = {
  windows: ProviderQuota;
} | {
  copilot: CopilotQuota;
};
export type ProviderQuotaWindow = {
  /** 窗口时长，例如 `5h`、`7d`；服务端没给时长时是种类，显示用 [`Self::display_label`]。 */
  label: string;
  /** 当前窗口已使用百分比。 */
  used_percent: number;
  /** 服务端给出的绝对重置时间（Unix 毫秒）。 */
  resets_at_ms: number | null;
  /** 没有绝对时间时保留相对重置秒数。 */
  reset_after_seconds: number | null;
};
export type ProvidersResult = {
  providers: PiAuthProvider[];
};
/** `push.register` 的参数。哪台设备由连接的身份决定，不在参数里。 */
export type PushRegisterParams = {
  /** 推送中继发的句柄（加密过的 APNs 令牌）。运行时只转交，解不开。 */
  handle: string;
  /** 推送中继的地址，`https://` 开头。手机编译时定好，运行时往这里发。 */
  relay_url: string;
  approval: boolean;
  input: boolean;
  failure: boolean;
  /** 跑完一轮。旧版手机没有这个开关，不带就是不推。 */
  success?: boolean;
  /** 见 [`PUSH_PAYLOAD_VERSION`]。旧版手机不带，就是 1。 */
  payload_version?: number;
  /** 智能体主动发的消息。正文原样显示，哪一版 App 都认，旧版手机不带就是推。 */
  message?: boolean;
  /** 手机的界面语言（如 `zh-CN`），推送里运行时自己说的话按它说。旧版手机不带，按英文说。 */
  locale?: string | null;
};
/** 设置页的一行。 */
export type QuotaProviderSetting = {
  target: string;
  enabled: boolean;
  /** 钥匙串访问被拒绝，后台不再读。手动刷新这一家后解除。 */
  keychain_denied: boolean;
};
export type QuotaRefreshParams = {
  /** `None`：后台刷新全部账户；否则是用户点了这一个，可以弹钥匙串窗口。 */
  target?: string | null;
};
export type QuotaRefreshResult = {
  /** 开着查询的全部账户。全量刷新时客户端据此更新展示顺序（新登录的 Pi provider 由此出现）。 */
  targets: string[];
  /** 这一轮实际查到的账户的结果。 */
  statuses: [string, QuotaStatus][];
};
export type QuotaSetEnabledParams = {
  target: string;
  enabled: boolean;
};
/** 全部账户的额度开关，含关掉的。 */
export type QuotaSettings = {
  providers: QuotaProviderSetting[];
};
/** 客户端启动时要的：开着查询的全部账户（决定展示顺序）和缓存里还不旧的结果。 */
export type QuotaSnapshot = {
  targets: string[];
  statuses: [string, QuotaStatus][];
};
export type QuotaStatus = {
  quota: ProviderQuotaData | null;
  checked_at_ms: number;
  error: string | null;
};
/** 远程访问现在的样子。 */
export type RemoteAccessState = {
  state: "off";
} | {
  state: "starting";
} | {
  state: "running";
  endpoint_id: string;
  /** 自建 relay；`None` 表示用公共 relay。 */
  relay: string | null;
} | {
  state: "failed";
  error: string;
};
/** 远程访问现在的样子。 */
export type RemoteAccessStateInput = {
  state: "off";
} | {
  state: "starting";
} | {
  state: "running";
  endpoint_id: string;
  /** 自建 relay；`None` 表示用公共 relay。 */
  relay?: string | null;
} | {
  state: "failed";
  error: string;
};
/** 远程访问配置。运行时收到新配置就照着开关 iroh endpoint，守护重启后也按它自愈。 */
export type RemoteConfig = {
  enabled: boolean;
  /** 自建 iroh relay。留空就用 n0 的公共 relay 和 DNS 地址发现。 */
  iroh_relay: string;
};
/** 远程访问配置。运行时收到新配置就照着开关 iroh endpoint，守护重启后也按它自愈。 */
export type RemoteConfigInput = {
  enabled?: boolean;
  /** 自建 iroh relay。留空就用 n0 的公共 relay 和 DNS 地址发现。 */
  iroh_relay?: string;
};
/** `remote.status.wait` 的参数：调用方手里的状态。 */
export type RemoteStatusWaitParams = {
  known: RemoteAccessStateInput;
};
export type RepoInfo = {
  /** 当前分支；detached 时是 `HEAD`。 */
  branch: string;
  /** 当前分支推送去的远端的 URL：有 upstream 用它的远端，没有就用 `origin`，再没有用第一个远端。 */
  remote_url?: string | null;
  /** 那个远端的默认分支（`refs/remotes/<远端>/HEAD` 指向的分支名）。 */
  default_branch?: string | null;
  /** 当前分支 upstream 在远端上的分支名（不带远端前缀）。没设 upstream 时为空。 */
  upstream_branch?: string | null;
};
/**
 * 仓库在工作区里的身份。UI 用它区分图标与分组标题，写操作不看它——
 * 任何 kind 的仓库都是独立的提交单位。
 */
export type RepoKind = "root" | "submodule" | "nested";
/** 这台运行时此刻的资源占用。 */
export type ResourceUsageResult = {
  /** smeltd 进程本身，不含子进程。 */
  daemon: ProcessUsage;
  /** 根进程还在的会话，按 id 排序。 */
  sessions: SessionResourceUsage[];
  /** 这台机器的物理内存总量；读不到是 0。 */
  total_memory_bytes: number;
};
export type Result_of_Array_of_PiMcpServer_or_string = {
  Ok: PiMcpServer[];
} | {
  Err: string;
};
export type Result_of_PiModelSettings_or_string = {
  Ok: PiModelSettings;
} | {
  Err: string;
};
export type Result_of_PiSettingsSummary_or_string = {
  Ok: PiSettingsSummary;
} | {
  Err: string;
};
export type RevokeResult = {
  /** 是否真有这台设备。 */
  revoked: boolean;
};
/**
 * 一个 CLI 或运行时可执行文件的本机诊断结果。
 *
 * `path` 为 `None` 代表 Smelt 无法提供该运行时；找到但 `--version` 失败时仍
 * 保留路径，并将原因写入 `error`，避免把损坏的 shim 误报成「未安装」。对于
 * 首次使用才物化的内置运行时，`path` 是它将落盘的目标入口。
 */
export type RuntimeExecutable = {
  program: string;
  path: string | null;
  version: string | null;
  error: string | null;
};
export type SaveCustomProviderParams = {
  /** 编辑已有 provider 时它原来的 id（可能改名）。 */
  previous_id?: string | null;
  /** `models` 为空表示「照端点上的来」：先问端点，问到了才落盘，并标成自动维护。 */
  provider: PiCustomProviderInput;
  /** 新的 API key；`None` 表示保持已存的那份。 */
  api_key?: string | null;
  set_default: boolean;
};
export type SaveDefaultModelParams = {
  config: PiDefaultModelConfigInput;
  /** 新的 API key；`None` 表示保持已存的那份。 */
  api_key?: string | null;
  /**
   * 手动填写表单里地址留空 = 用官方端点：清掉以前写进 `models.json` 的地址覆盖。
   * 从菜单里挑模型时不能清，自定义 provider 的地址就存在那里。
   */
  reset_base_url?: boolean;
};
export type SessionAgentBindingsResult = {
  /** 会话 id → 智能体定义 id。 */
  bindings: { [key: string]: string };
};
export type SessionCreateKind = "acp" | "terminal";
export type SessionCreateParams = {
  /**
   * 必须是工作区菜单里的项目。对话可以留空：给了 `cwd` 就开在那里；选项属于产品智能体时
   * 开在智能体自己的 space；否则报错。
   */
  project_root?: string;
  /**
   * 对话开在这个目录（绝对路径，必须已存在），不经项目菜单：插件自己管的工作树就开在
   * 这里。和 `project_root` 二选一，终端不认。落在某个工作树的任务目录里时照样绑到那棵树。
   */
  cwd?: string | null;
  /** 对话的名字，记成用户起的名字。只对对话有效。 */
  title?: string | null;
  /**
   * agent 不许用的工具，Claude 权限规则的写法（`Edit`、`Bash(git push:*)`），由 agent 自己的
   * 权限引擎执行，任何权限模式下都生效。只有 Claude 支持，别的 agent 给了就报错。限制只属于
   * 这次拉起的运行时，不落盘：运行时重启后续上的对话不带限制（见 `conversation.prompt`）。
   */
  disallowed_tools?: string[];
  /**
   * 开在项目里的哪棵任务工作树（目录里 `worksets[].id`，必须属于 `project_root`）。
   * 给了就开在它的任务目录（`worktrees/<任务>`），会话绑到这棵树上。
   */
  workset_id?: string | null;
  /** 对话用哪个 agent 选项（`new_session.catalog` 的 `agents[].id`）。 */
  agent_option_id?: string | null;
  /**
   * 新建目录里那一行的 key。给了它就由运行时决定开对话还是哪种终端，
   * `kind` / `agent_option_id` 不再看。
   */
  launch_key?: string | null;
  /** 续接哪段历史（只对对话有效）。 */
  resume_id?: string | null;
  /** 不给就是对话。 */
  kind?: SessionCreateKind | null;
  /**
   * 客户端为一次新建生成、重试时复用的 UUID。会话 id 由它确定，重试落到同一个会话；
   * 同一个 `request_id` 带着不同参数再来回 `conflict`。
   */
  request_id?: string | null;
};
export type SessionCreateResult = {
  session_id: string;
};
export type SessionIdParams = {
  session_id: string;
};
export type SessionListResult = {
  sessions: string[];
};
/**
 * 会话列表上「待处理」那一行里运行时自己说的话。智能体的原话（审批问题、最后一句回复）
 * 不在这里，还在 `pending_question`。
 */
export type SessionNote = {
  kind: "turn_failed";
  outcome: TurnFailure;
} | {
  kind: "ended";
  reason: ConversationEndReason;
  detail?: string;
} | {
  kind: "other";
};
export type SessionResourceUsage = {
  session_id: string;
  /** 会话根进程的整棵树：终端是 shell 和它起的一切，对话是会话宿主、provider 和工具进程。 */
  usage: ProcessUsage;
  /** 树里的进程，内存大的在前，最多 [`SESSION_PROCESS_LIMIT`] 个；`usage` 仍是整棵树。 */
  processes: ProcessEntry[];
};
export type SettingsPatch = {
  settings?: { [key: string]: unknown };
};
export type SettingsResult = {
  settings: { [key: string]: unknown };
};
/** 要求先登录的是哪家智能体，决定怎么教人登录。只写确认过登录方式的几家，其余说通用的。 */
export type SignInAgent = "copilot" | "claude" | "codex" | "antigravity" | "other";
/**
 * 一次 Pi 子智能体派发里的一个子智能体，按父会话记录里写下的那份。运行中的实时状态
 * 在会话状态的 `subagents` 里，按 `id` 对上。
 */
export type SubagentRun = {
  id: string;
  title: string;
  agent: string;
  status: SubagentStatus;
};
export type SubagentStatus = "running" | "completed" | "failed" | "stopped" | "interrupted";
export type SubagentTranscriptParams = {
  /** 会话宿主分的子智能体 id（`sa-` 加 12 位十六进制）。 */
  subagent_id: string;
  /** 只要这一条之前的条目（翻更早的页）。不给就是最后一页。 */
  before?: number | null;
  /** 一页最多几条。运行时会把它夹到 1..=1000，不给是 200。 */
  limit?: number | null;
};
export type SubagentTranscriptResult = {
  entries: AcpEntry[];
  /** `entries` 第一条在整段子会话里的下标。大于 0 说明前面还有。 */
  offset: number;
  /** 整段子会话一共多少条。 */
  total: number;
};
export type SystemDescribeResult = {
  control: ControlProtocolDescription;
  daemon: DaemonRuntimeInfo;
  /** `None` 只表示运行时早于版本协商。 */
  events?: EventProtocolDescription | null;
};
export type SystemTime = {
  secs_since_epoch: number;
  nanos_since_epoch: number;
};
/** 一条后台任务的结束说明。人看到的是状态行；模型另走自定义消息。 */
export type TaskNote = {
  /** 旧快照没有这个字段，都是后台任务。 */
  kind: TaskNoteKind;
  id: string;
  title: string;
  status: string;
  exitCode: number | null;
  outputTail: string;
  /** 后台子智能体派发：每个子智能体的结束状态。`id` 是派发它的工具调用 id。 */
  runs?: SubagentRun[];
  /** 别的 Pi 对话发来的消息：来源和类别。`output_tail` 是正文，`title` 和 `status` 不用。 */
  peer?: PeerNote | null;
};
/** 状态行说的是哪一种后台工作。 */
export type TaskNoteKind = "backgroundTask" | "subagent" | "peerMessage";
/**
 * 一套终端配色（0xRRGGBB）。字段与 `smelt::terminal` / `smelt::terminal_view`
 * 里实际渲染用的色位一一对应。
 */
export type TerminalThemeSnapshot = {
  version: number;
  /** 深色模式；移动端拿它决定周边 chrome（状态栏图标等）的明暗。 */
  dark: boolean;
  /** 默认背景色，也是 OSC 11 查询的应答值（含用户自选底色）。 */
  background: number;
  /** 默认前景色（OSC 10）。 */
  foreground: number;
  cursor: number;
  selection: number;
  /** ANSI 16 色。 */
  palette: number[];
  searchHit: number;
  searchHitCurrent: number;
};
export type ThinkingLevelParams = {
  level: string;
};
/** 工具调用状态，同上对齐 agent-client-protocol 的 wire 格式。 */
export type ToolCallStatus = "pending" | "in_progress" | "completed" | "failed";
/**
 * 工具类别，跟 `agent-client-protocol::ToolKind` 的 wire 格式（snake_case）
 * 对齐，落盘数据能跨协议版本读。`Other` 兜底未来协议新增的分类，不会因为一个
 * 陌生 tag 就让整条记录反序列化失败。
 */
export type ToolKind = "read" | "edit" | "delete" | "move" | "search" | "execute" | "think" | "fetch" | "switch_mode" | "collaborate" | "review" | "image" | "compact" | "wait" | "other";
/** 工具调用的一段输出：纯文本，文件 diff，或嵌入的 ACP 终端。 */
export type ToolOutputPart = {
  Text: string;
} | {
  Diff: {
    path: string;
    /** 新文件没有旧内容。 */
    old_text: string | null;
    new_text: string;
  };
} | {
  Image: AcpImage;
} | {
  Terminal: {
    id: string;
    output: string;
    truncated: boolean;
    exit_code: number | null;
    signal: string | null;
  };
};
/** 一轮没有正常完成的原因。用户自己取消的不算。 */
export type TurnFailure = "interrupted" | "max_tokens" | "max_turn_requests" | "refused" | "failed" | "other";
/** 回合失败时运行时认得出、能给出下一步的情况。只放确定能给出动作的：猜错了会把人支到错的地方。 */
export type TurnHint = "no_api_key" | "api_key_rejected" | "connection_refused" | "rate_limited" | "other";
export type WebhookIngress = {
  endpoint: string;
  secret?: string | null;
};
export type WebhookIngressInput = {
  endpoint?: string;
  secret?: string | null;
};
/** ACP 对话的启动参数和尚未交付的状态。 */
export type WorkspaceAcp = {
  cwd: string | null;
  /** 守护托管用的会话 id。写入时必填。 */
  sid: string;
  /** agent 种类标识（`ConversationAgentKind::id()`）。 */
  agent: string;
  profile_id: string | null;
  /** 续接用的 provider 对话 id。守护确认过之后以守护为准，客户端带来的旧值不覆盖它。 */
  history_session_id: string | null;
  launch: ConversationLaunchSpec;
  /** 最后生效的 provider 配置：`(config_id, value_id)`，按设置顺序。 */
  config_values: [string, string][];
  pending_prompt: string | null;
  pending_agent_preset: string | null;
  session_title: string | null;
  agent_definition_id: string | null;
  automation_id: string | null;
};
/** ACP 对话的启动参数和尚未交付的状态。 */
export type WorkspaceAcpInput = {
  cwd?: string | null;
  /** 守护托管用的会话 id。写入时必填。 */
  sid: string;
  /** agent 种类标识（`ConversationAgentKind::id()`）。 */
  agent: string;
  profile_id?: string | null;
  /** 续接用的 provider 对话 id。守护确认过之后以守护为准，客户端带来的旧值不覆盖它。 */
  history_session_id?: string | null;
  launch: ConversationLaunchSpecInput;
  /** 最后生效的 provider 配置：`(config_id, value_id)`，按设置顺序。 */
  config_values?: [string, string][];
  pending_prompt?: string | null;
  pending_agent_preset?: string | null;
  session_title?: string | null;
  agent_definition_id?: string | null;
  automation_id?: string | null;
};
/** 一段对话：侧栏里的一条。要么是一段 ACP 对话，要么是一组终端。 */
export type WorkspaceConversation = {
  /** 新建它的客户端生成，之后不再变。 */
  id: string;
  custom_title: string | null;
  /** 最近一轮结束的 unix 秒。只由守护在回合结束时写，客户端带来的值不算数（新建时填 0）。 */
  last_updated_at: number;
  /** 只由运行时的归档命令修改，整段更新不能覆盖。 */
  archived_at: number | null;
  /** 新会话创建时绑定的工作集；旧会话保持未绑定。 */
  workset_id: string | null;
  /** 已关闭的工作树原目录；未绑定时恢复前必须选择新的工作树。 */
  closed_workset_root: string | null;
  /** 这段对话里的终端，按客户端给的顺序。ACP 对话没有。 */
  terminals: WorkspaceTerminal[];
  acp: WorkspaceAcp | null;
};
export type WorkspaceConversationArchiveParams = {
  conversation_id: string;
  archived: boolean;
  target_workset_id?: string | null;
};
export type WorkspaceConversationIdParams = {
  conversation_id: string;
};
/** 一段对话：侧栏里的一条。要么是一段 ACP 对话，要么是一组终端。 */
export type WorkspaceConversationInput = {
  /** 新建它的客户端生成，之后不再变。 */
  id: string;
  custom_title?: string | null;
  /** 最近一轮结束的 unix 秒。只由守护在回合结束时写，客户端带来的值不算数（新建时填 0）。 */
  last_updated_at?: number;
  /** 只由运行时的归档命令修改，整段更新不能覆盖。 */
  archived_at?: number | null;
  /** 新会话创建时绑定的工作集；旧会话保持未绑定。 */
  workset_id?: string | null;
  /** 已关闭的工作树原目录；未绑定时恢复前必须选择新的工作树。 */
  closed_workset_root?: string | null;
  /** 这段对话里的终端，按客户端给的顺序。ACP 对话没有。 */
  terminals?: WorkspaceTerminalInput[];
  acp?: WorkspaceAcpInput | null;
};
/** 整份目录。`revision` 每次有改动加一，只在同一个 `epoch` 里能比，见 [`WorkspaceRevision`]。 */
export type WorkspaceDirectory = {
  /** 运行时这份库的随机身份。库换过（删了重建）就不同，版本号从头数。 */
  epoch: string;
  revision: number;
  /** 打开的项目根目录，按打开的先后。 */
  projects: string[];
  /** 项目手动纳入工作集的 Git 仓库。 */
  repositories: WorkspaceProjectRepository[];
  /** 项目下已创建的工作集，按创建顺序。 */
  worksets: WorkspaceWorkset[];
  /** 按新建的先后。 */
  conversations: WorkspaceConversation[];
};
export type WorkspaceProjectParams = {
  root: string;
};
export type WorkspaceProjectRepository = {
  project_root: string;
  name: string;
  root: string;
  position: number;
  managed_link: string | null;
};
export type WorkspaceProjectRepositoryAction = {
  kind: "discover";
} | {
  kind: "bind";
  source: string;
  name: string;
} | {
  kind: "clone";
  url: string;
  name: string;
} | {
  kind: "remove";
  name: string;
};
export type WorkspaceProjectRepositoryActionParams = {
  project_root: string;
  action: WorkspaceProjectRepositoryAction;
};
/**
 * `workspace.rename` 的参数。带 `terminal_id` 是给这段对话里的这一格终端改名，不带是给
 * 整段对话改名。
 */
export type WorkspaceRenameParams = {
  conversation_id: string;
  terminal_id?: string | null;
  title?: string | null;
};
/**
 * 目录的版本：哪一份（`epoch`）、改到第几次。命令回执也是它，没改动时是原来的版本。
 *
 * 版本号只在同一个 `epoch` 里能比。`epoch` 不同说明运行时的库换过，版本号从头数，
 * 新的那份不能因为数小就被当成旧的扔掉。
 */
export type WorkspaceRevision = {
  epoch: string;
  revision: number;
};
export type WorkspaceTerminal = {
  /** 守护里的终端会话 id。没起过的终端没有，运行时用对话 id 派生一个占位。 */
  id: string | null;
  cwd: string | null;
  custom_title: string | null;
  launch_label: string | null;
  launch_cmd: string | null;
};
export type WorkspaceTerminalInput = {
  /** 守护里的终端会话 id。没起过的终端没有，运行时用对话 id 派生一个占位。 */
  id?: string | null;
  cwd?: string | null;
  custom_title?: string | null;
  launch_label?: string | null;
  launch_cmd?: string | null;
};
export type WorkspaceWorkset = {
  id: string;
  project_root: string;
  task: string;
  branch: string;
  position: number;
  repositories: WorkspaceWorksetRepository[];
};
export type WorkspaceWorksetCreateParams = {
  project_root: string;
  task: string;
  branch_prefix?: string;
  branch?: string | null;
  repository_names: string[];
};
/** 新建工作树的回执。目录版本平铺在顶层，和以前只回 `WorkspaceRevision` 时同一个形状。 */
export type WorkspaceWorksetCreateResult = {
  epoch: string;
  revision: number;
  /** 跑了初始化脚本（`<源仓库>/.smelt/worktree-setup`）的仓库，没有脚本的不列。 */
  setup?: WorkspaceWorksetSetupResult[];
};
export type WorkspaceWorksetFinishParams = {
  workset_id: string;
  /**
   * 判断不了分支是否已合并（合进基线有冲突）的仓库也照关。有未提交改动、确实没合并的
   * 仍然拦下。用户在确认框里看过 [`WorkspaceWorksetInspectResult`] 再决定。
   */
  allow_unconfirmed_merge?: boolean;
};
export type WorkspaceWorksetFinishResult = {
  revision: WorkspaceRevision;
  retained_branches: string[];
  leftover_entries: string[];
};
export type WorkspaceWorksetInspectParams = {
  workset_id: string;
};
/** 关闭工作树前的检查：先 fetch 各仓库的基线，再看每个仓库干不干净、分支合没合进去。 */
export type WorkspaceWorksetInspectResult = {
  repositories: WorkspaceWorksetRepositoryState[];
  /** fetch 失败的说明；这时合并情况按本地已有的远端分支算。 */
  refresh_failures?: string[];
};
/** 工作树分支上的改动进没进基线。 */
export type WorkspaceWorksetMerge = "merged" | "unmerged" | "unconfirmed";
export type WorkspaceWorksetRenameParams = {
  workset_id: string;
  task: string;
};
export type WorkspaceWorksetRepository = {
  name: string;
  root: string;
  worktree: string;
  base_ref: string;
  branch_created: boolean;
  position: number;
};
export type WorkspaceWorksetRepositoryState = {
  name: string;
  /** 有未提交的改动或未跟踪的文件。 */
  dirty: boolean;
  /** 本地还有工作树分支时它的合并情况；分支已经不在了是空。 */
  merge?: WorkspaceWorksetMerge | null;
};
export type WorkspaceWorksetSetupResult = {
  repository: string;
  success: boolean;
  /** 失败时输出的末尾一段（stdout 和 stderr 合在一起），成功时为空。 */
  output?: string;
};
/** 运行时控制接口的方法（call 的 method）和各自的参数、结果。 */
export interface ControlMethods {
  "system.describe": { params: EmptyParams; result: SystemDescribeResult };
  "session.list": { params: EmptyParams; result: SessionListResult };
  "agent.memory.meta": { params: AgentMemorySessionParams; result: AgentMemoryMetaResult };
  "agent.memory.record_read": { params: AgentMemoryRecordReadParams; result: EmptyParams };
  "agent.memory.record_write": { params: AgentMemoryRecordWriteParams; result: EmptyParams };
  "agent.definition.list": { params: EmptyParams; result: AgentDefinitionListResult };
  "agent.definition.get": { params: AgentDefinitionIdParams; result: AgentDefinitionGetResult };
  "agent.definition.create": { params: AgentDefinitionCreate; result: AgentDefinitionGetResult };
  "agent.definition.update": { params: AgentDefinitionUpdateParams; result: AgentDefinitionGetResult };
  "agent.definition.delete": { params: AgentDefinitionIdParams; result: AgentDefinitionIdParams };
  "settings.get": { params: EmptyParams; result: SettingsResult };
  "settings.update": { params: SettingsPatch; result: SettingsResult };
  "automation.list": { params: EmptyParams; result: AutomationListResult };
  "automation.get": { params: AgentDefinitionIdParams; result: AutomationGetResult };
  "automation.upsert": { params: AutomationInput; result: AutomationGetResult };
  "automation.delete": { params: AgentDefinitionIdParams; result: AgentDefinitionIdParams };
  "automation.set_enabled": { params: AutomationSetEnabledParams; result: AutomationGetResult };
  "automation.run_once": { params: AgentDefinitionIdParams; result: AutomationRunOnceResult };
  "git.discover": { params: GitRootParams; result: Discovery };
  "git.repo_info": { params: GitRootParams; result: GitRepoInfoResult };
  "git.status": { params: GitRootParams; result: GitStatusData };
  "git.branches": { params: GitRootParams; result: BranchList };
  "git.checkout": { params: GitBranchParams; result: EmptyParams };
  "git.branch_delete": { params: GitBranchDeleteParams; result: EmptyParams };
  "git.merge": { params: GitBranchParams; result: EmptyParams };
  "git.stage": { params: GitPathsParams; result: EmptyParams };
  "git.unstage": { params: GitPathsParams; result: EmptyParams };
  "git.discard_file": { params: GitDiscardFileParams; result: EmptyParams };
  "git.apply_patch": { params: GitApplyPatchParams; result: EmptyParams };
  "git.discard_all": { params: GitDiscardAllParams; result: EmptyParams };
  "git.commit": { params: GitCommitParams; result: EmptyParams };
  "git.push": { params: GitBranchParams; result: EmptyParams };
  "git.fetch": { params: GitRootParams; result: EmptyParams };
  "git.pull": { params: GitRootParams; result: EmptyParams };
  "git.stash_push": { params: GitRootParams; result: EmptyParams };
  "git.stash_pop": { params: GitRootParams; result: EmptyParams };
  "git.diff.file": { params: GitFileDiffParams; result: GitFileDiffResult };
  "git.diff.all": { params: GitAllDiffParams; result: GitTextResult };
  "git.log": { params: GitLogParams; result: GitLogResult };
  "git.commit.files": { params: GitCommitRefParams; result: GitCommitFilesResult };
  "git.commit.file_diff": { params: GitCommitFileParams; result: GitTextResult };
  "project.tree": { params: ProjectDirParams; result: ProjectTreeResult };
  "project.search": { params: ProjectSearchParams; result: ProjectSearchResult };
  "project.delete_entry": { params: ProjectPathParams; result: EmptyParams };
  "file.open": { params: FilePathParams; result: FileOpenResult };
  "file.save": { params: FileSaveParams; result: FileSaveResult };
  "session.complete_files": { params: CompleteFilesParams; result: CompleteFilesResult };
  "history.list": { params: HistoryListParams; result: HistoryListResult };
  "history.detail": { params: HistoryDetailParams; result: HistoryDetailResult };
  "history.delete": { params: HistoryDeleteParams; result: EmptyParams };
  "agent.host.get": { params: EmptyParams; result: AgentHostSettings };
  "agent.host.apply_edit": { params: AgentHostEdit; result: AgentHostSettings };
  "launch.get": { params: EmptyParams; result: LaunchSettings };
  "launch.replace": { params: LaunchReplaceParams; result: LaunchSettings };
  "memory.test_embedding": { params: MemoryTestParams; result: MemoryTestResult };
  "workspace.conversation.create": { params: WorkspaceConversationInput; result: WorkspaceRevision };
  "workspace.conversation.update": { params: WorkspaceConversationInput; result: WorkspaceRevision };
  "workspace.conversation.delete": { params: WorkspaceConversationIdParams; result: WorkspaceRevision };
  "workspace.conversation.archive": { params: WorkspaceConversationArchiveParams; result: WorkspaceRevision };
  "workspace.project.open": { params: WorkspaceProjectParams; result: WorkspaceRevision };
  "workspace.project.close": { params: WorkspaceProjectParams; result: WorkspaceRevision };
  "workspace.project.repository.action": { params: WorkspaceProjectRepositoryActionParams; result: WorkspaceDirectory };
  "workspace.workset.create_with_branch": { params: WorkspaceWorksetCreateParams; result: WorkspaceWorksetCreateResult };
  "workspace.workset.rename_preserving_branch": { params: WorkspaceWorksetRenameParams; result: WorkspaceRevision };
  "workspace.workset.inspect": { params: WorkspaceWorksetInspectParams; result: WorkspaceWorksetInspectResult };
  "workspace.workset.finish": { params: WorkspaceWorksetFinishParams; result: WorkspaceWorksetFinishResult };
  "workspace.directory": { params: EmptyParams; result: WorkspaceDirectory };
  "workspace.rename": { params: WorkspaceRenameParams; result: WorkspaceRevision };
  "agent.memory.list": { params: AgentDefinitionIdParams; result: AgentMemories };
  "agent.memory.delete": { params: AgentMemoryFileParams; result: EmptyParams };
  "agent.memory.approve": { params: AgentMemoryFileParams; result: EmptyParams };
  "automation.run_detail": { params: AutomationRunDetailParams; result: AutomationRunDetailResult };
  "agent.space.ensure": { params: AgentDefinitionIdParams; result: AgentSpaceResult };
  "agent.hooks.status": { params: EmptyParams; result: AgentHooksStatus };
  "agent.hooks.install": { params: EmptyParams; result: AgentHooksStatus };
  "agent.hooks.uninstall": { params: EmptyParams; result: AgentHooksStatus };
  "agent.cli.inspect": { params: EmptyParams; result: AcpRuntimeDiagnostics };
  "agent.cli.install": { params: AgentCliInstallParams; result: AgentCliInstallResult };
  "pi.config.get": { params: EmptyParams; result: PiConfig };
  "pi.config.ensure_file": { params: EnsureConfigFileParams; result: EnsureConfigFileResult };
  "pi.models.set_thinking_level": { params: ThinkingLevelParams; result: PiConfig };
  "pi.models.save_default": { params: SaveDefaultModelParams; result: PiConfig };
  "pi.providers.discover_models": { params: DiscoverModelsParams; result: DiscoverModelsResult };
  "pi.providers.save_custom": { params: SaveCustomProviderParams; result: PiSaveCustomProviderResult };
  "pi.providers.remove_custom": { params: ProviderIdParams; result: PiConfig };
  "pi.providers.refresh_auto": { params: EmptyParams; result: EmptyParams };
  "pi.mcp.add_preset": { params: McpPresetParams; result: PiConfig };
  "pi.mcp.set_enabled": { params: McpEnabledParams; result: PiConfig };
  "pi.mcp.remove": { params: McpServerParams; result: PiConfig };
  "pi.mcp.login_start": { params: McpServerParams; result: LoginIdParams };
  "pi.plugins.remove": { params: PluginIdParams; result: PiConfig };
  "pi.plugins.import": { params: ImportPluginParams; result: PiConfig };
  "pi.plugins.adopt": { params: AdoptPluginParams; result: PiConfig };
  "pi.auth.providers": { params: EmptyParams; result: ProvidersResult };
  "pi.auth.models": { params: ProviderIdParams; result: ModelsResult };
  "pi.auth.logout": { params: ProviderIdParams; result: EmptyParams };
  "pi.auth.login_start": { params: LoginStartParams; result: LoginIdParams };
  "pi.auth.login_events": { params: LoginEventsParams; result: LoginEventsResult };
  "pi.auth.login_answer": { params: LoginAnswerParams; result: EmptyParams };
  "pi.auth.login_cancel": { params: LoginIdParams; result: EmptyParams };
  "quota.get": { params: EmptyParams; result: QuotaSnapshot };
  "quota.refresh": { params: QuotaRefreshParams; result: QuotaRefreshResult };
  "quota.settings.get": { params: EmptyParams; result: QuotaSettings };
  "quota.set_enabled": { params: QuotaSetEnabledParams; result: QuotaSettings };
  "remote.config.get": { params: EmptyParams; result: RemoteConfig };
  "remote.config.set": { params: RemoteConfigInput; result: RemoteAccessState };
  "remote.status": { params: EmptyParams; result: RemoteAccessState };
  "remote.status.wait": { params: RemoteStatusWaitParams; result: RemoteAccessState };
  "device.pair_begin": { params: EmptyParams; result: PairingOffer };
  "device.pair": { params: PairParams; result: EmptyParams };
  "device.list": { params: EmptyParams; result: DeviceList };
  "device.revoke": { params: DeviceIdParams; result: RevokeResult };
  "push.register": { params: PushRegisterParams; result: EmptyParams };
  "push.unregister": { params: EmptyParams; result: EmptyParams };
  "notify.send": { params: NotifySendParams; result: NotifySendResult };
  "attention.mark_read": { params: AttentionMarkReadParams; result: EmptyParams };
  "new_session.pins.get": { params: EmptyParams; result: NewSessionPreferences };
  "new_session.pins.toggle": { params: PinParams; result: NewSessionPreferences };
  "terminal_theme.publish": { params: TerminalThemeSnapshot; result: EmptyParams };
  "desktop.type_text": { params: DesktopTypeTextParams; result: EmptyParams };
  "session.launch.prepare": { params: LaunchPrepareParams; result: LaunchPrepareResult };
  "session.loaded_skills": { params: LoadedSkillsParams; result: LoadedSkillsResult };
  "agent.memory.ensure_dir": { params: AgentMemoryDirParams; result: AgentSpaceResult };
  "agent.memory.watch": { params: AgentDefinitionIdParams; result: EmptyParams };
  "project.watch": { params: ProjectRootParams; result: EmptyParams };
  "workspace.browse_dirs": { params: BrowseDirsParams; result: BrowseDirsResult };
  "conversation.create_for_agent": { params: AgentDefinitionIdParams; result: CreatedAgentConversation };
  "new_session.catalog": { params: EmptyParams; result: NewSessionCatalogResult };
  "session.create": { params: SessionCreateParams; result: SessionCreateResult };
  "session.delete": { params: SessionIdParams; result: EmptyParams };
  "session.agent_bindings": { params: EmptyParams; result: SessionAgentBindingsResult };
  "conversation.image": { params: ConversationImageParams; result: ConversationImageResult };
  "conversation.prompt_index": { params: ConversationPromptIndexParams; result: ConversationPromptIndexResult };
  "conversation.prompt": { params: ConversationPromptParams; result: EmptyParams };
  "conversation.turn_result": { params: ConversationTurnResultParams; result: ConversationTurnStatus };
  "subagent.transcript": { params: SubagentTranscriptParams; result: SubagentTranscriptResult };
  "resource.usage": { params: EmptyParams; result: ResourceUsageResult };
  "app.command.list": { params: EmptyParams; result: AppCommandListResult };
  "app.command.run": { params: AppCommandRunParams; result: AppCommandRunResult };
}
