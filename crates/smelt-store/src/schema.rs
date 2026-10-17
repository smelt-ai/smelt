//! 首个发布版本的库结构。以后改已发布的表时：
//! 1. 同步改这份 `CREATE_SCHEMA`（新库一步建齐）；
//! 2. `STORE_SCHEMA_VERSION` 加一；
//! 3. 在存储初始化器中补对应版本的事务迁移。
//!
//! 迁移必须先读出旧数据，再替换结构；认不出的库一律拒绝打开。
//!
//! 只是给表加一列、且该表没有表级约束时，用 `ALTER TABLE … ADD COLUMN`，并把新列写在
//! `CREATE_SCHEMA` 里这张表的最后：SQLite 只改表定义、不搬数据，改完的建表语句去掉空白后
//! 也和 `CREATE_SCHEMA` 一致。改约束、改默认值、删列、调列序这些 ALTER 做不到的，才整表重建。
//! 默认值要和列类型一致：BLOB 列用 `X'…'`，不要写文本字面量。
//!
//! 业务数据一律按字段建表：单例设置用 `id = 1` 的单行表，列表放子表并带 `position`。
//! 不要再引入通用的 key/value 或 JSON 文档表。v17 及更早版本的形状以冻结的
//! `V17_SCHEMA` 为准，旧版本的迁移和形状校验都从它推导。

pub const TABLES: [&str; 53] = [
    "acp_config_value",
    "acp_detail",
    "acp_launch_env",
    "agent_acp_command",
    "agent_acp_config_memory",
    "agent_acp_env",
    "agent_definition",
    "agent_host_pref",
    "agent_memory_provenance",
    "agent_memory_usage",
    "agent_profile",
    "auto_model_provider",
    "automation",
    "automation_meta",
    "automation_run",
    "automation_run_transcript",
    "automation_state",
    "command_dedupe",
    "default_conversation_agent",
    "event_dead_letter",
    "event_delivery_retry",
    "event_log",
    "event_outbox",
    "history_agent_binding",
    "history_title",
    "launch_entry",
    "launch_settings",
    "layout_node",
    "memory_settings",
    "new_session_pin",
    "paired_device",
    "peer_message_command",
    "project",
    "published_session",
    "quota_cache",
    "quota_cache_provider",
    "quota_cache_status",
    "quota_cache_window",
    "remote_acp_session",
    "remote_config",
    "remote_terminal_session",
    "runtime_identity",
    "session",
    "session_group",
    "subscription_cursor",
    "terminal_theme",
    "terminal_theme_palette",
    "workspace_catalog",
    "workspace_menu",
    "workspace_menu_project",
    "workspace_menu_session",
    "worktree_inherit",
    "worktree_inherit_skip_pattern",
];

/// v17 的表清单。v18 去掉了通用 `kv`，各领域改成按字段建表。
pub const V17_TABLES: [&str; 32] = [
    "acp_detail",
    "agent_acp_command",
    "agent_acp_config_memory",
    "agent_acp_env",
    "agent_definition",
    "agent_memory_provenance",
    "agent_memory_usage",
    "agent_profile",
    "agent_ui_pref",
    "automation",
    "automation_meta",
    "automation_run",
    "automation_run_transcript",
    "automation_state",
    "command_dedupe",
    "event_dead_letter",
    "event_delivery_retry",
    "event_log",
    "event_outbox",
    "history_title",
    "kv",
    "launch_entry",
    "layout_node",
    "paired_device",
    "peer_message_command",
    "project",
    "published_session",
    "remote_acp_session",
    "remote_terminal_session",
    "session",
    "session_group",
    "subscription_cursor",
];

/// v16 的表清单。v17 加了已配对设备表。
pub const V16_TABLES: [&str; 31] = [
    "acp_detail",
    "agent_acp_command",
    "agent_acp_config_memory",
    "agent_acp_env",
    "agent_definition",
    "agent_memory_provenance",
    "agent_memory_usage",
    "agent_profile",
    "agent_ui_pref",
    "automation",
    "automation_meta",
    "automation_run",
    "automation_run_transcript",
    "automation_state",
    "command_dedupe",
    "event_dead_letter",
    "event_delivery_retry",
    "event_log",
    "event_outbox",
    "history_title",
    "kv",
    "launch_entry",
    "layout_node",
    "peer_message_command",
    "project",
    "published_session",
    "remote_acp_session",
    "remote_terminal_session",
    "session",
    "session_group",
    "subscription_cursor",
];

/// v12 的表清单。v13 加了智能体记忆的元数据表（使用统计、来源）。
pub const V12_TABLES: [&str; 29] = [
    "acp_detail",
    "agent_acp_command",
    "agent_acp_config_memory",
    "agent_acp_env",
    "agent_definition",
    "agent_profile",
    "agent_ui_pref",
    "automation",
    "automation_meta",
    "automation_run",
    "automation_run_transcript",
    "automation_state",
    "command_dedupe",
    "event_dead_letter",
    "event_delivery_retry",
    "event_log",
    "event_outbox",
    "history_title",
    "kv",
    "launch_entry",
    "layout_node",
    "peer_message_command",
    "project",
    "published_session",
    "remote_acp_session",
    "remote_terminal_session",
    "session",
    "session_group",
    "subscription_cursor",
];

/// v9 的表清单。v10 把智能体配置、远程会话和守护会话目录从 `kv` 文档拆成关系表。
pub const V9_TABLES: [&str; 20] = [
    "acp_detail",
    "automation",
    "automation_meta",
    "automation_run",
    "automation_run_transcript",
    "automation_state",
    "command_dedupe",
    "event_dead_letter",
    "event_delivery_retry",
    "event_log",
    "event_outbox",
    "history_title",
    "kv",
    "launch_entry",
    "layout_node",
    "peer_message_command",
    "project",
    "session",
    "session_group",
    "subscription_cursor",
];

/// v8 的表清单。v9 把自动化从 `kv` 文档拆成关系表。
pub const V8_TABLES: [&str; 16] = [
    "acp_detail",
    "automation_run_transcript",
    "command_dedupe",
    "event_dead_letter",
    "event_delivery_retry",
    "event_log",
    "event_outbox",
    "history_title",
    "kv",
    "launch_entry",
    "layout_node",
    "peer_message_command",
    "project",
    "session",
    "session_group",
    "subscription_cursor",
];

/// v7 的表清单。v8 只多了 `automation_run_transcript`。
pub const V7_TABLES: [&str; 15] = [
    "acp_detail",
    "command_dedupe",
    "event_dead_letter",
    "event_delivery_retry",
    "event_log",
    "event_outbox",
    "history_title",
    "kv",
    "launch_entry",
    "layout_node",
    "peer_message_command",
    "project",
    "session",
    "session_group",
    "subscription_cursor",
];

pub const CREATE_SCHEMA: &str = "
CREATE TABLE launch_entry (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  label TEXT NOT NULL,
  command TEXT NOT NULL,
  provider TEXT
);

CREATE TABLE project (
  root TEXT PRIMARY KEY,
  position INTEGER NOT NULL UNIQUE CHECK (position >= 0)
);

