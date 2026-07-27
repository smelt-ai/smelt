# Terminal Agent Resume Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore non-ACP Claude Code, Codex, Copilot, and Grok conversations after smeltd loses their PTY process, without silently creating a new conversation.

**Architecture:** Add transport-neutral terminal agent metadata, keep private history discovery and command adaptation in `session_history.rs`, and persist the bound agent session ID with each terminal leaf. Replace the daemon's implicit "missing sid means run launch" behavior with an explicit missing-session action so live sessions attach, ordinary shells recreate, resumable agents run their resume command, and unsafe restoration is rejected.

**Tech Stack:** Rust 2021, serde/serde_json, GPUI, smol, portable-pty, Unix sockets, Cargo tests.

---

## File map

- Modify `crates/smelt-core/src/agent_kind.rs`: define serializable
  `TerminalAgentKind` and `TerminalResumeState`.
- Modify `crates/smelt/src/settings.rs`: persist an optional terminal agent kind
  on launch entries, add Grok to defaults, and expose the selector in settings.
- Modify `crates/smelt/src/session_history.rs`: centralize initial/resume command
  generation, history existence, snapshots, and unique Codex discovery.
- Modify `crates/smelt/src/terminal.rs`: send an explicit missing-session action
  in terminal open handshakes and surface daemon rejection.
- Modify `crates/smeltd/src/main.rs`: parse and enforce missing-session actions
  only when the requested sid is absent.
- Modify `crates/smelt/src/terminal_view.rs`: retain the original launch metadata
  and terminal resume state; use safe actions on reconnect.
- Modify `crates/smelt/src/main.rs`: persist resume state, build safe cold
  restore actions, use them for daemon hard restart, and save asynchronous
  Codex binding.
- Modify `crates/smelt/src/session_list.rs`: pass the selected terminal agent
  kind into initial launch preparation.
- Modify `crates/smelt/src/tasks.rs`: prepare task-triggered agent launches with
  the same identity binding instead of bypassing the adapter.

### Task 1: Add terminal agent metadata and launch configuration

**Files:**
- Modify: `crates/smelt-core/src/agent_kind.rs`
- Modify: `crates/smelt/src/settings.rs:136-235,1478-1485,1760-1855,2328-2405`

- [ ] **Step 1: Write failing metadata and compatibility tests**

Add to `crates/smelt-core/src/agent_kind.rs` tests:

```rust
#[test]
fn terminal_agent_kind_roundtrips_by_stable_id() {
    for kind in TerminalAgentKind::ALL {
        assert_eq!(TerminalAgentKind::from_id(kind.id()), Some(kind));
    }
    assert_eq!(TerminalAgentKind::from_id("unknown"), None);
}

#[test]
fn terminal_resume_state_roundtrips_json() {
    let state = TerminalResumeState {
        agent: TerminalAgentKind::Claude,
        session_id: "session-1".into(),
    };
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(
        serde_json::from_str::<TerminalResumeState>(&json).unwrap(),
        state
    );
}
```

Add to `crates/smelt/src/settings.rs` tests:

```rust
#[test]
fn old_launch_entry_without_agent_kind_stays_plain_terminal() {
    let entry: LaunchEntry =
        serde_json::from_str(r#"{"label":"custom","command":"my-agent"}"#).unwrap();
    assert_eq!(entry.agent_kind, None);
}

#[test]
fn default_launch_entries_enable_all_supported_terminal_agents() {
    let kinds = default_launch_entries()
        .into_iter()
        .filter_map(|entry| entry.agent_kind)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(kinds, TerminalAgentKind::ALL.into_iter().collect());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p smelt-core terminal_agent_kind
cargo test -p smelt old_launch_entry_without_agent_kind_stays_plain_terminal
```

Expected: compilation fails because the terminal agent types and
`LaunchEntry::agent_kind` do not exist.

- [ ] **Step 3: Define stable terminal agent types**

Add to `crates/smelt-core/src/agent_kind.rs`:

```rust
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAgentKind {
    Claude,
    Codex,
    Copilot,
    Grok,
}

impl TerminalAgentKind {
    pub const ALL: [Self; 4] = [
        Self::Claude,
        Self::Codex,
        Self::Copilot,
        Self::Grok,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
            Self::Grok => "grok",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Copilot => "Copilot",
            Self::Grok => "Grok",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalResumeState {
    pub agent: TerminalAgentKind,
    pub session_id: String,
    #[serde(default)]
    pub workspace_dir: Option<String>,
}
```

