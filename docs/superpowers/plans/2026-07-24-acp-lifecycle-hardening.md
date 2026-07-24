# ACP Lifecycle Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make daemon-hosted ACP sessions race-free across open, kill, restart, and upgrade while preserving structured workspace-profile launch settings and emitting one waiting notification.

**Architecture:** Introduce a structured `AcpLaunchSpec` in `smelt-core`, a focused `AcpRegistry` in `smeltd`, and one shared spawn/upgrade gate used at the actual process-spawn boundary. Persist profile identity and launch specs through the GUI, daemon protocol, and handoff; centralize invalid-handoff cleanup and notification production.

**Tech Stack:** Rust 2024, std synchronization primitives, serde/serde_json, smol, Unix sockets and file descriptors, GPUI, Cargo test/check.

---

## File map

- Create `crates/smeltd/src/acp_registry.rs`: atomic ACP ID reservation, per-ID lifecycle serialization, snapshots, removal, and shared spawn gate ownership.
- Modify `crates/smeltd/src/main.rs`: use `AcpRegistry`, wire structured launches, gate upgrade collection, clean rejected handoff resources, and update daemon tests.
- Modify `crates/smelt-core/src/agent_kind.rs`: define `AcpLaunchSpec` and generate profile launch specs.
- Modify `crates/smelt-core/src/acp_conn.rs`: consume `AcpLaunchSpec` and acquire the shared gate at the real child-spawn boundary.
- Modify `crates/smelt-core/src/acp_client.rs`: send structured launch specs over `acp_open`.
- Modify `crates/smelt-core/src/workspace_override.rs`: read workspace overrides from structured launch specs while retaining legacy command-prefix compatibility.
- Modify `crates/smelt-acp-view/src/acp_view.rs`: retain `profile_id` and launch spec, resolve restart configuration correctly, and stop producing duplicate notifications.
- Modify `crates/smelt/src/main.rs`: persist/restore `profile_id` and `AcpLaunchSpec`, and pass them through session creation.
- Modify `crates/smelt/src/session_list.rs`: create profile sessions with structured launch specs and profile IDs.
- Modify `crates/smelt/src/session_history.rs`: expand profile directories and resume history with structured launch specs.

### Task 1: Add structured ACP launch specifications

**Files:**
- Modify: `crates/smelt-core/src/agent_kind.rs:1-150`
- Modify: `crates/smelt-core/src/workspace_override.rs:1-110`
- Modify: `crates/smelt-core/src/acp_conn.rs:41-62,527-540,1058-1130`

- [ ] **Step 1: Write failing launch-spec tests**

Add these tests to `crates/smelt-core/src/agent_kind.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_launch_spec_keeps_workspace_path_as_one_env_value() {
        let profile = AcpProfile {
            id: "quant".into(),
            kind_id: "claude".into(),
            label: "Claude Quant".into(),
            workspace_dir: "~/Claude Workspaces/quant".into(),
        };

        let spec = profile.launch_spec();

        assert_eq!(spec.command, AcpAgentKind::Claude.default_cmd());
        assert_eq!(
            spec.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("~/Claude Workspaces/quant")
        );
    }

    #[test]
    fn plain_launch_spec_has_no_environment_overrides() {
        let spec = AcpLaunchSpec::from_command("claude --flag");
        assert_eq!(spec.command, "claude --flag");
        assert!(spec.env.is_empty());
    }
}
```

Add this test to `crates/smelt-core/src/workspace_override.rs`:

```rust
#[test]
fn structured_override_wins_and_expands_tilde() {
    let mut spec = crate::agent_kind::AcpLaunchSpec::from_command(
        "CLAUDE_CONFIG_DIR=/legacy claude",
    );
    spec.env
        .insert("CLAUDE_CONFIG_DIR".into(), "~/.claude quant".into());

    let expected = dirs::home_dir()
        .unwrap()
        .join(".claude quant")
        .display()
        .to_string();
    assert_eq!(
        env_override_from_launch(&spec, "CLAUDE_CONFIG_DIR"),
        Some(expected)
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p smelt-core agent_kind::tests workspace_override::tests::structured_override_wins_and_expands_tilde
```

Expected: compilation fails because `AcpLaunchSpec`, `launch_spec`, and `env_override_from_launch` do not exist.

- [ ] **Step 3: Implement `AcpLaunchSpec` and profile generation**

Add to `crates/smelt-core/src/agent_kind.rs`:

```rust
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AcpLaunchSpec {
    pub command: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl AcpLaunchSpec {
    pub fn from_command(command: impl Into<String>) -> Self {
        Self { command: command.into(), env: BTreeMap::new() }
    }

    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }
}
```

Replace `AcpProfile::command` with:

```rust
pub fn launch_spec(&self) -> AcpLaunchSpec {
    AcpLaunchSpec::from_command(self.kind().default_cmd())
        .with_env(self.env_var(), self.workspace_dir.clone())
}
```

Keep a compatibility helper only while call sites migrate:

```rust
pub fn command(&self) -> String {
    self.kind().default_cmd()
}
```

- [ ] **Step 4: Add structured override lookup**

Add to `crates/smelt-core/src/workspace_override.rs`:

```rust
pub fn env_override_from_launch(
    launch: &crate::agent_kind::AcpLaunchSpec,
    var_name: &str,
) -> Option<String> {
    launch
        .env
        .get(var_name)
        .map(|value| expand_tilde(value))
        .or_else(|| env_override_from_cmd(&launch.command, var_name))
}
```

- [ ] **Step 5: Make the ACP connection consume the launch spec**

Change `AcpLaunch` in `crates/smelt-core/src/acp_conn.rs`:

```rust
pub struct AcpLaunch {
    pub launch: crate::agent_kind::AcpLaunchSpec,
    pub cwd: Option<String>,
    pub sid: String,
    pub resume_session_id: Option<SessionId>,
    pub resume_needs_transcript_check: bool,
}
```

Change `build_agent` to accept `&AcpLaunchSpec`. Keep the current
`extended_search_path`, `resolve_in_path`, and `resolved` executable/argument
construction unchanged. Replace the current `user_env` collection and final
argument-chain construction with the following code so legacy leading
`VAR=value` tokens are parsed first and structured environment entries win:

```rust
fn build_agent(
    launch: &crate::agent_kind::AcpLaunchSpec,
) -> Result<AcpAgent, agent_client_protocol::Error> {
    let mut tokens = launch.command.split_whitespace();
    let mut env = std::collections::BTreeMap::<String, String>::new();
    let mut prog_token = None;
    for tok in tokens.by_ref() {
        match crate::workspace_override::split_env_assignment(tok) {
            Some((name, value)) => {
                env.insert(name.to_string(), crate::workspace_override::expand_tilde(value));
            }
            None => {
                prog_token = Some(tok);
                break;
            }
        }
    }
    for (name, value) in &launch.env {
        env.insert(name.clone(), crate::workspace_override::expand_tilde(value));
    }

    let env_args: Vec<String> =
        env.into_iter().map(|(name, value)| format!("{name}={value}")).collect();
    let args = std::iter::once(path_env.as_str())
        .chain(env_args.iter().map(String::as_str))
        .chain(resolved.iter().map(String::as_str));
    Ok(AcpAgent::from_args(args)?)
}
```

Update transcript and Claude-meta lookups to use `launch.launch.command` and
`env_override_from_launch(&launch.launch, "CLAUDE_CONFIG_DIR")`.

- [ ] **Step 6: Run tests and check the core crate**

Run:

```bash
cargo test -p smelt-core agent_kind::tests workspace_override::tests
cargo check -p smelt-core
```

Expected: all selected tests pass and `smelt-core` checks successfully.

- [ ] **Step 7: Commit**

```bash
git add crates/smelt-core/src/agent_kind.rs \
  crates/smelt-core/src/workspace_override.rs \
  crates/smelt-core/src/acp_conn.rs
git commit -m "refactor(acp): add structured launch specifications"
```

### Task 2: Wire launch specs and profile identity through GUI persistence

**Files:**
- Modify: `crates/smelt-core/src/acp_client.rs:15-130`
- Modify: `crates/smelt-acp-view/src/acp_view.rs:50-250,350-390`
- Modify: `crates/smelt/src/main.rs:852-871,1685-1710,1999-2125,2290-2330`
- Modify: `crates/smelt/src/session_list.rs:110-150,350-385`

- [ ] **Step 1: Write failing persistence tests**

Extend the `AcpSaved` serde test in `crates/smelt/src/main.rs`:

```rust
#[test]
fn acp_saved_round_trip_preserves_profile_and_launch_spec() {
    let saved = AcpSaved {
        cwd: Some("/repo".into()),
        launch: smelt_core::agent_kind::AcpLaunchSpec::from_command("claude")
            .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/quant"),
        profile_id: Some("quant".into()),
        agent: Some("claude".into()),
        entries: Vec::new(),
        resume_session_id: None,
        sid: Some("acp-1".into()),
    };

    let value = serde_json::to_value(&saved).unwrap();
    let restored: AcpSaved = serde_json::from_value(value).unwrap();

    assert_eq!(restored.profile_id.as_deref(), Some("quant"));
    assert_eq!(
        restored.launch.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
        Some("~/Claude Workspaces/quant")
    );
}
```

Add a backward-compatibility test:

```rust
#[test]
fn legacy_acp_saved_cmd_deserializes_into_launch_spec() {
    let restored: AcpSaved = serde_json::from_value(serde_json::json!({
        "cwd": "/repo",
        "cmd": "claude --flag",
        "agent": "claude"
    }))
    .unwrap();

    assert_eq!(restored.launch.command, "claude --flag");
    assert!(restored.profile_id.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p smelt acp_saved_round_trip_preserves_profile_and_launch_spec legacy_acp_saved_cmd_deserializes_into_launch_spec
```

Expected: compilation fails because the persisted fields and constructors still use `cmd`.

- [ ] **Step 3: Change the GUI-to-daemon request**

In `crates/smelt-core/src/acp_client.rs`:

```rust
pub struct AcpClientLaunch {
    pub id: String,
    pub cwd: Option<String>,
    pub launch: crate::agent_kind::AcpLaunchSpec,
    pub agent_id: String,
    pub resume_id: Option<String>,
}
```

Send:

```rust
let req = serde_json::json!({
    "op": "acp_open",
    "id": launch.id,
    "cwd": launch.cwd,
    "launch": launch.launch,
    "agent": launch.agent_id,
    "resume_id": launch.resume_id,
});
```

- [ ] **Step 4: Store launch spec and profile ID in `AcpView`**

Replace `cmd: String` with:

```rust
launch: smelt_core::agent_kind::AcpLaunchSpec,
profile_id: Option<String>,
```

Change `start` and `placeholder` to accept those fields, pass `launch.clone()`
into `AcpClientLaunch`, and expose:

```rust
pub fn launch_spec(&self) -> smelt_core::agent_kind::AcpLaunchSpec {
    self.launch.clone()
}

pub fn profile_id(&self) -> Option<&str> {
    self.profile_id.as_deref()
}
```

- [ ] **Step 5: Migrate `AcpSaved` with backward compatibility**

Use a custom legacy field:

```rust
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct AcpSaved {
    cwd: Option<String>,
    #[serde(default)]
    launch: smelt_core::agent_kind::AcpLaunchSpec,
    #[serde(default, skip_serializing)]
    cmd: Option<String>,
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    entries: Vec<acp_view::AcpEntry>,
    #[serde(default)]
    resume_session_id: Option<agent_client_protocol::schema::v1::SessionId>,
    #[serde(default)]
    sid: Option<String>,
}

impl AcpSaved {
    fn normalized_launch(&self) -> smelt_core::agent_kind::AcpLaunchSpec {
        if self.launch.command.is_empty() {
            smelt_core::agent_kind::AcpLaunchSpec::from_command(
                self.cmd.clone().unwrap_or_default(),
            )
        } else {
            self.launch.clone()
        }
    }
}
```

Persist `view.launch_spec()` and `view.profile_id()`.

- [ ] **Step 6: Pass profile IDs from session menus**

In `crates/smelt/src/session_list.rs`, replace `p.command()` call sites with:

```rust
let launch = p.launch_spec();
let profile_id = Some(p.id.clone());
```

Change `Workspace::add_acp_session` and `resume_acp_session` in
`crates/smelt/src/main.rs` to accept:

```rust
launch_override: Option<smelt_core::agent_kind::AcpLaunchSpec>,
profile_id: Option<String>,
```

Use `AcpLaunchSpec::from_command(settings::acp_cmd_for(agent, cx))` for ordinary
agent sessions.

- [ ] **Step 7: Run tests and check affected GUI crates**

Run:

```bash
cargo test -p smelt acp_saved_
cargo check -p smelt-acp-view
cargo check -p smelt
```

Expected: persistence tests pass and both GUI crates check.

- [ ] **Step 8: Commit**

```bash
git add crates/smelt-core/src/acp_client.rs \
  crates/smelt-acp-view/src/acp_view.rs \
  crates/smelt/src/main.rs \
  crates/smelt/src/session_list.rs
git commit -m "refactor(acp): persist structured profile launches"
```

### Task 3: Add the atomic ACP registry

**Files:**
- Create: `crates/smeltd/src/acp_registry.rs`
- Modify: `crates/smeltd/src/main.rs:170-190,1170-1260,2450-2540,3077-3438,4850-5085`

- [ ] **Step 1: Write registry concurrency tests**