CREATE TABLE session_group (
  id TEXT PRIMARY KEY,
  project_root TEXT REFERENCES project(root) ON DELETE SET NULL,
  position INTEGER NOT NULL UNIQUE CHECK (position >= 0),
  active_session_id TEXT REFERENCES session(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED,
  custom_title TEXT,
  last_updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE session (
  id TEXT PRIMARY KEY,
  group_id TEXT NOT NULL REFERENCES session_group(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('terminal', 'acp')),
  cwd TEXT,
  custom_title TEXT,
  launch_label TEXT,
  launch_command TEXT
);
CREATE INDEX session_group_id_idx ON session(group_id);

CREATE TABLE layout_node (
  id TEXT PRIMARY KEY,
  group_id TEXT NOT NULL REFERENCES session_group(id) ON DELETE CASCADE,
  parent_id TEXT REFERENCES layout_node(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  kind TEXT NOT NULL CHECK (kind IN ('split', 'leaf')),
  axis TEXT CHECK (axis IN ('horizontal', 'vertical') OR axis IS NULL),
  session_id TEXT UNIQUE REFERENCES session(id) ON DELETE CASCADE,
  size_px REAL CHECK (size_px IS NULL OR size_px >= 0),
  UNIQUE (group_id, parent_id, position),
  CHECK (
    (kind = 'split' AND axis IS NOT NULL AND session_id IS NULL)
    OR (kind = 'leaf' AND axis IS NULL AND session_id IS NOT NULL)
  )
);
CREATE UNIQUE INDEX layout_node_root_idx
  ON layout_node(group_id)
  WHERE parent_id IS NULL;

CREATE TABLE acp_detail (
  session_id TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  profile_id TEXT,
  history_session_id TEXT,
  launch_command TEXT NOT NULL,
  refresh_launch_from_settings INTEGER NOT NULL DEFAULT 0
    CHECK (refresh_launch_from_settings IN (0, 1)),
  pending_prompt TEXT,
  pending_delivery_id TEXT,
  agent_definition_id TEXT,
  automation_id TEXT,
  binding_automation_run_id TEXT,
  pending_agent_preset TEXT,
  session_title TEXT
);
CREATE TABLE acp_launch_env (
  session_id TEXT NOT NULL REFERENCES acp_detail(session_id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY (session_id, name)
) WITHOUT ROWID;
CREATE TABLE acp_config_value (
  session_id TEXT NOT NULL REFERENCES acp_detail(session_id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  config_id TEXT NOT NULL,
  value_id TEXT NOT NULL,
  PRIMARY KEY (session_id, position)
) WITHOUT ROWID;

CREATE TABLE history_title (
  agent TEXT NOT NULL,
  profile_id TEXT NOT NULL DEFAULT '',
  resume_id TEXT NOT NULL,
  custom_title TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (agent, profile_id, resume_id)
) WITHOUT ROWID;

CREATE TABLE event_log (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  topic TEXT NOT NULL,
  aggregate_kind TEXT,
  aggregate_id TEXT,
  aggregate_revision INTEGER,
  occurred_at_ms INTEGER NOT NULL,
  envelope_json BLOB NOT NULL,
  CHECK (
    (aggregate_kind IS NULL AND aggregate_id IS NULL AND aggregate_revision IS NULL)
    OR (aggregate_kind IS NOT NULL AND aggregate_id IS NOT NULL AND aggregate_revision IS NOT NULL)
  )
);
CREATE INDEX event_log_topic_sequence_idx ON event_log(topic, sequence);
CREATE INDEX event_log_aggregate_sequence_idx
  ON event_log(aggregate_kind, aggregate_id, sequence);

CREATE TABLE event_outbox (
  outbox_id INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  envelope_json BLOB NOT NULL,
  created_at_ms INTEGER NOT NULL,
  dispatched_sequence INTEGER REFERENCES event_log(sequence)
);
CREATE INDEX event_outbox_pending_idx
  ON event_outbox(dispatched_sequence, outbox_id);

CREATE TABLE subscription_cursor (
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  last_acked_sequence INTEGER NOT NULL DEFAULT 0 CHECK (last_acked_sequence >= 0),
  declaration_fingerprint TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (plugin_id, subscription_id)
) WITHOUT ROWID;

CREATE TABLE event_dead_letter (
  dead_letter_id INTEGER PRIMARY KEY AUTOINCREMENT,
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  event_id TEXT NOT NULL,
  attempts INTEGER NOT NULL CHECK (attempts > 0),
  reason TEXT NOT NULL,
  failed_at_ms INTEGER NOT NULL,
  UNIQUE(plugin_id, subscription_id, sequence)
);
CREATE INDEX event_dead_letter_subscription_idx
  ON event_dead_letter(plugin_id, subscription_id, dead_letter_id);

CREATE TABLE event_delivery_retry (
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  sequence INTEGER NOT NULL REFERENCES event_log(sequence),
  event_id TEXT NOT NULL,
  attempts INTEGER NOT NULL CHECK (attempts > 0),
  next_attempt_at_ms INTEGER NOT NULL,
  reason TEXT NOT NULL,
  PRIMARY KEY (plugin_id, subscription_id),
  FOREIGN KEY (plugin_id, subscription_id)
    REFERENCES subscription_cursor(plugin_id, subscription_id) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE TABLE command_dedupe (
  plugin_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  completed_at_ms INTEGER NOT NULL,
  result_json BLOB NOT NULL,
  PRIMARY KEY (plugin_id, command_id)
) WITHOUT ROWID;

CREATE TABLE peer_message_command (
  source_session_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  completed_at_ms INTEGER NOT NULL,
  message_id TEXT NOT NULL,
  target_session_id TEXT NOT NULL,
  PRIMARY KEY (source_session_id, command_id)
) WITHOUT ROWID;

CREATE TABLE automation (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  workspace_dir TEXT,
  position INTEGER NOT NULL CHECK (position >= 0),
  trigger_json BLOB NOT NULL,
  action_json BLOB NOT NULL,
  sinks_json BLOB NOT NULL
);
CREATE TABLE automation_state (
  automation_id TEXT PRIMARY KEY REFERENCES automation(id) ON DELETE CASCADE,
  next_run_at INTEGER,
  last_run_id TEXT
);
CREATE TABLE automation_run (
  id TEXT PRIMARY KEY,
  automation_id TEXT NOT NULL REFERENCES automation(id) ON DELETE CASCADE,
  source TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  scheduled_for INTEGER,
  started_at INTEGER,
  delivery_attempt_at INTEGER,
  delivery_attempts INTEGER NOT NULL DEFAULT 0 CHECK (delivery_attempts >= 0),
  finished_at INTEGER,
  session_id TEXT,
  provider_session_id TEXT,
  output TEXT,
  error TEXT,
  runtime_released_at INTEGER,
  context_json BLOB NOT NULL
);
CREATE INDEX automation_run_automation_created_idx
  ON automation_run(automation_id, created_at);
CREATE TABLE automation_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE automation_run_transcript (
  run_id TEXT PRIMARY KEY REFERENCES automation_run(id) ON DELETE CASCADE,
  entries_json BLOB NOT NULL,
  updated_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE agent_host_pref (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  hooks_enabled INTEGER NOT NULL CHECK (hooks_enabled IN (0, 1)),
  cross_agent_enabled INTEGER NOT NULL CHECK (cross_agent_enabled IN (0, 1))
);
CREATE TABLE agent_acp_command (
  command_key TEXT PRIMARY KEY,
  command TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE agent_acp_env (
  engine_kind_id TEXT NOT NULL,
  name TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY (engine_kind_id, name)
) WITHOUT ROWID;
CREATE TABLE agent_acp_config_memory (
  engine_kind_id TEXT NOT NULL,
  config_id TEXT NOT NULL,
  value_id TEXT NOT NULL,
  position INTEGER NOT NULL CHECK (position >= 0),
  PRIMARY KEY (engine_kind_id, config_id)
) WITHOUT ROWID;
CREATE TABLE agent_definition (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  engine_kind_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  plugins_json BLOB NOT NULL,
  mcp_servers_json BLOB NOT NULL DEFAULT X'5B5D',
  context_folders_json BLOB NOT NULL,
  context_links_json BLOB NOT NULL,
  model_provider TEXT NOT NULL DEFAULT '',
  model_id TEXT NOT NULL DEFAULT '',
  memory_enabled INTEGER NOT NULL DEFAULT 1 CHECK (memory_enabled IN (0, 1)),
  position INTEGER NOT NULL CHECK (position >= 0)
);
CREATE TABLE agent_memory_provenance (
  scope TEXT NOT NULL,
  file_name TEXT NOT NULL,
  trust TEXT NOT NULL CHECK (trust IN ('trusted', 'pending')),
  reason TEXT NOT NULL,
  session_id TEXT NOT NULL,
  written_at_ms INTEGER NOT NULL,
  PRIMARY KEY (scope, file_name)
) WITHOUT ROWID;
CREATE TABLE agent_memory_usage (
  scope TEXT NOT NULL,
  file_name TEXT NOT NULL,
  read_count INTEGER NOT NULL CHECK (read_count >= 0),
  last_read_at_ms INTEGER NOT NULL,
  PRIMARY KEY (scope, file_name)
) WITHOUT ROWID;
CREATE TABLE agent_profile (
  id TEXT PRIMARY KEY,
  kind_id TEXT NOT NULL,
  label TEXT NOT NULL,
  workspace_dir TEXT NOT NULL,
  position INTEGER NOT NULL CHECK (position >= 0)
);
CREATE TABLE remote_acp_session (
  id TEXT PRIMARY KEY,
  cwd TEXT NOT NULL,
  title TEXT NOT NULL,
  agent_option_id TEXT NOT NULL,
  agent TEXT NOT NULL,
  launch_command TEXT NOT NULL,
  launch_env_json BLOB NOT NULL,
  resume_id TEXT,
  created_at INTEGER NOT NULL,
  lifecycle TEXT NOT NULL,
  hidden INTEGER NOT NULL CHECK (hidden IN (0, 1))
);
CREATE TABLE remote_terminal_session (
  id TEXT PRIMARY KEY,
  cwd TEXT NOT NULL,
  title TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  lifecycle TEXT NOT NULL
);
CREATE TABLE published_session (
  id TEXT PRIMARY KEY,
  cwd TEXT,
  launch TEXT,
  provider TEXT,
  conversation_id TEXT,
  title TEXT,
  phase TEXT NOT NULL,
  phase_since INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  structured_events INTEGER NOT NULL CHECK (structured_events IN (0, 1)),
  turn_events INTEGER NOT NULL CHECK (turn_events IN (0, 1)),
  agent_event_version INTEGER,
  tokens_used INTEGER,
  branch TEXT,
  dirty_files_json BLOB NOT NULL
);
CREATE TABLE paired_device (
  endpoint_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  paired_at_ms INTEGER NOT NULL,
  last_seen_at_ms INTEGER
) WITHOUT ROWID;
CREATE TABLE runtime_identity (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  runtime_id TEXT NOT NULL
);
CREATE TABLE workspace_catalog (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  revision INTEGER NOT NULL CHECK (revision >= 0)
);
CREATE TABLE launch_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL CHECK (version >= 0)
);
CREATE TABLE history_agent_binding (
  agent TEXT NOT NULL,
  profile_id TEXT NOT NULL DEFAULT '',
  resume_id TEXT NOT NULL,
  agent_definition_id TEXT NOT NULL,
  PRIMARY KEY (agent, profile_id, resume_id)
) WITHOUT ROWID;
CREATE TABLE default_conversation_agent (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  agent_definition_id TEXT NOT NULL
);
CREATE TABLE new_session_pin (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  action_key TEXT NOT NULL UNIQUE
);
CREATE TABLE memory_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  embedding_base_url TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_api_key TEXT NOT NULL,
  rerank_model_provider TEXT NOT NULL,
  rerank_model_id TEXT NOT NULL
);
CREATE TABLE remote_config (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  iroh_relay TEXT NOT NULL
);
CREATE TABLE worktree_inherit (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
);
CREATE TABLE worktree_inherit_skip_pattern (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  pattern TEXT NOT NULL
);
CREATE TABLE terminal_theme (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL CHECK (version >= 0),
  dark INTEGER NOT NULL CHECK (dark IN (0, 1)),
  background INTEGER NOT NULL,
  foreground INTEGER NOT NULL,
  cursor INTEGER NOT NULL,
  selection INTEGER NOT NULL,
  search_hit INTEGER NOT NULL,
  search_hit_current INTEGER NOT NULL
);
CREATE TABLE terminal_theme_palette (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  color INTEGER NOT NULL
);
CREATE TABLE auto_model_provider (
  harness TEXT NOT NULL CHECK (harness IN ('dsh', 'pi')),
  provider TEXT NOT NULL,
  PRIMARY KEY (harness, provider)
) WITHOUT ROWID;
CREATE TABLE quota_cache (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  saved_at_ms INTEGER NOT NULL,
  schema_version INTEGER NOT NULL CHECK (schema_version >= 0)
);
CREATE TABLE quota_cache_provider (
  provider_id TEXT PRIMARY KEY
) WITHOUT ROWID;
CREATE TABLE quota_cache_status (
  provider_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('windows', 'copilot')),
  checked_at_ms INTEGER NOT NULL,
  error TEXT,
  copilot_unit TEXT CHECK (copilot_unit IN ('ai_credits', 'premium_requests')),
  copilot_remaining REAL,
  copilot_limit REAL,
  copilot_used REAL,
  CHECK (
    (kind = 'windows' AND copilot_unit IS NULL AND copilot_remaining IS NULL
      AND copilot_limit IS NULL AND copilot_used IS NULL)
    OR (kind = 'copilot' AND copilot_unit IS NOT NULL AND copilot_remaining IS NOT NULL)
  )
) WITHOUT ROWID;
CREATE TABLE quota_cache_window (
  provider_id TEXT NOT NULL REFERENCES quota_cache_status(provider_id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  label TEXT NOT NULL,
  used_percent REAL NOT NULL,
  resets_at_ms INTEGER,
  reset_after_seconds INTEGER,
  PRIMARY KEY (provider_id, position)
) WITHOUT ROWID;
CREATE TABLE workspace_menu (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL CHECK (version >= 0),
  revision INTEGER NOT NULL CHECK (revision >= 0),
  source_id TEXT NOT NULL,
  source_revision INTEGER NOT NULL CHECK (source_revision >= 0)
);
CREATE TABLE workspace_menu_project (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  root TEXT NOT NULL,
  title TEXT NOT NULL,
  display_order INTEGER NOT NULL CHECK (display_order >= 0)
);
CREATE TABLE workspace_menu_session (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('terminal', 'acp')),
  title TEXT NOT NULL,
  custom_title INTEGER NOT NULL CHECK (custom_title IN (0, 1)),
  cwd TEXT,
  project_root TEXT,
  project_title TEXT,
  project_order INTEGER NOT NULL CHECK (project_order >= 0),
  session_order INTEGER NOT NULL CHECK (session_order >= 0),
  leaf_order INTEGER NOT NULL CHECK (leaf_order >= 0),
  agent TEXT
);
";

/// v17 的完整结构，冻结不再改。v17 之前各版本的迁移和形状校验都以它为准：
/// 那些版本还带着 `kv` 表和旧的 `acp_detail`、`project`。
pub const V17_SCHEMA: &str = "
CREATE TABLE kv (
  scope TEXT NOT NULL,
  key TEXT NOT NULL,
  value BLOB,
  value_type TEXT NOT NULL CHECK (
    value_type IN ('null', 'boolean', 'integer', 'real', 'text', 'object', 'array', 'blob')
  ),
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (scope, key),
  CHECK (
    (value_type = 'null' AND value IS NULL)
    OR (value_type <> 'null' AND value IS NOT NULL)
  )
) WITHOUT ROWID;

CREATE TABLE launch_entry (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  label TEXT NOT NULL,
  command TEXT NOT NULL,
  provider TEXT
);

CREATE TABLE project (
  root TEXT PRIMARY KEY,
  position INTEGER NOT NULL UNIQUE CHECK (position >= 0),
  collapsed INTEGER NOT NULL DEFAULT 0 CHECK (collapsed IN (0, 1))
);

CREATE TABLE session_group (
  id TEXT PRIMARY KEY,
  project_root TEXT REFERENCES project(root) ON DELETE SET NULL,
  position INTEGER NOT NULL UNIQUE CHECK (position >= 0),
  active_session_id TEXT REFERENCES session(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED,
  custom_title TEXT,
  last_updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE session (
  id TEXT PRIMARY KEY,
  group_id TEXT NOT NULL REFERENCES session_group(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('terminal', 'acp')),
  cwd TEXT,
  custom_title TEXT,
  launch_label TEXT,
  launch_command TEXT
);
CREATE INDEX session_group_id_idx ON session(group_id);

CREATE TABLE layout_node (
  id TEXT PRIMARY KEY,
  group_id TEXT NOT NULL REFERENCES session_group(id) ON DELETE CASCADE,
  parent_id TEXT REFERENCES layout_node(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  kind TEXT NOT NULL CHECK (kind IN ('split', 'leaf')),
  axis TEXT CHECK (axis IN ('horizontal', 'vertical') OR axis IS NULL),
  session_id TEXT UNIQUE REFERENCES session(id) ON DELETE CASCADE,
  size_px REAL CHECK (size_px IS NULL OR size_px >= 0),
  UNIQUE (group_id, parent_id, position),
  CHECK (
    (kind = 'split' AND axis IS NOT NULL AND session_id IS NULL)
    OR (kind = 'leaf' AND axis IS NULL AND session_id IS NOT NULL)
  )
);
CREATE UNIQUE INDEX layout_node_root_idx
  ON layout_node(group_id)
  WHERE parent_id IS NULL;

CREATE TABLE acp_detail (
  session_id TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  profile_id TEXT,
  history_session_id TEXT,
  launch_command TEXT NOT NULL,
  refresh_launch_from_settings INTEGER NOT NULL DEFAULT 0
    CHECK (refresh_launch_from_settings IN (0, 1)),
  fork_session_id TEXT,
  fork_title TEXT,
  fork_agent TEXT,
  fork_profile_label TEXT,
  fork_from_history INTEGER NOT NULL DEFAULT 0 CHECK (fork_from_history IN (0, 1)),
  pending_prompt TEXT,
  pending_delivery_id TEXT,
  agent_definition_id TEXT,
  automation_id TEXT,
  CHECK (
    (fork_session_id IS NULL AND fork_title IS NULL)
    OR (fork_session_id IS NOT NULL AND fork_title IS NOT NULL)
  )
);

CREATE TABLE history_title (
  agent TEXT NOT NULL,
  profile_id TEXT NOT NULL DEFAULT '',
  resume_id TEXT NOT NULL,
  custom_title TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (agent, profile_id, resume_id)
) WITHOUT ROWID;

CREATE TABLE event_log (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  topic TEXT NOT NULL,
  aggregate_kind TEXT,
  aggregate_id TEXT,
  aggregate_revision INTEGER,
  occurred_at_ms INTEGER NOT NULL,
  envelope_json BLOB NOT NULL,
  CHECK (
    (aggregate_kind IS NULL AND aggregate_id IS NULL AND aggregate_revision IS NULL)
    OR (aggregate_kind IS NOT NULL AND aggregate_id IS NOT NULL AND aggregate_revision IS NOT NULL)
  )
);
CREATE INDEX event_log_topic_sequence_idx ON event_log(topic, sequence);
CREATE INDEX event_log_aggregate_sequence_idx
  ON event_log(aggregate_kind, aggregate_id, sequence);

CREATE TABLE event_outbox (
  outbox_id INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  envelope_json BLOB NOT NULL,
  created_at_ms INTEGER NOT NULL,
  dispatched_sequence INTEGER REFERENCES event_log(sequence)
);
CREATE INDEX event_outbox_pending_idx
  ON event_outbox(dispatched_sequence, outbox_id);

CREATE TABLE subscription_cursor (
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  last_acked_sequence INTEGER NOT NULL DEFAULT 0 CHECK (last_acked_sequence >= 0),
  declaration_fingerprint TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (plugin_id, subscription_id)
) WITHOUT ROWID;

CREATE TABLE event_dead_letter (
  dead_letter_id INTEGER PRIMARY KEY AUTOINCREMENT,
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  event_id TEXT NOT NULL,
  attempts INTEGER NOT NULL CHECK (attempts > 0),
  reason TEXT NOT NULL,
  failed_at_ms INTEGER NOT NULL,
  UNIQUE(plugin_id, subscription_id, sequence)
);
CREATE INDEX event_dead_letter_subscription_idx
  ON event_dead_letter(plugin_id, subscription_id, dead_letter_id);

CREATE TABLE event_delivery_retry (
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  sequence INTEGER NOT NULL REFERENCES event_log(sequence),
  event_id TEXT NOT NULL,
  attempts INTEGER NOT NULL CHECK (attempts > 0),
  next_attempt_at_ms INTEGER NOT NULL,
  reason TEXT NOT NULL,
  PRIMARY KEY (plugin_id, subscription_id),
  FOREIGN KEY (plugin_id, subscription_id)
    REFERENCES subscription_cursor(plugin_id, subscription_id) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE TABLE command_dedupe (
  plugin_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  completed_at_ms INTEGER NOT NULL,
  result_json BLOB NOT NULL,
  PRIMARY KEY (plugin_id, command_id)
) WITHOUT ROWID;

CREATE TABLE peer_message_command (
  source_session_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  completed_at_ms INTEGER NOT NULL,
  message_id TEXT NOT NULL,
  target_session_id TEXT NOT NULL,
  PRIMARY KEY (source_session_id, command_id)
) WITHOUT ROWID;

CREATE TABLE automation (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  workspace_dir TEXT,
  position INTEGER NOT NULL CHECK (position >= 0),
  trigger_json BLOB NOT NULL,
  action_json BLOB NOT NULL,
  sinks_json BLOB NOT NULL
);
CREATE TABLE automation_state (
  automation_id TEXT PRIMARY KEY REFERENCES automation(id) ON DELETE CASCADE,
  next_run_at INTEGER,
  last_run_id TEXT
);
CREATE TABLE automation_run (
  id TEXT PRIMARY KEY,
  automation_id TEXT NOT NULL REFERENCES automation(id) ON DELETE CASCADE,
  source TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  scheduled_for INTEGER,
  started_at INTEGER,
  delivery_attempt_at INTEGER,
  delivery_attempts INTEGER NOT NULL DEFAULT 0 CHECK (delivery_attempts >= 0),
  finished_at INTEGER,
  session_id TEXT,
  provider_session_id TEXT,
  output TEXT,
  error TEXT,
  runtime_released_at INTEGER,
  context_json BLOB NOT NULL
);
CREATE INDEX automation_run_automation_created_idx
  ON automation_run(automation_id, created_at);
CREATE TABLE automation_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE automation_run_transcript (
  run_id TEXT PRIMARY KEY REFERENCES automation_run(id) ON DELETE CASCADE,
  entries_json BLOB NOT NULL,
  updated_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE agent_ui_pref (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE agent_acp_command (
  command_key TEXT PRIMARY KEY,
  command TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE agent_acp_env (
  engine_kind_id TEXT NOT NULL,
  name TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY (engine_kind_id, name)
) WITHOUT ROWID;
CREATE TABLE agent_acp_config_memory (
  engine_kind_id TEXT NOT NULL,
  config_id TEXT NOT NULL,
  value_id TEXT NOT NULL,
  position INTEGER NOT NULL CHECK (position >= 0),
  PRIMARY KEY (engine_kind_id, config_id)
) WITHOUT ROWID;
CREATE TABLE agent_definition (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  engine_kind_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  plugins_json BLOB NOT NULL,
  mcp_servers_json BLOB NOT NULL DEFAULT X'5B5D',
  context_folders_json BLOB NOT NULL,
  context_links_json BLOB NOT NULL,
  model_provider TEXT NOT NULL DEFAULT '',
  model_id TEXT NOT NULL DEFAULT '',
  memory_enabled INTEGER NOT NULL DEFAULT 1 CHECK (memory_enabled IN (0, 1)),
  position INTEGER NOT NULL CHECK (position >= 0)
);
CREATE TABLE agent_memory_provenance (
  scope TEXT NOT NULL,
  file_name TEXT NOT NULL,
  trust TEXT NOT NULL CHECK (trust IN ('trusted', 'pending')),
  reason TEXT NOT NULL,
  session_id TEXT NOT NULL,
  written_at_ms INTEGER NOT NULL,
  PRIMARY KEY (scope, file_name)
) WITHOUT ROWID;
CREATE TABLE agent_memory_usage (
  scope TEXT NOT NULL,
  file_name TEXT NOT NULL,
  read_count INTEGER NOT NULL CHECK (read_count >= 0),
  last_read_at_ms INTEGER NOT NULL,
  PRIMARY KEY (scope, file_name)
) WITHOUT ROWID;
CREATE TABLE agent_profile (
  id TEXT PRIMARY KEY,
  kind_id TEXT NOT NULL,
  label TEXT NOT NULL,
  workspace_dir TEXT NOT NULL,
  position INTEGER NOT NULL CHECK (position >= 0)
);
CREATE TABLE remote_acp_session (
  id TEXT PRIMARY KEY,
  cwd TEXT NOT NULL,
  title TEXT NOT NULL,
  agent_option_id TEXT NOT NULL,
  agent TEXT NOT NULL,
  launch_command TEXT NOT NULL,
  launch_env_json BLOB NOT NULL,
  resume_id TEXT,
  created_at INTEGER NOT NULL,
  lifecycle TEXT NOT NULL,
  hidden INTEGER NOT NULL CHECK (hidden IN (0, 1))
);
CREATE TABLE remote_terminal_session (
  id TEXT PRIMARY KEY,
  cwd TEXT NOT NULL,
  title TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  lifecycle TEXT NOT NULL
);
CREATE TABLE published_session (
  id TEXT PRIMARY KEY,
  cwd TEXT,
  launch TEXT,
  provider TEXT,
  conversation_id TEXT,
  title TEXT,
  phase TEXT NOT NULL,
  phase_since INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  structured_events INTEGER NOT NULL CHECK (structured_events IN (0, 1)),
  turn_events INTEGER NOT NULL CHECK (turn_events IN (0, 1)),
  agent_event_version INTEGER,
  tokens_used INTEGER,
  branch TEXT,
  dirty_files_json BLOB NOT NULL
);
CREATE TABLE paired_device (
  endpoint_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  paired_at_ms INTEGER NOT NULL,
  last_seen_at_ms INTEGER
) WITHOUT ROWID;
";

pub const MIGRATE_V1_TO_V2: &str = "
CREATE TABLE event_log (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  topic TEXT NOT NULL,
  aggregate_kind TEXT,
  aggregate_id TEXT,
  aggregate_revision INTEGER,
  occurred_at_ms INTEGER NOT NULL,
  envelope_json BLOB NOT NULL,
  CHECK (
    (aggregate_kind IS NULL AND aggregate_id IS NULL AND aggregate_revision IS NULL)
    OR (aggregate_kind IS NOT NULL AND aggregate_id IS NOT NULL AND aggregate_revision IS NOT NULL)
  )
);
CREATE INDEX event_log_topic_sequence_idx ON event_log(topic, sequence);
CREATE INDEX event_log_aggregate_sequence_idx
  ON event_log(aggregate_kind, aggregate_id, sequence);
CREATE TABLE event_outbox (
  outbox_id INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  envelope_json BLOB NOT NULL,
  created_at_ms INTEGER NOT NULL,
  dispatched_sequence INTEGER REFERENCES event_log(sequence)
);
CREATE INDEX event_outbox_pending_idx ON event_outbox(dispatched_sequence, outbox_id);
CREATE TABLE subscription_cursor (
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  last_acked_sequence INTEGER NOT NULL DEFAULT 0 CHECK (last_acked_sequence >= 0),
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (plugin_id, subscription_id)
) WITHOUT ROWID;
CREATE TABLE event_dead_letter (
  dead_letter_id INTEGER PRIMARY KEY AUTOINCREMENT,
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  event_id TEXT NOT NULL,
  attempts INTEGER NOT NULL CHECK (attempts > 0),
  reason TEXT NOT NULL,
  failed_at_ms INTEGER NOT NULL,
  UNIQUE(plugin_id, subscription_id, sequence)
);
CREATE INDEX event_dead_letter_subscription_idx
  ON event_dead_letter(plugin_id, subscription_id, dead_letter_id);
CREATE TABLE command_dedupe (
  plugin_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  completed_at_ms INTEGER NOT NULL,
  result_json BLOB NOT NULL,
  PRIMARY KEY (plugin_id, command_id)
) WITHOUT ROWID;
";

pub const MIGRATE_V2_TO_V3: &str = "
CREATE TABLE peer_message_command (
  source_session_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  completed_at_ms INTEGER NOT NULL,
  message_id TEXT NOT NULL,
  target_session_id TEXT NOT NULL,
  PRIMARY KEY (source_session_id, command_id)
) WITHOUT ROWID;
";

pub const MIGRATE_V3_TO_V4: &str = "
ALTER TABLE subscription_cursor RENAME TO subscription_cursor_v3;
CREATE TABLE subscription_cursor (
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  last_acked_sequence INTEGER NOT NULL DEFAULT 0 CHECK (last_acked_sequence >= 0),
  declaration_fingerprint TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (plugin_id, subscription_id)
) WITHOUT ROWID;
INSERT INTO subscription_cursor(
  plugin_id, subscription_id, last_acked_sequence, declaration_fingerprint, updated_at_ms
)
SELECT
  plugin_id, subscription_id, last_acked_sequence, 'legacy-v3-unbound', updated_at_ms
FROM subscription_cursor_v3;
DROP TABLE subscription_cursor_v3;

ALTER TABLE peer_message_command RENAME TO peer_message_command_v3;
CREATE TABLE peer_message_command (
  source_session_id TEXT NOT NULL,
  command_id TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  completed_at_ms INTEGER NOT NULL,
  message_id TEXT NOT NULL,
  target_session_id TEXT NOT NULL,
  PRIMARY KEY (source_session_id, command_id)
) WITHOUT ROWID;
INSERT INTO peer_message_command(
  source_session_id, command_id, request_fingerprint, completed_at_ms, message_id,
  target_session_id
)
SELECT
  source_session_id, command_id, 'legacy-v3-unbound', completed_at_ms, message_id,
  target_session_id
FROM peer_message_command_v3;
DROP TABLE peer_message_command_v3;
";

pub const MIGRATE_V4_TO_V5: &str = "
CREATE TABLE event_delivery_retry (
  plugin_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  sequence INTEGER NOT NULL REFERENCES event_log(sequence),
  event_id TEXT NOT NULL,
  attempts INTEGER NOT NULL CHECK (attempts > 0),
  next_attempt_at_ms INTEGER NOT NULL,
  reason TEXT NOT NULL,
  PRIMARY KEY (plugin_id, subscription_id),
  FOREIGN KEY (plugin_id, subscription_id)
    REFERENCES subscription_cursor(plugin_id, subscription_id) ON DELETE CASCADE
) WITHOUT ROWID;
";

/// v5 之前 `acp_detail` 带 6 个 Multica 专用列和配套 CHECK。Multica 专用路径已被通用
/// Agent/Session Controller 贡献取代，这些列不再有读写方。CHECK 约束引用了它们，
/// `ALTER TABLE DROP COLUMN` 用不了，只能整表重建；显式列清单让脚本对「已经是新形状」
/// 的库同样安全。
pub const MIGRATE_V5_TO_V6: &str = "
ALTER TABLE acp_detail RENAME TO acp_detail_v5;
CREATE TABLE acp_detail (
  session_id TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  profile_id TEXT,
  history_session_id TEXT,
  launch_command TEXT NOT NULL,
  refresh_launch_from_settings INTEGER NOT NULL DEFAULT 0
    CHECK (refresh_launch_from_settings IN (0, 1)),
  fork_session_id TEXT,
  fork_title TEXT,
  fork_agent TEXT,
  fork_profile_label TEXT,
  fork_from_history INTEGER NOT NULL DEFAULT 0 CHECK (fork_from_history IN (0, 1)),
  pending_prompt TEXT,
  pending_delivery_id TEXT,
  CHECK (
    (fork_session_id IS NULL AND fork_title IS NULL)
    OR (fork_session_id IS NOT NULL AND fork_title IS NOT NULL)
  )
);
INSERT INTO acp_detail(
  session_id, agent, profile_id, history_session_id, launch_command,
  refresh_launch_from_settings, fork_session_id, fork_title, fork_agent,
  fork_profile_label, fork_from_history, pending_prompt, pending_delivery_id
)
SELECT
  session_id, agent, profile_id, history_session_id, launch_command,
  refresh_launch_from_settings, fork_session_id, fork_title, fork_agent,
  fork_profile_label, fork_from_history, pending_prompt, pending_delivery_id
FROM acp_detail_v5;
DROP TABLE acp_detail_v5;
";

/// v7 给 `acp_detail` 补上智能体对话与自动化的归属列。
///
/// v6 之前这两个字段只存在于内存快照里，落盘时被整张表的列清单丢掉，重启后
/// 智能体对话认不出自己的定义，会退化成按 cwd 成组的普通「项目」。
pub const MIGRATE_V6_TO_V7: &str = "
ALTER TABLE acp_detail RENAME TO acp_detail_v6;
CREATE TABLE acp_detail (
  session_id TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  profile_id TEXT,
  history_session_id TEXT,
  launch_command TEXT NOT NULL,
  refresh_launch_from_settings INTEGER NOT NULL DEFAULT 0
    CHECK (refresh_launch_from_settings IN (0, 1)),
  fork_session_id TEXT,
  fork_title TEXT,
  fork_agent TEXT,
  fork_profile_label TEXT,
  fork_from_history INTEGER NOT NULL DEFAULT 0 CHECK (fork_from_history IN (0, 1)),
  pending_prompt TEXT,
  pending_delivery_id TEXT,
  agent_definition_id TEXT,
  automation_id TEXT,
  CHECK (
    (fork_session_id IS NULL AND fork_title IS NULL)
    OR (fork_session_id IS NOT NULL AND fork_title IS NOT NULL)
  )
);
INSERT INTO acp_detail(
  session_id, agent, profile_id, history_session_id, launch_command,
  refresh_launch_from_settings, fork_session_id, fork_title, fork_agent,
  fork_profile_label, fork_from_history, pending_prompt, pending_delivery_id
)
SELECT
  session_id, agent, profile_id, history_session_id, launch_command,
  refresh_launch_from_settings, fork_session_id, fork_title, fork_agent,
  fork_profile_label, fork_from_history, pending_prompt, pending_delivery_id
FROM acp_detail_v6;
DROP TABLE acp_detail_v6;
";

/// v8 为自动化 Run 建立独立对话表。旧版曾把同一份 JSON 写进 `kv` blob
/// 作用域 `automation_run_transcript`，迁移时一并搬过来。
pub const MIGRATE_V7_TO_V8: &str = "
CREATE TABLE automation_run_transcript (
  run_id TEXT PRIMARY KEY,
  entries_json BLOB NOT NULL,
  updated_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
INSERT INTO automation_run_transcript(run_id, entries_json, updated_at_ms)
SELECT key, value, updated_at_ms FROM kv
WHERE scope = 'automation_run_transcript' AND value_type = 'blob';
DELETE FROM kv WHERE scope = 'automation_run_transcript';
";

/// v9 把自动化从 `kv` 里的整份 JSON 文档拆成关系表。对话表随后按 Run 外键重建。
pub const MIGRATE_V8_TO_V9: &str = "
CREATE TABLE automation (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  workspace_dir TEXT,
  position INTEGER NOT NULL CHECK (position >= 0),
  trigger_json BLOB NOT NULL,
  action_json BLOB NOT NULL,
  sinks_json BLOB NOT NULL
);
CREATE TABLE automation_state (
  automation_id TEXT PRIMARY KEY REFERENCES automation(id) ON DELETE CASCADE,
  next_run_at INTEGER,
  last_run_id TEXT
);
CREATE TABLE automation_run (
  id TEXT PRIMARY KEY,
  automation_id TEXT NOT NULL REFERENCES automation(id) ON DELETE CASCADE,
  source TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  scheduled_for INTEGER,
  started_at INTEGER,
  delivery_attempt_at INTEGER,
  delivery_attempts INTEGER NOT NULL DEFAULT 0 CHECK (delivery_attempts >= 0),
  finished_at INTEGER,
  session_id TEXT,
  provider_session_id TEXT,
  output TEXT,
  error TEXT,
  runtime_released_at INTEGER,
  context_json BLOB NOT NULL
);
CREATE INDEX automation_run_automation_created_idx
  ON automation_run(automation_id, created_at);
CREATE TABLE automation_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
) WITHOUT ROWID;
";

/// v10 把智能体配置、远程会话和守护会话目录从 `kv` 文档拆成关系表。
pub const MIGRATE_V9_TO_V10: &str = "
CREATE TABLE agent_ui_pref (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE agent_acp_command (
  command_key TEXT PRIMARY KEY,
  command TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE agent_acp_env (
  agent_id TEXT NOT NULL,
  name TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY (agent_id, name)
) WITHOUT ROWID;
CREATE TABLE agent_acp_config_memory (
  agent_id TEXT NOT NULL,
  config_id TEXT NOT NULL,
  value_id TEXT NOT NULL,
  position INTEGER NOT NULL CHECK (position >= 0),
  PRIMARY KEY (agent_id, config_id)
) WITHOUT ROWID;
CREATE TABLE agent_definition (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  plugins_json BLOB NOT NULL,
  context_folders_json BLOB NOT NULL,
  context_links_json BLOB NOT NULL,
  position INTEGER NOT NULL CHECK (position >= 0)
);
CREATE TABLE agent_profile (
  id TEXT PRIMARY KEY,
  kind_id TEXT NOT NULL,
  label TEXT NOT NULL,
  workspace_dir TEXT NOT NULL,
  position INTEGER NOT NULL CHECK (position >= 0)
);
CREATE TABLE remote_acp_session (
  id TEXT PRIMARY KEY,
  cwd TEXT NOT NULL,
  title TEXT NOT NULL,
  agent_option_id TEXT NOT NULL,
  agent TEXT NOT NULL,
  launch_command TEXT NOT NULL,
  launch_env_json BLOB NOT NULL,
  resume_id TEXT,
  created_at INTEGER NOT NULL,
  lifecycle TEXT NOT NULL,
  hidden INTEGER NOT NULL CHECK (hidden IN (0, 1))
);
CREATE TABLE remote_terminal_session (
  id TEXT PRIMARY KEY,
  cwd TEXT NOT NULL,
  title TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  lifecycle TEXT NOT NULL
);
CREATE TABLE published_session (
  id TEXT PRIMARY KEY,
  cwd TEXT,
  launch TEXT,
  provider TEXT,
  conversation_id TEXT,
  title TEXT,
  phase TEXT NOT NULL,
  phase_since INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  structured_events INTEGER NOT NULL CHECK (structured_events IN (0, 1)),
  turn_events INTEGER NOT NULL CHECK (turn_events IN (0, 1)),
  agent_event_version INTEGER,
  tokens_used INTEGER,
  branch TEXT,
  dirty_files_json BLOB NOT NULL
);
";

/// v11 把三个实际表示 ACP 执行引擎的 `agent_id` 列改为无歧义名称。
pub const MIGRATE_V10_TO_V11: &str = "
ALTER TABLE agent_acp_env RENAME COLUMN agent_id TO engine_kind_id;
ALTER TABLE agent_acp_config_memory RENAME COLUMN agent_id TO engine_kind_id;
ALTER TABLE agent_definition RENAME COLUMN agent_id TO engine_kind_id;
";

/// v12 给智能体定义加上默认模型。重建表是为了 sqlite_schema.sql 与
/// CREATE_SCHEMA 逐字一致，ADD COLUMN 会把列接到 position 后面。
pub const MIGRATE_V11_TO_V12: &str = "
ALTER TABLE agent_definition RENAME TO agent_definition_v11;
CREATE TABLE agent_definition (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  engine_kind_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  plugins_json BLOB NOT NULL,
  context_folders_json BLOB NOT NULL,
  context_links_json BLOB NOT NULL,
  model_provider TEXT NOT NULL DEFAULT '',
  model_id TEXT NOT NULL DEFAULT '',
  position INTEGER NOT NULL CHECK (position >= 0)
);
INSERT INTO agent_definition(
  id, name, description, engine_kind_id, prompt, plugins_json,
  context_folders_json, context_links_json, model_provider, model_id, position
)
SELECT
  id, name, description, engine_kind_id, prompt, plugins_json,
  context_folders_json, context_links_json, '', '', position
FROM agent_definition_v11;
DROP TABLE agent_definition_v11;
";

/// v12 → v13：智能体记忆的元数据（使用统计、来源）。记忆正文仍是 Markdown 文件，
/// 这两张表只存结构化状态，scope 为 `agent:<智能体 id>` 或 `user`。
pub const MIGRATE_V12_TO_V13: &str = "
CREATE TABLE agent_memory_provenance (
  scope TEXT NOT NULL,
  file_name TEXT NOT NULL,
  trust TEXT NOT NULL CHECK (trust IN ('trusted', 'pending')),
  reason TEXT NOT NULL,
  session_id TEXT NOT NULL,
  written_at_ms INTEGER NOT NULL,
  PRIMARY KEY (scope, file_name)
) WITHOUT ROWID;
CREATE TABLE agent_memory_usage (
  scope TEXT NOT NULL,
  file_name TEXT NOT NULL,
  read_count INTEGER NOT NULL CHECK (read_count >= 0),
  last_read_at_ms INTEGER NOT NULL,
  PRIMARY KEY (scope, file_name)
) WITHOUT ROWID;
";

/// v13 → v14：智能体可以关闭记忆。关闭时不注入、不写入，已有记忆文件保留。
pub const MIGRATE_V13_TO_V14: &str = "
ALTER TABLE agent_definition RENAME TO agent_definition_v13;
CREATE TABLE agent_definition (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  engine_kind_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  plugins_json BLOB NOT NULL,
  context_folders_json BLOB NOT NULL,
  context_links_json BLOB NOT NULL,
  model_provider TEXT NOT NULL DEFAULT '',
  model_id TEXT NOT NULL DEFAULT '',
  memory_enabled INTEGER NOT NULL DEFAULT 1 CHECK (memory_enabled IN (0, 1)),
  position INTEGER NOT NULL CHECK (position >= 0)
);
INSERT INTO agent_definition(
  id, name, description, engine_kind_id, prompt, plugins_json,
  context_folders_json, context_links_json, model_provider, model_id, memory_enabled, position
)
SELECT
  id, name, description, engine_kind_id, prompt, plugins_json,
  context_folders_json, context_links_json, model_provider, model_id, 1, position
FROM agent_definition_v13;
DROP TABLE agent_definition_v13;
";

/// v14 → v15：智能体勾选要连接的 MCP 服务器名（`mcp.json` 里 `mcpServers` 的键）。
/// 已有智能体迁移后为空，即不连任何 MCP 服务器。回填值显式转成 BLOB：
/// 文本 '[]' 读成 `Vec<u8>` 时 rusqlite 会报类型不符。
pub const MIGRATE_V14_TO_V15: &str = "
ALTER TABLE agent_definition RENAME TO agent_definition_v14;
CREATE TABLE agent_definition (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  engine_kind_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  plugins_json BLOB NOT NULL,
  mcp_servers_json BLOB NOT NULL DEFAULT '[]',
  context_folders_json BLOB NOT NULL,
  context_links_json BLOB NOT NULL,
  model_provider TEXT NOT NULL DEFAULT '',
  model_id TEXT NOT NULL DEFAULT '',
  memory_enabled INTEGER NOT NULL DEFAULT 1 CHECK (memory_enabled IN (0, 1)),
  position INTEGER NOT NULL CHECK (position >= 0)
);
INSERT INTO agent_definition(
  id, name, description, engine_kind_id, prompt, plugins_json, mcp_servers_json,
  context_folders_json, context_links_json, model_provider, model_id, memory_enabled, position
)
SELECT
  id, name, description, engine_kind_id, prompt, plugins_json, CAST('[]' AS BLOB),
  context_folders_json, context_links_json, model_provider, model_id, memory_enabled, position
FROM agent_definition_v14;
DROP TABLE agent_definition_v14;
";

/// v15 → v16：`agent_definition.mcp_servers_json` 的默认值从文本 `'[]'` 改成 BLOB `X'5B5D'`，
/// 与列类型一致。SQLite 改不了已有列的默认值，只能重建表；列顺序和其余定义不变。
pub const MIGRATE_V15_TO_V16: &str = "
ALTER TABLE agent_definition RENAME TO agent_definition_v15;
CREATE TABLE agent_definition (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  engine_kind_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  plugins_json BLOB NOT NULL,
  mcp_servers_json BLOB NOT NULL DEFAULT X'5B5D',
  context_folders_json BLOB NOT NULL,
  context_links_json BLOB NOT NULL,
  model_provider TEXT NOT NULL DEFAULT '',
  model_id TEXT NOT NULL DEFAULT '',
  memory_enabled INTEGER NOT NULL DEFAULT 1 CHECK (memory_enabled IN (0, 1)),
  position INTEGER NOT NULL CHECK (position >= 0)
);
INSERT INTO agent_definition(
  id, name, description, engine_kind_id, prompt, plugins_json, mcp_servers_json,
  context_folders_json, context_links_json, model_provider, model_id, memory_enabled, position
)
SELECT
  id, name, description, engine_kind_id, prompt, plugins_json, mcp_servers_json,
  context_folders_json, context_links_json, model_provider, model_id, memory_enabled, position
FROM agent_definition_v15;
DROP TABLE agent_definition_v15;
";

/// v16 → v17：已配对设备。远程客户端按 iroh endpoint id 进白名单（设计文档 D3）。
pub const MIGRATE_V16_TO_V17: &str = "
CREATE TABLE paired_device (
  endpoint_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  paired_at_ms INTEGER NOT NULL,
  last_seen_at_ms INTEGER
) WITHOUT ROWID;
";

/// v17 → v18：运行时库不再有通用 `kv`，各领域按字段建表（表结构的部分）。
///
/// 旧 `kv` 和 `agent_ui_pref` 里的数据由存储初始化器先读进内存，执行完这段脚本再写进新表；
/// 归客户端的界面状态（外观、更新、通知开关、侧栏、会话 route）不搬，随 `kv` 一起删掉。
/// `acp_detail` 去掉早已不写的分叉列和配套 CHECK，补上原来散在 `kv` 里的会话字段，
/// 只能整表重建；`project.collapsed` 是客户端的折叠状态，直接删列。
pub const MIGRATE_V17_TO_V18: &str = "
ALTER TABLE acp_detail RENAME TO acp_detail_v17;
CREATE TABLE acp_detail (
  session_id TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  profile_id TEXT,
  history_session_id TEXT,
  launch_command TEXT NOT NULL,
  refresh_launch_from_settings INTEGER NOT NULL DEFAULT 0
    CHECK (refresh_launch_from_settings IN (0, 1)),
  pending_prompt TEXT,
  pending_delivery_id TEXT,
  agent_definition_id TEXT,
  automation_id TEXT,
  binding_automation_run_id TEXT,
  pending_agent_preset TEXT,
  session_title TEXT
);
CREATE TABLE acp_launch_env (
  session_id TEXT NOT NULL REFERENCES acp_detail(session_id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY (session_id, name)
) WITHOUT ROWID;
CREATE TABLE acp_config_value (
  session_id TEXT NOT NULL REFERENCES acp_detail(session_id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  config_id TEXT NOT NULL,
  value_id TEXT NOT NULL,
  PRIMARY KEY (session_id, position)
) WITHOUT ROWID;
INSERT INTO acp_detail(
  session_id, agent, profile_id, history_session_id, launch_command,
  refresh_launch_from_settings, pending_prompt, pending_delivery_id, agent_definition_id,
  automation_id
)
SELECT
  session_id, agent, profile_id, history_session_id, launch_command,
  refresh_launch_from_settings, pending_prompt, pending_delivery_id, agent_definition_id,
  automation_id
FROM acp_detail_v17;
DROP TABLE acp_detail_v17;
ALTER TABLE project DROP COLUMN collapsed;
CREATE TABLE agent_host_pref (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  hooks_enabled INTEGER NOT NULL CHECK (hooks_enabled IN (0, 1)),
  cross_agent_enabled INTEGER NOT NULL CHECK (cross_agent_enabled IN (0, 1))
);
DROP TABLE agent_ui_pref;
CREATE TABLE runtime_identity (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  runtime_id TEXT NOT NULL
);
CREATE TABLE workspace_catalog (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  revision INTEGER NOT NULL CHECK (revision >= 0)
);
CREATE TABLE launch_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL CHECK (version >= 0)
);
CREATE TABLE history_agent_binding (
  agent TEXT NOT NULL,
  profile_id TEXT NOT NULL DEFAULT '',
  resume_id TEXT NOT NULL,
  agent_definition_id TEXT NOT NULL,
  PRIMARY KEY (agent, profile_id, resume_id)
) WITHOUT ROWID;
CREATE TABLE default_conversation_agent (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  agent_definition_id TEXT NOT NULL
);
CREATE TABLE new_session_pin (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  action_key TEXT NOT NULL UNIQUE
);
CREATE TABLE memory_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  embedding_base_url TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_api_key TEXT NOT NULL,
  rerank_model_provider TEXT NOT NULL,
  rerank_model_id TEXT NOT NULL
);
CREATE TABLE remote_config (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  iroh_relay TEXT NOT NULL
);
CREATE TABLE worktree_inherit (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
);
CREATE TABLE worktree_inherit_skip_pattern (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  pattern TEXT NOT NULL
);
CREATE TABLE terminal_theme (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL CHECK (version >= 0),
  dark INTEGER NOT NULL CHECK (dark IN (0, 1)),
  background INTEGER NOT NULL,
  foreground INTEGER NOT NULL,
  cursor INTEGER NOT NULL,
  selection INTEGER NOT NULL,
  search_hit INTEGER NOT NULL,
  search_hit_current INTEGER NOT NULL
);
CREATE TABLE terminal_theme_palette (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  color INTEGER NOT NULL
);
CREATE TABLE auto_model_provider (
  harness TEXT NOT NULL CHECK (harness IN ('dsh', 'pi')),
  provider TEXT NOT NULL,
  PRIMARY KEY (harness, provider)
) WITHOUT ROWID;
CREATE TABLE quota_cache (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  saved_at_ms INTEGER NOT NULL,
  schema_version INTEGER NOT NULL CHECK (schema_version >= 0)
);
CREATE TABLE quota_cache_provider (
  provider_id TEXT PRIMARY KEY
) WITHOUT ROWID;
CREATE TABLE quota_cache_status (
  provider_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('windows', 'copilot')),
  checked_at_ms INTEGER NOT NULL,
  error TEXT,
  copilot_unit TEXT CHECK (copilot_unit IN ('ai_credits', 'premium_requests')),
  copilot_remaining REAL,
  copilot_limit REAL,
  copilot_used REAL,
  CHECK (
    (kind = 'windows' AND copilot_unit IS NULL AND copilot_remaining IS NULL
      AND copilot_limit IS NULL AND copilot_used IS NULL)
    OR (kind = 'copilot' AND copilot_unit IS NOT NULL AND copilot_remaining IS NOT NULL)
  )
) WITHOUT ROWID;
CREATE TABLE quota_cache_window (
  provider_id TEXT NOT NULL REFERENCES quota_cache_status(provider_id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  label TEXT NOT NULL,
  used_percent REAL NOT NULL,
  resets_at_ms INTEGER,
  reset_after_seconds INTEGER,
  PRIMARY KEY (provider_id, position)
) WITHOUT ROWID;
CREATE TABLE workspace_menu (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL CHECK (version >= 0),
  revision INTEGER NOT NULL CHECK (revision >= 0),
  source_id TEXT NOT NULL,
  source_revision INTEGER NOT NULL CHECK (source_revision >= 0)
);
CREATE TABLE workspace_menu_project (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  root TEXT NOT NULL,
  title TEXT NOT NULL,
  display_order INTEGER NOT NULL CHECK (display_order >= 0)
);
CREATE TABLE workspace_menu_session (
  position INTEGER PRIMARY KEY CHECK (position >= 0),
  id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('terminal', 'acp')),
  title TEXT NOT NULL,
  custom_title INTEGER NOT NULL CHECK (custom_title IN (0, 1)),
  cwd TEXT,
  project_root TEXT,
  project_title TEXT,
  project_order INTEGER NOT NULL CHECK (project_order >= 0),
  session_order INTEGER NOT NULL CHECK (session_order >= 0),
  leaf_order INTEGER NOT NULL CHECK (leaf_order >= 0),
  agent TEXT
);
DROP TABLE kv;
";