- [ ] **Step 4: Extend launch entries and defaults**

Change `LaunchEntry`:

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct LaunchEntry {
    pub label: String,
    pub command: String,
    #[serde(default)]
    pub agent_kind: Option<smelt_core::agent_kind::TerminalAgentKind>,
}
```

Set the existing three defaults to their matching kind and add:

```rust
LaunchEntry {
    label: "Grok".into(),
    command: "grok".into(),
    agent_kind: Some(TerminalAgentKind::Grok),
}
```

New custom rows use `agent_kind: None`.

- [ ] **Step 5: Add the launch-entry agent selector**

Add:

```rust
pub fn set_launch_agent_kind(
    &mut self,
    index: usize,
    kind: Option<TerminalAgentKind>,
    cx: &mut Context<Self>,
) {
    apply_launch_config(
        move |config| {
            if let Some(entry) = config.entries.get_mut(index) {
                entry.agent_kind = kind;
            }
        },
        cx,
    );
    cx.notify();
}
```

In the launch editor row, add a button column labeled `普通终端` for `None` or
`kind.label()` for `Some(kind)`. Its dropdown contains `普通终端` followed by
`TerminalAgentKind::ALL`; each callback calls `set_launch_agent_kind(row_ix,
choice, cx)`. Reuse the existing profile-kind dropdown pattern at
`settings.rs:2847-2864` and reduce `cmd_w` by the selector width.

- [ ] **Step 6: Run tests and checks**

Run:

```bash
cargo test -p smelt-core terminal_agent_kind
cargo test -p smelt old_launch_entry_without_agent_kind_stays_plain_terminal
cargo test -p smelt default_launch_entries_enable_all_supported_terminal_agents
cargo check -p smelt
```

Expected: all selected tests pass and `smelt` checks.

- [ ] **Step 7: Commit metadata**

```bash
git add crates/smelt-core/src/agent_kind.rs crates/smelt/src/settings.rs
git commit -m "feat(terminal): configure resumable agent launches

Refs #4

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 2: Build terminal agent commands and history bindings

**Files:**
- Modify: `crates/smelt/src/session_history.rs`
- Modify: `crates/smelt/Cargo.toml`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

- [ ] **Step 1: Add the direct shell parser dependency**

Add `shell-words = "1"` to workspace dependencies in the root `Cargo.toml` and
`shell-words.workspace = true` to `crates/smelt/Cargo.toml`.

- [ ] **Step 2: Write failing adapter tests**

Add a `terminal_resume_tests` module to `session_history.rs`:

```rust
#[test]
fn generated_id_agents_receive_safe_initial_arguments() {
    let id = "11111111-1111-4111-8111-111111111111";
    assert_eq!(
        prepare_terminal_agent_launch(
            TerminalAgentKind::Claude,
            "claude --dangerously-skip-permissions",
            id,
        )
        .unwrap(),
        PreparedTerminalAgentLaunch::Known {
            command: format!(
                "claude --dangerously-skip-permissions --session-id {id}"
            ),
            state: TerminalResumeState {
                agent: TerminalAgentKind::Claude,
                session_id: id.into(),
                workspace_dir: None,
            },
        }
    );
    assert!(matches!(
        prepare_terminal_agent_launch(TerminalAgentKind::Copilot, "copilot --allow-all", id),
        Ok(PreparedTerminalAgentLaunch::Known { .. })
    ));
    assert!(matches!(
        prepare_terminal_agent_launch(TerminalAgentKind::Grok, "grok", id),
        Ok(PreparedTerminalAgentLaunch::Known { .. })
    ));
}

#[test]
fn codex_waits_for_history_assigned_id() {
    assert_eq!(
        prepare_terminal_agent_launch(
            TerminalAgentKind::Codex,
            "codex --dangerously-bypass-approvals-and-sandbox",
            "unused",
        )
        .unwrap(),
        PreparedTerminalAgentLaunch::Discover {
            command: "codex --dangerously-bypass-approvals-and-sandbox".into(),
        }
    );
}

#[test]
fn resume_commands_keep_original_flags() {
    assert_eq!(
        terminal_resume_command(
            TerminalAgentKind::Claude,
            "claude --dangerously-skip-permissions",
            "sid-1",
        )
        .unwrap(),
        "claude --dangerously-skip-permissions --resume sid-1"
    );
    assert_eq!(
        terminal_resume_command(TerminalAgentKind::Copilot, "copilot --allow-all", "sid-1")
            .unwrap(),
        "copilot --allow-all --resume=sid-1"
    );
    assert_eq!(
        terminal_resume_command(TerminalAgentKind::Grok, "grok --yolo", "sid-1").unwrap(),
        "grok --yolo --resume sid-1"
    );
    assert_eq!(
        terminal_resume_command(
            TerminalAgentKind::Codex,
            "codex --dangerously-bypass-approvals-and-sandbox",
            "sid-1",
        )
        .unwrap(),
        "codex resume sid-1 --dangerously-bypass-approvals-and-sandbox"
    );
}

#[test]
fn unsafe_or_mismatched_commands_are_rejected() {
    assert!(terminal_resume_command(
        TerminalAgentKind::Claude,
        "claude; rm -rf /tmp/example",
        "sid-1"
    )
    .is_err());
    assert!(terminal_resume_command(TerminalAgentKind::Claude, "codex", "sid-1").is_err());
    assert!(prepare_terminal_agent_launch(
        TerminalAgentKind::Claude,
        "claude --resume old",
        "sid-1"
    )
    .is_err());
}

#[test]
fn workspace_prefix_is_preserved_and_bound_to_identity() {
    let prepared = prepare_terminal_agent_launch(
        TerminalAgentKind::Claude,
        "CLAUDE_CONFIG_DIR=~/.claude-alt claude",
        "11111111-1111-4111-8111-111111111111",
    )
    .unwrap();
    let PreparedTerminalAgentLaunch::Known { command, state } = prepared else {
        panic!("Claude should have a known ID");
    };
    assert!(command.starts_with("CLAUDE_CONFIG_DIR="));
    assert!(state.workspace_dir.unwrap().ends_with("/.claude-alt"));
}

#[test]
fn codex_delta_requires_exactly_one_new_id() {
    let baseline = ids(&["old"]);
    assert_eq!(
        discover_unique_session_id(&baseline, &ids(&["old", "new"])),
        Ok(Some("new".into()))
    );
    assert_eq!(discover_unique_session_id(&baseline, &baseline), Ok(None));
    assert!(discover_unique_session_id(&baseline, &ids(&["old", "a", "b"])).is_err());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test -p smelt terminal_resume_tests
```

Expected: compilation fails because the adapter API does not exist.

- [ ] **Step 4: Implement safe command adaptation**

Add:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PreparedTerminalAgentLaunch {
    Known {
        command: String,
        state: TerminalResumeState,
    },
    Discover {
        command: String,
    },
}

fn safe_agent_command(
    kind: TerminalAgentKind,
    command: &str,
) -> Result<Vec<String>, String> {
    if command.chars().any(|ch| matches!(ch, ';' | '|' | '&' | '\n' | '\r')) {
        return Err("自动恢复不支持 shell 管道或控制符".into());
    }
    let words = shell_words::split(command)
        .map_err(|error| format!("启动命令解析失败：{error}"))?;
    let executable = words
        .iter()
        .find(|word| smelt_core::workspace_override::split_env_assignment(word).is_none())
        .ok_or_else(|| "启动命令为空".to_string())?;
    let basename = std::path::Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(executable);
    if basename != kind.id() {
        return Err(format!("启动命令不是 {}", kind.label()));
    }
    Ok(words)
}

fn quote_words(words: impl IntoIterator<Item = String>) -> String {
    shell_words::join(words)
}
```

`prepare_terminal_agent_launch` rejects existing `--resume`, `--continue`, and
`--session-id` flags. It obtains the supported workspace override with
`config_dir_env_var(kind.id())` plus `env_override_from_cmd`, stores it on the
resume state, and preserves all leading assignments. It returns `Discover`
unchanged for Codex. For the other agents it appends `--session-id`, quotes
the words, and returns `Known`.

`terminal_resume_command` parses the original command again. It appends the
agent-specific resume flag for Claude, Copilot, and Grok. For Codex it inserts
`resume` and the ID immediately after the executable.

- [ ] **Step 5: Add shared history lookup helpers**

Add:

```rust
pub(crate) fn terminal_session_ids(
    agent: TerminalAgentKind,
    cwd: &str,
    override_dir: Option<&str>,
) -> std::collections::HashSet<String> {
    list_terminal_sessions(agent, cwd, override_dir)
        .into_iter()
        .map(|session| session.resume_id)
        .collect()
}