Create `crates/smeltd/src/acp_registry.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn concurrent_reserve_returns_one_entry() {
        let registry = Arc::new(AcpRegistry::<usize>::new(Arc::new(RwLock::new(()))));
        let created = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();

        for _ in 0..8 {
            let registry = Arc::clone(&registry);
            let created = Arc::clone(&created);
            threads.push(std::thread::spawn(move || {
                registry.reserve_with("same", || {
                    created.fetch_add(1, Ordering::SeqCst);
                    7
                })
            }));
        }

        let entries: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(created.load(Ordering::SeqCst), 1);
        assert!(entries.windows(2).all(|pair| Arc::ptr_eq(&pair[0].0, &pair[1].0)));
    }

    #[test]
    fn remove_if_same_does_not_delete_a_replacement() {
        let registry = AcpRegistry::new(Arc::new(RwLock::new(())));
        let (old, _) = registry.reserve_with("id", || 1usize);
        assert!(registry.remove_if_same("id", &old).is_some());
        let (replacement, _) = registry.reserve_with("id", || 2usize);

        assert!(registry.remove_if_same("id", &old).is_none());
        assert!(Arc::ptr_eq(&registry.get("id").unwrap(), &replacement));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p smeltd acp_registry
```

Expected: compilation fails because the module and registry types do not exist.

- [ ] **Step 3: Implement the registry**

Use:

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

pub(crate) struct AcpSlot<T> {
    pub(crate) lifecycle: Mutex<()>,
    pub(crate) value: T,
}

pub(crate) struct AcpRegistry<T> {
    sessions: Mutex<HashMap<String, Arc<AcpSlot<T>>>>,
    spawn_gate: Arc<RwLock<()>>,
}

impl<T> AcpRegistry<T> {
    pub(crate) fn new(spawn_gate: Arc<RwLock<()>>) -> Self {
        Self { sessions: Mutex::new(HashMap::new()), spawn_gate }
    }

    pub(crate) fn reserve_with(
        &self,
        id: &str,
        create: impl FnOnce() -> T,
    ) -> (Arc<AcpSlot<T>>, bool) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(slot) = sessions.get(id) {
            return (Arc::clone(slot), false);
        }
        let slot = Arc::new(AcpSlot { lifecycle: Mutex::new(()), value: create() });
        sessions.insert(id.to_string(), Arc::clone(&slot));
        (slot, true)
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<AcpSlot<T>>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    pub(crate) fn snapshot(&self) -> Vec<(String, Arc<AcpSlot<T>>)> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .map(|(id, slot)| (id.clone(), Arc::clone(slot)))
            .collect()
    }

    pub(crate) fn remove_if_same(
        &self,
        id: &str,
        expected: &Arc<AcpSlot<T>>,
    ) -> Option<Arc<AcpSlot<T>>> {
        let mut sessions = self.sessions.lock().unwrap();
        let current = sessions.get(id)?;
        if !Arc::ptr_eq(current, expected) {
            return None;
        }
        sessions.remove(id)
    }

    pub(crate) fn spawn_gate(&self) -> Arc<RwLock<()>> {
        Arc::clone(&self.spawn_gate)
    }
}
```

Declare `mod acp_registry;` in `main.rs`.

- [ ] **Step 4: Replace `AcpSessions` with the registry**

Use:

```rust
type AcpSessions = Arc<acp_registry::AcpRegistry<AcpSession>>;
```

Replace direct map locking with `get`, `snapshot`, `reserve_with`, and
`remove_if_same`. Access the session as `slot.value`.

In `handle_acp_open`, reserve first, then serialize launch decisions:

```rust
let (slot, created) = acp_sessions.reserve_with(&id, || {
    make_acp_session(&id, cwd.clone(), needs_check, launch.clone())
});
let _lifecycle = slot.lifecycle.lock().unwrap();
let sess = &slot.value;
if created || (sess.handle.lock().unwrap().is_none() && !launch.command.is_empty()) {
    let known = sess.reduced.lock().unwrap().acp_session_id.clone();
    acp_relaunch(
        sess,
        &id,
        launch,
        known.or(req_resume_id),
        acp_sessions.spawn_gate(),
        &subscribers,
    );
}
```

In `handle_acp_kill`, get the slot, remove only that exact slot, then hold its
lifecycle lock while shutting down the handle and connections.

- [ ] **Step 5: Add open/open and open/kill regression tests**

Add to `main.rs` `acp_tests`:

```rust
#[test]
fn concurrent_open_same_id_keeps_one_registry_slot() {
    let registry = test_acp_registry();
    let subscribers = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let registry = Arc::clone(&registry);
            let subscribers = Arc::clone(&subscribers);
            scope.spawn(move || open_acp_session_once("acp-race", &registry, &subscribers));
        }
    });
    assert_eq!(registry.snapshot().len(), 1);
}

