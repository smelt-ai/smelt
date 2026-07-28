# Legacy Agent Metadata Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Safely migrate legacy launch entries and workspace panes that predate terminal-agent metadata, without guessing any historical conversation ID.

**Architecture:** Centralize exact executable inference in `session_history.rs`, reusing its shell parser and environment-prefix rules. `settings.rs` uses that helper to migrate and persist legacy launch entries; `main.rs` recursively upgrades saved pane kinds while leaving `resume_state` empty.

**Tech Stack:** Rust 2021, serde/serde_json, shell-words, existing `smelt` unit tests.

---

### Task 1: Centralize safe agent-kind inference

**Files:**
- Modify: `crates/smelt/src/session_history.rs:24-56`
- Test: `crates/smelt/src/session_history.rs:1840-1970`

- [ ] **Step 1: Write failing inference tests**

Add tests alongside the existing terminal command adapter tests:

```rust
#[test]
fn infers_terminal_agent_only_from_exact_executable() {
    assert_eq!(
        infer_terminal_agent_kind("copilot --allow-all"),
        Some(TerminalAgentKind::Copilot)
    );
    assert_eq!(
        infer_terminal_agent_kind("COPILOT_HOME='~/Copilot Data' copilot --allow-all"),
        Some(TerminalAgentKind::Copilot)
    );
    assert_eq!(infer_terminal_agent_kind("/opt/bin/claude"), Some(TerminalAgentKind::Claude));
    assert_eq!(infer_terminal_agent_kind("claude-quant --dangerously-skip-permissions"), None);
    assert_eq!(infer_terminal_agent_kind("echo copilot"), None);
    assert_eq!(infer_terminal_agent_kind("copilot | cat"), None);
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p smelt session_history::tests::infers_terminal_agent_only_from_exact_executable -- --exact
```

Expected: compilation fails because `infer_terminal_agent_kind` does not exist.

- [ ] **Step 3: Implement the shared inference helper**

Add this immediately after `safe_agent_command`:

```rust
pub(crate) fn infer_terminal_agent_kind(command: &str) -> Option<TerminalAgentKind> {
    TerminalAgentKind::ALL
        .into_iter()
        .find(|kind| safe_agent_command(*kind, command).is_ok())
}
```

Import the helper in the test module. This deliberately inherits rejection of shell control operators, exact basename matching, quoted paths, and leading environment assignments from the existing resume adapter.

- [ ] **Step 4: Run adapter tests and verify GREEN**

Run:

```bash
cargo test -p smelt session_history::tests -- --test-threads=1
```

Expected: all `session_history::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add crates/smelt/src/session_history.rs
git commit -m "refactor(terminal): centralize agent command inference"
```

### Task 2: Migrate legacy launch entries and persist them

**Files:**
- Modify: `crates/smelt/src/settings.rs:138-255`
- Test: `crates/smelt/src/settings.rs`

- [ ] **Step 1: Write failing migration tests**

Add a `launch_config_tests` module that exercises a pure migration function:

```rust
#[cfg(test)]
mod launch_config_tests {
    use super::{LaunchEntry, migrate_launch_entries};
    use smelt_core::agent_kind::TerminalAgentKind;

    #[test]
    fn fills_missing_kinds_without_overwriting_explicit_choices() {
        let mut entries = vec![
            LaunchEntry {
                label: "Copilot".into(),
                command: "copilot --allow-all".into(),
                agent_kind: None,
            },
            LaunchEntry {
                label: "Custom Claude".into(),
                command: "claude-quant".into(),
                agent_kind: None,
            },
            LaunchEntry {
                label: "Explicit".into(),
                command: "copilot".into(),
                agent_kind: Some(TerminalAgentKind::Claude),
            },
        ];

        assert!(migrate_launch_entries(&mut entries));
        assert_eq!(entries[0].agent_kind, Some(TerminalAgentKind::Copilot));
        assert_eq!(entries[1].agent_kind, None);
        assert_eq!(entries[2].agent_kind, Some(TerminalAgentKind::Claude));
        assert!(!migrate_launch_entries(&mut entries));
    }
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p smelt settings::launch_config_tests -- --test-threads=1
```

Expected: compilation fails because `migrate_launch_entries` does not exist.

- [ ] **Step 3: Implement migration and write-back**

Add:

```rust
fn migrate_launch_entries(entries: &mut [LaunchEntry]) -> bool {
    let mut changed = false;
    for entry in entries {
        if entry.agent_kind.is_none() {
            if let Some(kind) = crate::session_history::infer_terminal_agent_kind(&entry.command) {
                entry.agent_kind = Some(kind);
                changed = true;
            }
        }
    }
    changed
}
```

Change the `Some(entries)` branch in `load_launch_config` to:

```rust
Some(mut entries) => {
    let changed = migrate_launch_entries(&mut entries);
    let config = LaunchConfig { entries };
    if changed {
        save_launch_config(&config);
    }
    config
}
```

Explicit user-selected kinds remain untouched, unknown/custom commands remain `None`, and the second migration pass is idempotent.

- [ ] **Step 4: Run settings and command adapter tests**

Run:

```bash
cargo test -p smelt settings::launch_config_tests -- --test-threads=1
cargo test -p smelt session_history::tests -- --test-threads=1
```