pub(crate) fn terminal_session_exists(
    state: &TerminalResumeState,
    cwd: &str,
    override_dir: Option<&str>,
) -> bool {
    terminal_session_ids(state.agent, cwd, override_dir).contains(&state.session_id)
}

pub(crate) fn discover_unique_session_id(
    baseline: &std::collections::HashSet<String>,
    current: &std::collections::HashSet<String>,
) -> Result<Option<String>, String> {
    let mut added = current.difference(baseline);
    let first = added.next().cloned();
    if added.next().is_some() {
        return Err("发现多个新 Codex 会话，无法确定当前终端对应哪一个".into());
    }
    Ok(first)
}
```

`list_terminal_sessions` dispatches to the existing four list functions and
does not duplicate any private history path. Every call passes
`state.workspace_dir.as_deref()` (or the workspace override captured before a
Codex launch), never an unconditional `None`.

- [ ] **Step 6: Run adapter tests and checks**

Run:

```bash
cargo test -p smelt terminal_resume_tests
cargo test -p smelt session_history::tests
cargo check -p smelt
```

Expected: all tests pass.

- [ ] **Step 7: Commit adapters**

```bash
git add Cargo.toml Cargo.lock crates/smelt/Cargo.toml crates/smelt/src/session_history.rs
git commit -m "feat(terminal): add agent resume adapters

Refs #4

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 3: Make missing terminal-session behavior explicit

**Files:**
- Modify: `crates/smelt/src/terminal.rs:1614-1812`
- Modify: `crates/smeltd/src/main.rs:3354-3405`

- [ ] **Step 1: Write failing client handshake tests**

Add to terminal tests:

```rust
#[test]
fn open_request_serializes_missing_session_action() {
    let value = open_request(
        24,
        80,
        Some("/tmp/project"),
        "sid-1",
        &MissingSessionAction::Launch("claude --resume old".into()),
    );
    assert_eq!(value["missing"]["kind"], "launch");
    assert_eq!(value["missing"]["command"], "claude --resume old");
}

#[test]
fn rejected_handshake_still_builds_a_terminal_view() {
    let response = serde_json::json!({
        "ok": false,
        "rejected": "无法恢复：历史不存在",
        "rows": 24,
        "cols": 80,
        "replay_len": 0
    });
    assert_eq!(
        parse_open_response(&response, 24, 80).unwrap(),
        (TermSize { rows: 24, cols: 80 }, 0)
    );
}
```

- [ ] **Step 2: Write failing daemon action tests**

Add to smeltd tests:

```rust
#[test]
fn legacy_open_request_preserves_launch_behavior() {
    assert_eq!(
        parse_missing_session_action(&serde_json::json!({"launch":"claude"})),
        MissingSessionAction::Launch("claude".into())
    );
}

#[test]
fn reject_action_never_produces_a_spawn_command() {
    assert_eq!(
        parse_missing_session_action(&serde_json::json!({
            "missing":{"kind":"reject","reason":"无法恢复"}
        })),
        MissingSessionAction::Reject("无法恢复".into())
    );
}

#[test]
fn existing_session_ignores_reject_action() {
    assert_eq!(
        select_missing_session_command(
            true,
            MissingSessionAction::Reject("must not run".into())
        ),
        MissingSessionDecision::Attach
    );
}

#[test]
fn rejected_terminal_reply_contains_actionable_reason() {
    let reply = rejected_terminal_reply("无法恢复：历史不存在", 24, 80);
    assert!(reply.starts_with(
        br#"{"cols":80,"ok":false,"rejected":"无法恢复：历史不存在","replay_len":0,"rows":24}"#
    ));
    assert!(String::from_utf8_lossy(&reply).contains("无法恢复：历史不存在"));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test -p smelt open_request_serializes_missing_session_action
cargo test -p smeltd missing_session_action
```

Expected: compilation fails because the action types and helpers do not exist.

- [ ] **Step 4: Add the client action type and wire shape**

