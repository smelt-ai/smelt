//! 首个发布版本的库结构。以后改已发布的表时：
//! 1. 同步改这份 `CREATE_SCHEMA`（新库一步建齐）；
//! 2. `STORE_SCHEMA_VERSION` 加一；
//! 3. 在存储初始化器中补对应版本的事务迁移。
//!
//! 迁移必须先读出旧数据，再替换结构；认不出的库一律拒绝打开。

pub const TABLES: [&str; 29] = [
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
  context_folders_json BLOB NOT NULL,
  context_links_json BLOB NOT NULL,
  model_provider TEXT NOT NULL DEFAULT '',
  model_id TEXT NOT NULL DEFAULT '',
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