Expected: migration and adapter tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/smelt/src/settings.rs crates/smelt/src/session_history.rs
git commit -m "fix(settings): migrate legacy agent launch metadata"
```

### Task 3: Backfill legacy workspace pane kinds without guessing IDs

**Files:**
- Modify: `crates/smelt/src/main.rs:1020-1045`
- Modify: `crates/smelt/src/main.rs:1616-1624`
- Test: `crates/smelt/src/main.rs:7750-7960`

- [ ] **Step 1: Write failing pane migration tests**

Add to `pane_state_tests`:

```rust
#[test]
fn legacy_agent_leaf_gains_kind_but_not_resume_identity() {
    let old = r#"{"Leaf":{
        "cwd":"/tmp/x",
        "id":"terminal-sid",
        "launch_label":"Copilot",
        "launch_cmd":"COPILOT_HOME='~/Copilot Data' copilot --allow-all"
    }}"#;
    let mut pane: PaneState = serde_json::from_str(old).unwrap();

    assert!(pane.migrate_legacy_agent_kind());
    match pane {
        PaneState::Leaf {
            agent_kind,
            resume_state,
            ..
        } => {
            assert_eq!(agent_kind, Some(TerminalAgentKind::Copilot));
            assert!(resume_state.is_none());
        }
        PaneState::Split { .. } => panic!("expected leaf"),
    }
}

#[test]
fn legacy_custom_alias_is_not_migrated() {
    let old = r#"{"Leaf":{
        "cwd":"/tmp/x",
        "id":"terminal-sid",
        "launch_label":"Claude Quant",
        "launch_cmd":"claude-quant --dangerously-skip-permissions"
    }}"#;
    let mut pane: PaneState = serde_json::from_str(old).unwrap();

    assert!(!pane.migrate_legacy_agent_kind());
}
```

Add a split-tree assertion so recursive migration is covered:

```rust
#[test]
fn legacy_agent_migration_walks_split_children() {
    let mut pane: PaneState = serde_json::from_str(
        r#"{"Split":{"axis":"H","children":[
            {"Leaf":{"cwd":"/tmp/a","launch_cmd":"claude"}},
            {"Leaf":{"cwd":"/tmp/b","launch_cmd":"grok"}}
        ]}}"#,
    )
    .unwrap();

    assert!(pane.migrate_legacy_agent_kind());
}
```

- [ ] **Step 2: Run pane tests and verify RED**

Run:

```bash
cargo test -p smelt pane_state_tests -- --test-threads=1
```

Expected: compilation fails because `PaneState::migrate_legacy_agent_kind` does not exist.

- [ ] **Step 3: Implement recursive conservative migration**

Add an implementation next to `PaneState`:

```rust
impl PaneState {
    fn migrate_legacy_agent_kind(&mut self) -> bool {
        match self {
            Self::Leaf {
                launch_cmd,
                agent_kind,
                ..
            } => {
                if agent_kind.is_some() {
                    return false;
                }
                let Some(kind) = launch_cmd
                    .as_deref()
                    .and_then(crate::session_history::infer_terminal_agent_kind)
                else {
                    return false;
                };
                *agent_kind = Some(kind);
                true
            }
            Self::Split { children, .. } => children
                .iter_mut()
                .fold(false, |changed, child| {
                    child.migrate_legacy_agent_kind() || changed
                }),
        }
    }
}
```

Do not create or modify `resume_state`.

- [ ] **Step 4: Apply migration while loading and persist the upgraded state**

Replace `load_ws_state` with:

```rust
fn load_ws_state() -> Option<WsState> {
    let path = ws_state_path()?;
    let data = std::fs::read_to_string(&path).ok()?;
    let mut state: WsState = serde_json::from_str(&data).ok()?;
    let mut changed = false;
    for session in &mut state.sessions {
        changed |= session.layout.migrate_legacy_agent_kind();
    }
    if let Some(layout) = &mut state.layout {
        changed |= layout.migrate_legacy_agent_kind();
    }
    if changed {
        crate::json_store::save_json(Some(path), &state);
    }
    Some(state)
}
```

This covers both current `sessions` archives and the older single-`layout` archive. The oldest cwd-only `tabs` format has no launch command and cannot be inferred.

- [ ] **Step 5: Run pane and workspace tests**

Run:

```bash
cargo test -p smelt pane_state_tests -- --test-threads=1
cargo test -p smelt workspace_state_tests -- --test-threads=1
```

Expected: all pane and workspace persistence tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/smelt/src/main.rs
git commit -m "fix(workspace): backfill legacy terminal agent kinds"
```

### Task 4: Verify the complete migration

**Files:**
- Verify: `crates/smelt/src/session_history.rs`
- Verify: `crates/smelt/src/settings.rs`
- Verify: `crates/smelt/src/main.rs`

- [ ] **Step 1: Run the complete smelt test target**

Run:

```bash
cargo test -p smelt -- --test-threads=1
```

Expected: all tests pass. If PTY tests cannot locate `smeltd`, first run `cargo build -p smeltd`, copy `target/debug/smeltd` into an isolated `$HOME/.smelt/bin/`, and rerun with that isolated `HOME`.

- [ ] **Step 2: Run formatting and workspace checks**

Run:

```bash
cargo fmt --all -- --check
cargo check --workspace
git diff --check
```

Expected: all commands exit 0.

- [ ] **Step 3: Verify migration against sanitized copies of the observed legacy shapes**

Use tests—not the user's live files—to confirm:

- a launch entry containing `copilot --allow-all` gains `agent_kind: "copilot"`;
- a pane containing that launch gains only `agent_kind`, with `resume_state: null`;
- `claude-quant`, SSH entries, and explicit kinds remain unchanged;
- a newly launched recognized agent receives a non-null `resume_state`.

- [ ] **Step 4: Push the new commits to PR #6**

Run:

```bash
git push origin feat/issue-4-terminal-agent-resume
```

Expected: PR #6 updates without rewriting history.