In `terminal.rs` add:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MissingSessionAction {
    Shell,
    Launch(String),
    Reject(String),
}
```

Add `Terminal::spawn_with_action`, receiving `&MissingSessionAction`, and
change `handshake_on` to receive that action. Keep `Terminal::spawn` as the
backward-compatible wrapper used by ordinary existing call sites:

```rust
pub fn spawn(
    rows: usize,
    cols: usize,
    cwd: Option<&str>,
    id: &str,
    launch: Option<&str>,
) -> anyhow::Result<Self> {
    let action = launch
        .map(|command| MissingSessionAction::Launch(command.to_string()))
        .unwrap_or(MissingSessionAction::Shell);
    Self::spawn_with_action(rows, cols, cwd, id, &action)
}
```

`open_request` serializes:

```rust
match action {
    MissingSessionAction::Shell => serde_json::json!({"kind":"shell"}),
    MissingSessionAction::Launch(command) => {
        serde_json::json!({"kind":"launch","command":command})
    }
    MissingSessionAction::Reject(reason) => {
        serde_json::json!({"kind":"reject","reason":reason})
    }
}
```

Keep `"launch"` in the request for one release when the action is `Launch`, so
old daemons retain initial-launch compatibility. Extract `parse_open_response`; both normal and rejected handshakes return the
advertised size and replay length. The bytes after the first JSON line remain
in the buffered stream and are consumed by the normal terminal reader.

- [ ] **Step 5: Enforce the action in smeltd**

Add private daemon equivalents:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
enum MissingSessionAction {
    Shell,
    Launch(String),
    Reject(String),
}

enum MissingSessionDecision {
    Attach,
    Spawn(Option<String>),
    Reject(String),
}
```

`parse_missing_session_action` prefers the structured `missing` object and
falls back to legacy `"launch"` or `Shell`.

`select_missing_session_command(existing, action)` returns `Attach` whenever
the sid exists. For a missing sid it maps `Shell` to `Spawn(None)`, `Launch`
to `Spawn(Some(command))`, and `Reject` to `Reject(reason)`.

Refactor `handle_open` to call this helper before `spawn_session`. On rejection,
call:

```rust
fn write_rejected_terminal(
    mut conn: &UnixStream,
    reason: &str,
    rows: u16,
    cols: u16,
) {
    let reply = rejected_terminal_reply(reason, rows, cols);
    let _ = conn.write_all(&reply);
}
```

`rejected_terminal_reply` produces one JSON handshake line followed by a
red ANSI terminal message:

```rust
let header = serde_json::json!({
    "ok": false,
    "rejected": reason,
    "rows": rows,
    "cols": cols,
    "replay_len": 0,
});
format!("{header}\n\r\n\x1b[31m{reason}\x1b[0m\r\n").into_bytes()
```

Return without spawning or inserting a daemon session. This keeps the restored
tab visible, preserves its metadata, and allows a later reconnect to retry.
Use the same reply for `spawn_session` errors, with
`终端启动失败：{error:#}` as the reason.

- [ ] **Step 6: Run protocol tests and checks**

Run:

```bash
cargo test -p smelt open_request_serializes_missing_session_action
cargo test -p smelt rejected_handshake_still_builds_a_terminal_view
cargo test -p smeltd missing_session_action
cargo test -p smeltd rejected_terminal_reply_contains_actionable_reason
cargo check -p smelt
cargo check -p smeltd
```

Expected: tests and checks pass.

- [ ] **Step 7: Commit protocol behavior**

```bash
git add crates/smelt/src/terminal.rs crates/smeltd/src/main.rs
git commit -m "feat(terminal): enforce missing-session restore actions

Refs #4

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 4: Persist terminal resume state and restore safely

**Files:**
- Modify: `crates/smelt/src/terminal_view.rs:121-218,289-345`
- Modify: `crates/smelt/src/main.rs:994-1022,1310-1425,4120-4230,7534-7595`

- [ ] **Step 1: Write failing workspace serialization tests**

Extend the existing `leaf_custom_title_roundtrips` test:

```rust
let resume = TerminalResumeState {
    agent: TerminalAgentKind::Claude,
    session_id: "agent-session-1".into(),
};
let leaf = PaneState::Leaf {
    cwd: Some("/tmp/x".into()),
    id: Some("sid-1".into()),
    custom_title: Some("跑测试的终端".into()),
    launch_label: Some("Claude Code".into()),
    launch_cmd: Some("claude --dangerously-skip-permissions".into()),
    agent_kind: Some(TerminalAgentKind::Claude),
    resume_state: Some(resume.clone()),
};
// After deserialization:
assert_eq!(resume_state, Some(resume));
```

Add:

```rust
#[test]
fn old_leaf_without_resume_state_remains_compatible() {
    let json = r#"{
      "Leaf":{
        "cwd":"/tmp/x",
        "id":"sid-1",
        "custom_title":null,
        "launch_label":"Claude Code",
        "launch_cmd":"claude"
      }
    }"#;
    let leaf: PaneState = serde_json::from_str(json).unwrap();
    assert!(matches!(leaf, PaneState::Leaf { resume_state: None, .. }));
}
```

- [ ] **Step 2: Write failing restore-action tests**

Add pure helper tests:

```rust
#[test]
fn ordinary_shell_restore_recreates_shell() {
    assert_eq!(
        terminal_restore_action(None, None, None, Some("/tmp/x"), true),
        MissingSessionAction::Shell
    );
}