#[test]
fn kill_only_removes_the_slot_it_locked() {
    let registry = test_acp_registry();
    let (old, _) = registry.reserve_with("acp-race", || {
        make_acp_session_value("acp-race", AcpSessionState::default())
    });
    assert!(registry.remove_if_same("acp-race", &old).is_some());
    let (replacement, _) = registry.reserve_with("acp-race", || {
        make_acp_session_value("acp-race", AcpSessionState::default())
    });
    assert!(registry.remove_if_same("acp-race", &old).is_none());
    assert!(Arc::ptr_eq(&registry.get("acp-race").unwrap(), &replacement));
}
```

- [ ] **Step 6: Run tests and check smeltd**

Run:

```bash
cargo test -p smeltd acp_registry
cargo test -p smeltd acp_tests
cargo check -p smeltd
```

Expected: registry and ACP daemon tests pass; `smeltd` checks.

- [ ] **Step 7: Commit**

```bash
git add crates/smeltd/src/acp_registry.rs crates/smeltd/src/main.rs
git commit -m "refactor(smeltd): centralize ACP session lifecycle"
```

### Task 4: Gate actual ACP spawning against upgrade

**Files:**
- Modify: `crates/smelt-core/src/acp_conn.rs:365-410,527-545`
- Modify: `crates/smeltd/src/main.rs:180-190,3200-3270,3460-3590,3660-3680`

- [ ] **Step 1: Write a failing spawn-gate test**

Add a test-only spawn hook to `spawn_acp` and test in
`crates/smelt-core/src/acp_conn.rs`:

```rust
#[test]
fn process_spawn_waits_for_upgrade_write_guard() {
    let gate = Arc::new(RwLock::new(()));
    let write_guard = gate.write().unwrap();
    let reached_spawn = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reached_spawn2 = Arc::clone(&reached_spawn);

    let worker = std::thread::spawn({
        let gate = Arc::clone(&gate);
        move || with_spawn_gate(Some(gate), || {
            reached_spawn2.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    });

    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!reached_spawn.load(std::sync::atomic::Ordering::SeqCst));
    drop(write_guard);
    worker.join().unwrap();
    assert!(reached_spawn.load(std::sync::atomic::Ordering::SeqCst));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p smelt-core process_spawn_waits_for_upgrade_write_guard
```

Expected: compilation fails because `with_spawn_gate` does not exist.

- [ ] **Step 3: Add the optional gate to `spawn_acp`**

Implement:

```rust
fn with_spawn_gate<R>(
    gate: Option<Arc<std::sync::RwLock<()>>>,
    spawn: impl FnOnce() -> R,
) -> R {
    let _permit = gate.as_ref().map(|gate| gate.read().unwrap());
    spawn()
}

pub fn spawn_acp(
    launch: AcpLaunch,
    spawn_gate: Option<Arc<std::sync::RwLock<()>>>,
) -> AcpHandle {
    let (cmd_tx, cmd_rx) = smol::channel::unbounded::<AcpCommand>();
    let (event_tx, event_rx) = smol::channel::unbounded::<AcpEvent>();
    let stdio: Arc<Mutex<Option<AcpStdio>>> = Arc::new(Mutex::new(None));
    let stdio_for_thread = Arc::clone(&stdio);
    let thread_name = format!("smelt-acp-{}", &launch.sid[..launch.sid.len().min(12)]);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::default();
            let cmd = {
                let tx = event_tx.clone();
                match resolve_runtime_command(&launch.launch.command, &|msg| {
                    let _ = tx.try_send(AcpEvent::Status(msg.to_string()));
                }) {
                    Ok(command) => command,
                    Err(error) => {
                        let _ = event_tx.try_send(AcpEvent::Fatal(error));
                        return;
                    }
                }
            };
            let launch = AcpLaunch {
                launch: crate::agent_kind::AcpLaunchSpec {
                    command: cmd,
                    env: launch.launch.env,
                },
                ..launch
            };
            let result = smol::block_on(run_connection(
                &launch,
                spawn_gate,
                cmd_rx,
                event_tx.clone(),
                stderr_tail.clone(),
                stdio_for_thread,
            ));
            if let Err(error) = result {
                let tail = stderr_tail.lock().unwrap().join("\n");
                let message = if tail.is_empty() {
                    error.to_string()
                } else {
                    format!("{error}\n--- agent stderr ---\n{tail}")
                };
                let _ = event_tx.try_send(AcpEvent::Fatal(message));
            }
        })
        .expect("spawn smelt-acp thread");
    AcpHandle { cmd_tx, event_rx, stdio }
}
```

In `run_connection`, hold the read permit through stdio publication:

```rust
let _spawn_permit = spawn_gate.as_ref().map(|gate| gate.read().unwrap());
let agent = build_agent(&launch.launch)?;
let (child_stdin, child_stdout, child_stderr, child) = agent.spawn_process()?;
let pid = child.id() as i32;
*stdio_out.lock().unwrap() = Some(AcpStdio {
    pid,
    stdin_fd: child_stdin.as_raw_fd(),
    stdout_fd: child_stdout.as_raw_fd(),
});
drop(_spawn_permit);
```

- [ ] **Step 4: Share one gate with terminal and ACP spawning**

Change `SPAWN_GATE` in `smeltd`:

```rust
static SPAWN_GATE: std::sync::LazyLock<Arc<RwLock<()>>> =
    std::sync::LazyLock::new(|| Arc::new(RwLock::new(())));