#[test]
fn resumable_agent_restore_uses_resume_command() {
    let state = TerminalResumeState {
        agent: TerminalAgentKind::Claude,
        session_id: "sid-1".into(),
    };
    let action = terminal_restore_action(
        Some(TerminalAgentKind::Claude),
        Some("claude --dangerously-skip-permissions"),
        Some(&state),
        Some("/tmp/x"),
        true,
    );
    assert!(matches!(
        action,
        MissingSessionAction::Launch(command) if command.contains("--resume sid-1")
    ));
}

#[test]
fn agent_without_identity_is_rejected_instead_of_relaunched() {
    assert!(matches!(
        terminal_restore_action(
            Some(TerminalAgentKind::Claude),
            Some("claude"),
            None,
            Some("/tmp/x"),
            false,
        ),
        MissingSessionAction::Reject(_)
    ));
}

#[test]
fn missing_history_is_rejected_instead_of_relaunched() {
    let state = TerminalResumeState {
        agent: TerminalAgentKind::Claude,
        session_id: "missing".into(),
    };
    assert!(matches!(
        terminal_restore_action(
            Some(TerminalAgentKind::Claude),
            Some("claude"),
            Some(&state),
            Some("/tmp/x"),
            false,
        ),
        MissingSessionAction::Reject(_)
    ));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test -p smelt leaf_custom_title_roundtrips
cargo test -p smelt terminal_restore_action
```

Expected: compilation fails because persisted resume state and helper do not
exist.

- [ ] **Step 4: Persist resume state on leaves and views**

Add to `PaneState::Leaf`:

```rust
#[serde(default)]
agent_kind: Option<TerminalAgentKind>,
#[serde(default)]
resume_state: Option<TerminalResumeState>,
```

Add matching fields, getters, and a setter to `TerminalView`. Its constructor
receives the original launch command, launch label, agent kind, and resume
state. Extend `LaunchKind` with `Grok` and derive it from `agent_kind` when
available, falling back to the existing command classifier for old callers.
`pane_to_state` copies all metadata back into the leaf. Update every
`TerminalView::from_terminal` call site; ordinary shell and legacy task call
sites pass `None, None` for the two new fields.

Keep the original launch command separate from the actual initial/resume
command sent to smeltd.

- [ ] **Step 5: Build safe cold-restore actions**

Implement:

```rust
fn terminal_restore_action(
    agent_kind: Option<TerminalAgentKind>,
    original_launch: Option<&str>,
    resume_state: Option<&TerminalResumeState>,
    cwd: Option<&str>,
    history_exists: bool,
) -> MissingSessionAction {
    match agent_kind {
        None => original_launch
            .map(|command| MissingSessionAction::Launch(command.to_string()))
            .unwrap_or(MissingSessionAction::Shell),
        Some(_) => {
            let Some(state) = resume_state else {
                return MissingSessionAction::Reject(
                    "无法恢复：没有记录这个 agent 的历史会话 ID".into(),
                );
            };
            if agent_kind != Some(state.agent) {
                return MissingSessionAction::Reject(
                    "无法恢复：记录的 agent 类型与启动项不一致".into(),
                );
            }
            let Some(command) = original_launch else {
                return MissingSessionAction::Reject(
                    "无法恢复：缺少原始启动命令".into(),
                );
            };
            if cwd.is_none() {
                return MissingSessionAction::Reject(
                    "无法恢复：缺少工作目录".into(),
                );
            }
            if !history_exists {
                return MissingSessionAction::Reject(
                    "无法恢复：历史会话已不存在".into(),
                );
            }
            session_history::terminal_resume_command(
                state.agent,
                command,
                &state.session_id,
            )
            .map(MissingSessionAction::Launch)
            .unwrap_or_else(|error| {
                MissingSessionAction::Reject(format!("无法恢复：{error}"))
            })
        }
    }
}
```

Pass the result to `Terminal::spawn_with_action` from
`spawn_layout_leaves_rec`. Live sid attachment ignores the action in smeltd; a
missing sid executes or rejects it.

Use the same helper in `TerminalView::reconnect`: reconnecting to a live sid
still attaches, while an unexpectedly missing sid resumes or rejects safely.
Expose `TerminalView::missing_session_action()` so the daemon hard-restart
flow in `Workspace::confirm_restart_daemon` collects an action per terminal
instead of collecting `launch_cmd`. Spawn each replacement with
`spawn_with_action`; never replay an original supported-agent command.
Each caller computes `history_exists` with
`session_history::terminal_session_exists(state, cwd, None)` before invoking
the pure helper.

- [ ] **Step 6: Run persistence and restore tests**

Run:

```bash
cargo test -p smelt leaf_custom_title_roundtrips
cargo test -p smelt old_leaf_without_resume_state_remains_compatible
cargo test -p smelt terminal_restore_action
cargo test -p smelt old_archive_without_custom_title_still_loads
cargo check -p smelt
```

Expected: tests and check pass.

- [ ] **Step 7: Commit persistence and restore**

```bash
git add crates/smelt/src/main.rs crates/smelt/src/terminal_view.rs
git commit -m "feat(workspace): persist terminal agent identity

Refs #4

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 5: Bind identities during initial terminal launches

**Files:**
- Modify: `crates/smelt/src/session_list.rs:21,466-505`
- Modify: `crates/smelt/src/main.rs:2803-2892`
- Modify: `crates/smelt/src/tasks.rs:1820-1890`

- [ ] **Step 1: Write failing initial-launch preparation tests**

Add pure helper tests in `main.rs`:

```rust
#[test]
fn known_id_agent_launch_keeps_original_and_persists_identity() {
    let entry = LaunchEntry {
        label: "Claude Code".into(),
        command: "claude --dangerously-skip-permissions".into(),
        agent_kind: Some(TerminalAgentKind::Claude),
    };
    let prepared = prepare_terminal_launch(&entry, Some("/tmp/project")).unwrap();
    assert_eq!(prepared.original_command, entry.command);
    assert!(prepared.spawn_command.contains("--session-id"));
    assert!(prepared.resume_state.is_some());
    assert!(prepared.codex_baseline.is_none());
}

#[test]
fn codex_launch_records_baseline_for_discovery() {
    let entry = LaunchEntry {
        label: "Codex".into(),
        command: "codex --dangerously-bypass-approvals-and-sandbox".into(),
        agent_kind: Some(TerminalAgentKind::Codex),
    };
    let prepared = prepare_terminal_launch(&entry, Some("/tmp/project")).unwrap();
    assert_eq!(prepared.spawn_command, entry.command);
    assert!(prepared.resume_state.is_none());
    assert!(prepared.codex_baseline.is_some());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p smelt prepare_terminal_launch
```

Expected: compilation fails because the preparation type and helper do not
exist.

- [ ] **Step 3: Prepare launches before spawning**

Add:

```rust
struct PreparedTerminalLaunch {
    original_command: String,
    spawn_command: String,
    agent_kind: Option<TerminalAgentKind>,
    resume_state: Option<TerminalResumeState>,
    codex_baseline: Option<std::collections::HashSet<String>>,
}
```

`prepare_terminal_launch` generates `uuid::Uuid::new_v4()` and calls the
adapter. For `Known`, store the returned state. For `Discover`, require `cwd`
and snapshot Codex IDs before spawning. Plain entries copy the command and
leave all resume fields empty.

Change `session_list.rs` to pass the full `LaunchEntry` into
`add_session_with_launch` instead of moving only label and command.

- [ ] **Step 4: Store prepared metadata on the new view**

The background spawn receives
`MissingSessionAction::Launch(prepared.spawn_command.clone())`. The resulting
`TerminalView` receives `prepared.original_command`, label, agent kind, and
resume state. Save workspace state immediately after insertion.

If preparation fails, set `background_error` and do not call
`Terminal::spawn`.

- [ ] **Step 5: Discover and persist Codex identity**

Extract `Workspace::start_codex_binding(view, cwd, baseline, cx)` and call it
when `codex_baseline` is present after view insertion:

```rust
for _ in 0..20 {
    smol::Timer::after(std::time::Duration::from_millis(500)).await;
    let current = session_history::terminal_session_ids(
        TerminalAgentKind::Codex,
        &cwd,
        None,
    );
    match session_history::discover_unique_session_id(&baseline, &current) {
        Ok(Some(session_id)) => {
            this.update(cx, |workspace, cx| {
                view.update(cx, |view, _| {
                    view.set_resume_state(Some(TerminalResumeState {
                        agent: TerminalAgentKind::Codex,
                        session_id,
                    }));
                });
                workspace.save_state(cx);
                cx.notify();
            })?;
            return;
        }
        Ok(None) => {}
        Err(error) => {
            this.update(cx, |workspace, cx| {
                workspace.background_error = Some(error);
                cx.notify();
            })?;
            return;
        }
    }
}
```

Before updating, verify the view still belongs to a live workspace session.
Timeout leaves the tab without a resume ID; cold restore will reject rather
than relaunch.

- [ ] **Step 6: Route task-triggered agent launches through the adapter**

In `tasks.rs`, retain the full selected `LaunchEntry`, call
`prepare_terminal_launch(&entry, cwd.as_deref())`, and append the task prompt
to `prepared.spawn_command`, not to the original command. Spawn with:

```rust
Terminal::spawn_with_action(
    24,
    80,
    cwd.as_deref(),
    &sid,
    &MissingSessionAction::Launch(task_command),
)
```

Construct the view with `prepared.original_command`, `prepared.agent_kind`,
and `prepared.resume_state`. If a Codex baseline exists, call
`start_codex_binding` after inserting the view. Persisting the original base
command ensures resume does not replay the task's initial prompt.

- [ ] **Step 7: Run initial launch tests and checks**

Run:

```bash
cargo test -p smelt prepare_terminal_launch
cargo test -p smelt terminal_resume_tests
cargo check -p smelt
```

Expected: tests and check pass.

- [ ] **Step 8: Commit initial binding**

```bash
git add crates/smelt/src/main.rs crates/smelt/src/session_list.rs crates/smelt/src/tasks.rs
git commit -m "feat(workspace): bind terminal agent sessions

Refs #4

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 6: Verify terminal agent restoration

**Files:**
- Verify: `crates/smelt-core/src/agent_kind.rs`
- Verify: `crates/smelt/src/settings.rs`
- Verify: `crates/smelt/src/session_history.rs`
- Verify: `crates/smelt/src/terminal.rs`
- Verify: `crates/smelt/src/terminal_view.rs`
- Verify: `crates/smelt/src/main.rs`
- Verify: `crates/smelt/src/session_list.rs`
- Verify: `crates/smeltd/src/main.rs`

- [ ] **Step 1: Format and inspect scope**

Run:

```bash
cargo fmt --all -- --check
git diff --check main...HEAD
git diff --name-only main...HEAD
```

Expected: formatting and whitespace pass; changed files are limited to the
design, plan, dependency manifests, and files listed above.

- [ ] **Step 2: Run affected tests**

Run:

```bash
cargo test -p smelt-core
cargo test -p smelt session_history::tests
cargo test -p smelt terminal_resume
cargo test -p smeltd
```

Expected: all selected suites pass.

- [ ] **Step 3: Run workspace compilation**

Run:

```bash
cargo check --workspace
```

Expected: workspace check succeeds. Existing warnings outside this change may
remain warnings.

- [ ] **Step 4: Manually exercise the measurable restore boundary**

Using a disposable Claude terminal launch:

1. Start the agent from the project `+` menu.
2. Confirm `workspace.json` contains its terminal agent kind and session ID.
3. Confirm reconnecting while smeltd is alive attaches without another agent
   process.
4. Stop only the disposable smeltd process, restart smelt, and confirm the
   terminal launches Claude with the same saved conversation.
5. Replace the saved ID with a nonexistent ID in a disposable workspace copy
   and confirm restore displays an error without running plain `claude`.

Do not kill unrelated user sessions. Record exact commands and observed IDs in
the PR test plan.

- [ ] **Step 5: Request final code review**

Review `main...HEAD` against issue #4 and the design spec. Fix every Critical
or Important finding, rerun the affected verification, and only then create
the pull request.