```

Construct `AcpRegistry` with `Arc::clone(&SPAWN_GATE)`. Pass
`Some(acp_sessions.spawn_gate())` to `spawn_acp`. Terminal spawn keeps taking a
read lock from the same gate.

- [ ] **Step 5: Move upgrade locking before all snapshot collection**

At the beginning of handoff collection in `handle_upgrade`:

```rust
let _spawn_gate = SPAWN_GATE.write().unwrap();
let acp_session_list = acp_sessions.snapshot();
let session_list: Vec<_> = sessions
    .lock()
    .unwrap()
    .iter()
    .map(|(id, session)| (id.clone(), Arc::clone(session)))
    .collect();
```

Remove the later write-lock acquisition. Keep the guard alive through exec or
rollback.

- [ ] **Step 6: Add an upgrade-boundary regression test**

Add to `smeltd` tests:

```rust
#[test]
fn upgrade_write_guard_blocks_acp_spawn_until_snapshot_is_finished() {
    let gate = Arc::new(RwLock::new(()));
    let registry = Arc::new(AcpRegistry::<usize>::new(Arc::clone(&gate)));
    let write_guard = gate.write().unwrap();
    let spawned = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let spawned2 = Arc::clone(&spawned);
    let gate2 = registry.spawn_gate();

    let worker = thread::spawn(move || {
        let _read = gate2.read().unwrap();
        spawned2.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    thread::sleep(Duration::from_millis(50));
    assert!(!spawned.load(std::sync::atomic::Ordering::SeqCst));
    drop(write_guard);
    worker.join().unwrap();
    assert!(spawned.load(std::sync::atomic::Ordering::SeqCst));
}
```

- [ ] **Step 7: Run tests and checks**

Run:

```bash
cargo test -p smelt-core process_spawn_waits_for_upgrade_write_guard
cargo test -p smeltd upgrade_write_guard_blocks_acp_spawn_until_snapshot_is_finished
cargo check -p smeltd
```

Expected: both synchronization tests pass and `smeltd` checks.

- [ ] **Step 8: Commit**

```bash
git add crates/smelt-core/src/acp_conn.rs crates/smeltd/src/main.rs
git commit -m "fix(acp): serialize process spawn with daemon upgrade"
```

### Task 5: Centralize rejected-handoff cleanup

**Files:**
- Modify: `crates/smeltd/src/main.rs:1400-1490,1800-1945`

- [ ] **Step 1: Write failing cleanup-decision tests**

Extract validation into a pure result first and test:

```rust
#[test]
fn missing_snapshot_requires_process_cleanup() {
    let item = serde_json::json!({
        "id": "acp-1",
        "stdin_fd": 10,
        "stdout_fd": 11,
        "pid": 123,
    });
    assert!(matches!(
        validate_acp_handoff_item(&item),
        Err(AcpHandoffReject::OwnedResources { pid: 123, stdin_fd: 10, stdout_fd: 11 })
    ));
}

#[test]
fn malformed_snapshot_requires_process_cleanup() {
    let item = serde_json::json!({
        "id": "acp-1",
        "stdin_fd": 10,
        "stdout_fd": 11,
        "pid": 123,
        "snapshot": {"phase": 42},
    });
    assert!(matches!(
        validate_acp_handoff_item(&item),
        Err(AcpHandoffReject::OwnedResources { .. })
    ));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p smeltd missing_snapshot_requires_process_cleanup malformed_snapshot_requires_process_cleanup
```

Expected: compilation fails because validation and rejection types do not exist.

- [ ] **Step 3: Implement one cleanup path**

Add:

```rust
#[derive(Debug, PartialEq, Eq)]
enum AcpHandoffReject {
    Unowned,
    OwnedResources { pid: i32, stdin_fd: RawFd, stdout_fd: RawFd },
}

fn cleanup_rejected_acp_handoff(pid: i32, stdin_fd: RawFd, stdout_fd: RawFd) {
    unsafe {
        libc::close(stdin_fd);
        libc::close(stdout_fd);
        if pid > 0 {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
            let mut status = 0;
            libc::waitpid(pid, &mut status, libc::WNOHANG);
        }
    }
}
```

Make validation return all parsed values only after FD, PID, snapshot, and ACP
session ID validation. Every `OwnedResources` error calls the helper before
continuing. Invalid or absent FDs remain `Unowned` because the new process never
accepted ownership.

- [ ] **Step 4: Add an integration test with real descriptors**

Use `UnixStream::pair`, duplicate its descriptors, feed an item with a malformed
snapshot, run the rejection cleanup, and assert:

```rust
assert_eq!(unsafe { libc::fcntl(stdin_fd, libc::F_GETFD) }, -1);
assert_eq!(unsafe { libc::fcntl(stdout_fd, libc::F_GETFD) }, -1);
```

Use a forked child that pauses, then assert `waitpid` observes termination.

- [ ] **Step 5: Run handoff tests**

Run:

```bash
cargo test -p smeltd handoff
cargo check -p smeltd
```

Expected: handoff tests pass and the daemon checks.

- [ ] **Step 6: Commit**

```bash
git add crates/smeltd/src/main.rs
git commit -m "fix(smeltd): clean rejected ACP handoff resources"
```

### Task 6: Preserve profile identity on restart and history access

**Files:**
- Modify: `crates/smelt-acp-view/src/acp_view.rs:140-250`
- Modify: `crates/smelt/src/session_history.rs:895-905,1301-1355,1395-1495`
- Modify: `crates/smelt/src/main.rs:2053-2125`

- [ ] **Step 1: Write launch-resolution tests in `smelt-acp-view`**

Extract a pure helper:

```rust
fn resolve_restart_launch(
    current: &smelt_core::agent_kind::AcpLaunchSpec,
    profile_id: Option<&str>,
    config: &smelt_ui::agent_ui_config::AgentUiConfig,
    agent: AcpAgentKind,
) -> smelt_core::agent_kind::AcpLaunchSpec
```

Test:

```rust
#[test]
fn restart_regenerates_existing_profile_launch() {
    let mut config = AgentUiConfig::default();
    config.profiles.push(AcpProfile {
        id: "quant".into(),
        kind_id: "claude".into(),
        label: "Quant".into(),
        workspace_dir: "~/new quant".into(),
    });
    let old = AcpLaunchSpec::from_command("claude")
        .with_env("CLAUDE_CONFIG_DIR", "~/old");

    let resolved =
        resolve_restart_launch(&old, Some("quant"), &config, AcpAgentKind::Claude);

    assert_eq!(
        resolved.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
        Some("~/new quant")
    );
}

#[test]
fn removed_profile_falls_back_to_persisted_launch() {
    let config = AgentUiConfig::default();
    let persisted = AcpLaunchSpec::from_command("claude")
        .with_env("CLAUDE_CONFIG_DIR", "~/removed profile");

    assert_eq!(
        resolve_restart_launch(
            &persisted,
            Some("missing"),
            &config,
            AcpAgentKind::Claude,
        ),
        persisted
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p smelt-acp-view restart_
```

Expected: compilation fails because `resolve_restart_launch` does not exist.

- [ ] **Step 3: Implement restart resolution**

Use:

```rust
fn resolve_restart_launch(
    current: &AcpLaunchSpec,
    profile_id: Option<&str>,
    config: &AgentUiConfig,
    agent: AcpAgentKind,
) -> AcpLaunchSpec {
    match profile_id {
        Some(id) => config
            .find_profile(id)
            .map(AcpProfile::launch_spec)
            .unwrap_or_else(|| current.clone()),
        None => AcpLaunchSpec::from_command(config.acp_cmd_for(agent)),
    }
}
```

Call it from `restart`, update `self.launch`, and send the resolved spec.

- [ ] **Step 4: Write the history tilde-and-space test**

In `crates/smelt/src/session_history.rs`:

```rust
#[test]
fn profile_override_expands_tilde_and_preserves_spaces() {
    let tmp = std::env::temp_dir().join("smelt history home");
    std::fs::create_dir_all(&tmp).unwrap();
    with_home(&tmp, || {
        let expanded = smelt_core::workspace_override::expand_tilde(
            "~/Claude Workspaces/quant",
        );
        assert_eq!(
            expanded,
            tmp.join("Claude Workspaces/quant").display().to_string()
        );
    });
    std::fs::remove_dir_all(&tmp).unwrap();
}
```

- [ ] **Step 5: Normalize the directory before background history access**

Change:

```rust
let override_dir = profile_id.as_deref().and_then(|id| {
    cx.global::<crate::settings::AgentUiConfig>()
        .find_profile(id)
        .map(|profile| {
            smelt_core::workspace_override::expand_tilde(&profile.workspace_dir)
        })
});
```

Use `profile.launch_spec()` for history resume instead of `profile.command()`,
and pass the profile ID to `resume_acp_session`.

- [ ] **Step 6: Run tests and checks**

Run:

```bash
cargo test -p smelt-acp-view restart_
cargo test -p smelt profile_override_expands_tilde_and_preserves_spaces
cargo check -p smelt
```

Expected: restart and history path tests pass; GUI checks.

- [ ] **Step 7: Commit**

```bash
git add crates/smelt-acp-view/src/acp_view.rs \
  crates/smelt/src/session_history.rs \
  crates/smelt/src/main.rs
git commit -m "fix(acp): preserve workspace profiles across restart"
```

### Task 7: Make daemon state the sole notification producer

**Files:**
- Modify: `crates/smelt-acp-view/src/acp_view.rs:1-12,625-675`
- Modify: `crates/smelt/src/main.rs:5950-6005`

- [ ] **Step 1: Extract and test daemon transition notification logic**

In `crates/smelt/src/main.rs`, extract:

```rust
fn waiting_notification(
    previous: Option<terminal::DaemonPhase>,
    current: &terminal::DaemonSessionState,
) -> Option<(String, String, bool)> {
    let entered = matches!(
        current.phase,
        terminal::DaemonPhase::AwaitingApproval
            | terminal::DaemonPhase::WaitingForUser
    ) && previous != Some(current.phase);
    if !entered {
        return None;
    }
    let title = current.phase_label().to_string();
    let message = current
        .detail_line()
        .or_else(|| current.title.clone())
        .unwrap_or_else(|| format!("会话 {}", &current.id[..8.min(current.id.len())]));
    Some((
        title,
        message,
        current.phase == terminal::DaemonPhase::AwaitingApproval,
    ))
}
```

Add:

```rust
#[test]
fn waiting_transition_notifies_once() {
    let state = terminal::DaemonSessionState {
        id: "acp-123".into(),
        phase: terminal::DaemonPhase::WaitingForUser,
        pending_question: Some("Choose one".into()),
        ..Default::default()
    };
    assert!(waiting_notification(Some(terminal::DaemonPhase::Running), &state).is_some());
    assert!(waiting_notification(Some(terminal::DaemonPhase::WaitingForUser), &state).is_none());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p smelt waiting_transition_notifies_once
```

Expected: compilation fails because `waiting_notification` does not exist.

- [ ] **Step 3: Remove snapshot-side enqueueing**

Delete the `was_awaiting`/`now_awaiting` and `PendingAgentNotifs` block from
`AcpView::apply_snapshot`. Update the module comment to state that daemon state
is the only notification source.

Use `waiting_notification` from the daemon-state consumer before inserting the
new state into the map.

- [ ] **Step 4: Run tests and checks**

Run:

```bash
cargo test -p smelt waiting_transition_notifies_once
cargo check -p smelt-acp-view
cargo check -p smelt
```

Expected: the transition test passes and both GUI crates check.

- [ ] **Step 5: Commit**

```bash
git add crates/smelt-acp-view/src/acp_view.rs crates/smelt/src/main.rs
git commit -m "fix(acp): deduplicate waiting notifications"
```

### Task 8: Verify complete lifecycle and handoff behavior

**Files:**
- Modify: `crates/smeltd/src/main.rs`
- Modify: `crates/smelt-core/src/acp_conn.rs`

- [ ] **Step 1: Add a two-upgrade pending-request regression test**

Extend the existing resumed-line test so the first resumed connection receives
a live permission request, captures its raw line, serializes it into a second
handoff item, and the second resumed connection replays the same request ID:

```rust
assert_eq!(
    second_snapshot.pending_raw_request_line(),
    Some(r#"{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{}}"#)
);
```

- [ ] **Step 2: Add an open-then-handoff registry test**

Construct an `AcpRegistry`, reserve one session, attach test stdio metadata, call
the handoff collection helper, and assert:

```rust
assert_eq!(items.len(), 1);
assert_eq!(items[0]["id"], "acp-upgrade");
assert_eq!(registry.snapshot().len(), 1);
```

- [ ] **Step 3: Run all targeted tests**

Run:

```bash
cargo test -p smelt-core acp
cargo test -p smeltd acp
cargo test -p smelt-acp-view
cargo test -p smelt session_history
cargo test -p smelt waiting_transition_notifies_once
```

Expected: all targeted suites pass.

- [ ] **Step 4: Run workspace checks**

Run:

```bash
cargo check --workspace
```

Expected: every workspace crate checks successfully.

- [ ] **Step 5: Review the final branch diff**

Run:

```bash
git status --short
git diff --check HEAD~7..HEAD
git diff --stat HEAD~7..HEAD
```

Expected: no whitespace errors; changes are limited to the design, plan, ACP
lifecycle, launch configuration, profile history/restart, handoff, and
notification files.

- [ ] **Step 6: Commit final regression coverage**

```bash
git add crates/smelt-core/src/acp_conn.rs crates/smeltd/src/main.rs
git commit -m "test(acp): cover lifecycle and repeated handoff"
```

- [ ] **Step 7: Request code review**

Dispatch a read-only code-review agent against the complete branch diff. Fix
all high-confidence Critical and High findings before considering the work
complete.
