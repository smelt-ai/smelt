# ACP Strict Resume Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent transient ACP restoration errors from silently creating a new conversation after smeltd restarts.

**Architecture:** Keep the existing `sid` attach path in smeltd and classify protocol-level resume/load failures in `smelt-core`. Only typed `MethodNotFound` or `ResourceNotFound` outcomes may reach `session/new`; all other errors end the attempt with a retryable message while preserving the saved agent session ID and local history.

**Tech Stack:** Rust 2021, agent-client-protocol 1.3, ACP JSON-RPC error codes, smol, Cargo tests.

---

## File map

- Modify `crates/smelt-core/src/acp_conn.rs`: define restoration decisions,
  classify typed ACP errors, gate `session/new`, and report retryable failures.
- Modify `crates/smelt-core/src/acp_session.rs`: carry an optional fresh-session
  fallback reason into persistent state and verify failed restoration preserves
  conversation identity and entries.
- Modify `crates/smeltd/src/main.rs`: update the `acp_open` protocol comment and
  add a regression test proving daemon-known identity wins over the request
  fallback identity.

### Task 1: Classify ACP restoration failures

**Files:**
- Modify: `crates/smelt-core/src/acp_conn.rs:163-174,683-791`

- [ ] **Step 1: Write failing classification tests**

Add a test module near the existing `resume_incoming_lines_tests`:

```rust
#[cfg(test)]
mod restore_failure_tests {
    use super::{RestoreDecision, classify_load_failure, classify_resume_failure};
    use agent_client_protocol::Error;

    #[test]
    fn resume_method_missing_tries_load_when_supported() {
        let error = Error::method_not_found();
        assert_eq!(
            classify_resume_failure(&error, true),
            RestoreDecision::TryLoad
        );
    }

    #[test]
    fn resume_method_missing_starts_fresh_without_load_support() {
        let error = Error::method_not_found();
        assert!(matches!(
            classify_resume_failure(&error, false),
            RestoreDecision::StartFresh(_)
        ));
    }

    #[test]
    fn missing_session_may_start_fresh() {
        let error = Error::resource_not_found(None);
        assert!(matches!(
            classify_resume_failure(&error, true),
            RestoreDecision::StartFresh(_)
        ));
        assert!(matches!(
            classify_load_failure(&error),
            RestoreDecision::StartFresh(_)
        ));
    }

    #[test]
    fn internal_and_transport_shaped_errors_are_retryable() {
        let error = Error::internal_error();
        assert_eq!(
            classify_resume_failure(&error, true),
            RestoreDecision::Retryable
        );
        assert_eq!(
            classify_load_failure(&error),
            RestoreDecision::Retryable
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:

```bash
cargo test -p smelt-core restore_failure_tests
```

Expected: compilation fails because `RestoreDecision`,
`classify_resume_failure`, and `classify_load_failure` do not exist.

- [ ] **Step 3: Add the typed decision helpers**

Add above `run_connection`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
enum RestoreDecision {
    TryLoad,
    StartFresh(String),
    Retryable,
}

fn classify_resume_failure(
    error: &agent_client_protocol::Error,
    load_supported: bool,
) -> RestoreDecision {
    match error.code {
        agent_client_protocol::ErrorCode::ResourceNotFound => {
            RestoreDecision::StartFresh("旧会话不存在，已创建新对话".into())
        }
        agent_client_protocol::ErrorCode::MethodNotFound if load_supported => {
            RestoreDecision::TryLoad
        }
        agent_client_protocol::ErrorCode::MethodNotFound => {
            RestoreDecision::StartFresh("agent 不支持恢复会话，已创建新对话".into())
        }
        _ => RestoreDecision::Retryable,
    }
}

fn classify_load_failure(error: &agent_client_protocol::Error) -> RestoreDecision {
    match error.code {
        agent_client_protocol::ErrorCode::ResourceNotFound => {
            RestoreDecision::StartFresh("旧会话不存在，已创建新对话".into())
        }
        agent_client_protocol::ErrorCode::MethodNotFound => {
            RestoreDecision::StartFresh("agent 不支持恢复会话，已创建新对话".into())
        }
        _ => RestoreDecision::Retryable,
    }
}
```

Do not compare error messages. `ErrorCode` is the cross-agent contract.

- [ ] **Step 4: Run the classification tests**

Run:

```bash
cargo test -p smelt-core restore_failure_tests
```

Expected: 4 tests pass.

- [ ] **Step 5: Commit the classifier**

```bash
git add crates/smelt-core/src/acp_conn.rs
git commit -m "fix(acp): classify session restore failures

Refs #1

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 2: Gate new-session fallback and preserve recovery context

**Files:**
- Modify: `crates/smelt-core/src/acp_conn.rs:93-102,683-815,980-1010`
- Modify: `crates/smelt-core/src/acp_session.rs:408-430,752-820`

- [ ] **Step 1: Write failing reducer tests for fallback and retry**

Extend `crates/smelt-core/src/acp_session.rs` tests:

```rust
#[test]
fn retryable_restore_failure_preserves_identity_and_entries() {
    let mut state = fresh_state();
    state.acp_session_id = Some("old-session".into());
    state.entries.push(AcpEntry::User("old question".into()));

    apply_event(
        &mut state,
        AcpEvent::Fatal("恢复失败，可重试：transport closed".into()),
    );

    assert_eq!(state.acp_session_id.as_deref(), Some("old-session"));
    assert_eq!(state.entries.len(), 1);
    assert!(matches!(
        &state.entries[0],
        AcpEntry::User(text) if text == "old question"
    ));
    assert!(matches!(
        &state.phase,
        AcpPhase::Ended(message) if message.starts_with("恢复失败，可重试：")
    ));
}

#[test]
fn fresh_fallback_exposes_reason_and_replaces_identity_after_ready() {
    let mut state = fresh_state();
    state.acp_session_id = Some("old-session".into());
    state.entries.push(AcpEntry::User("old question".into()));

    apply_event(
        &mut state,
        AcpEvent::Ready {
            session_id: agent_client_protocol::schema::v1::SessionId::new("new-session"),
            kind: ReadyKind::Fresh,
            supports_image: true,
            fallback_reason: Some("旧会话不存在，已创建新对话".into()),
        },
    );

    assert_eq!(state.acp_session_id.as_deref(), Some("new-session"));
    assert_eq!(
        state.status_line.as_deref(),
        Some("旧会话不存在，已创建新对话")
    );
    assert!(matches!(state.entries.last(), Some(AcpEntry::Divider(_))));
}
```

Update existing `AcpEvent::Ready` constructors in this test module with
`fallback_reason: None`.

- [ ] **Step 2: Run the reducer tests to verify they fail**

Run:

```bash
cargo test -p smelt-core \
  acp_session::tests
```

Expected: compilation fails because `AcpEvent::Ready` has no
`fallback_reason`.

- [ ] **Step 3: Carry fallback context on the Ready event**

Change `AcpEvent::Ready` in `acp_conn.rs`:

```rust
Ready {
    session_id: SessionId,
    kind: ReadyKind,
    supports_image: bool,
    fallback_reason: Option<String>,
},
```

Add a `fallback_reason: Option<String>` parameter to `drive_session`. Emit it
with `AcpEvent::Ready`, and pass `None` from both successful resume paths and
the inherited-FD path.

In `acp_session.rs`, destructure the new field and replace the unconditional
status clear:

```rust
state.phase = AcpPhase::Idle;
state.status_line = fallback_reason;
```

- [ ] **Step 4: Refactor the restore chain to require an explicit fresh decision**

In `run_connection`, initialize:

```rust
let mut fresh_fallback_reason = None;
```

For a known session ID, treat the Claude transcript precheck as confirmed
absence only when a `cwd` is available and the computed transcript path does
not exist. A missing `cwd` is inconclusive and must still attempt the protocol
restore. Non-Claude agents always attempt the protocol restore.

Then:

1. Send `session/resume`.
2. On success, call `drive_session` with
   `ReadyKind::ResumedKeepHistory` and `None`.
3. On `TryLoad`, send `session/load`.
4. On `StartFresh(reason)`, store the reason.
5. On `Retryable`, send
   `AcpEvent::Fatal(format!("恢复失败，可重试：{error}"))` and return `Ok(())`.

Apply `classify_load_failure` to load errors with the same
`StartFresh`/`Retryable` handling. Do not execute `NewSessionRequest` unless
there is no resume ID, the Claude transcript precheck confirms absence, or
`fresh_fallback_reason` is `Some`.

Pass `fresh_fallback_reason` to the final fresh `drive_session` call:

```rust
drive_session(
    session,
    cmd_rx,
    event_tx,
    ReadyKind::Fresh,
    supports_image,
    model_cfg,
    fresh_fallback_reason,
)
.await
```

For a failed Claude transcript precheck, set:

```rust
fresh_fallback_reason =
    Some("旧会话记录已不存在，已创建新对话".to_string());
```

- [ ] **Step 5: Run focused core tests**

Run:

```bash
cargo test -p smelt-core restore_failure_tests
cargo test -p smelt-core acp_session::tests
cargo check -p smelt-core
```

Expected: all tests pass and the crate checks successfully.

- [ ] **Step 6: Commit strict restore behavior**

```bash
git add crates/smelt-core/src/acp_conn.rs crates/smelt-core/src/acp_session.rs
git commit -m "fix(acp): avoid fresh sessions on transient restore errors

Refs #1

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 3: Lock daemon identity selection and update protocol documentation

**Files:**
- Modify: `crates/smeltd/src/main.rs:3657-3667,3996-4041,6190-6235`

- [ ] **Step 1: Write the failing identity-selection test**

Extract the existing expression into a helper and first add its test:

```rust
#[test]
fn daemon_known_resume_id_wins_after_gui_reconnect() {
    assert_eq!(
        select_resume_id(Some("daemon-session".into()), Some("saved-session".into()))
            .as_deref(),
        Some("daemon-session")
    );
    assert_eq!(
        select_resume_id(None, Some("saved-session".into())).as_deref(),
        Some("saved-session")
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p smeltd daemon_known_resume_id_wins_after_gui_reconnect
```

Expected: compilation fails because `select_resume_id` does not exist.

- [ ] **Step 3: Add and use the identity helper**

Add beside `parse_acp_open_request`:

```rust
fn select_resume_id(known: Option<String>, requested: Option<String>) -> Option<String> {
    known.or(requested)
}
```

Replace:

```rust
known.or_else(|| req_resume_id.clone())
```

with:

```rust
select_resume_id(known, req_resume_id.clone())
```

Update the `acp_open` protocol comment to state that a missing daemon slot
launches a replacement process, attempts protocol restoration, and creates a
new conversation only for typed unsupported/not-found outcomes.

- [ ] **Step 4: Run daemon tests and check**

Run:

```bash
cargo test -p smeltd daemon_known_resume_id_wins_after_gui_reconnect
cargo test -p smeltd acp_open_request
cargo check -p smeltd
```

Expected: all selected tests pass and smeltd checks successfully.

- [ ] **Step 5: Commit daemon regression coverage**

```bash
git add crates/smeltd/src/main.rs
git commit -m "test(acp): lock resume identity precedence

Refs #1

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

### Task 4: Verify the complete recovery change

**Files:**
- Verify: `crates/smelt-core/src/acp_conn.rs`
- Verify: `crates/smelt-core/src/acp_session.rs`
- Verify: `crates/smeltd/src/main.rs`

- [ ] **Step 1: Format and inspect the diff**

Run:

```bash
cargo fmt --all -- --check
git diff --check
git diff --stat main...HEAD
```

Expected: formatting and whitespace checks pass; the diff is limited to the
design, plan, core restoration logic, reducer tests, and daemon regression
coverage.

- [ ] **Step 2: Run all affected crate tests**

Run:

```bash
cargo test -p smelt-core
cargo test -p smeltd
```

Expected: both crate suites pass.

- [ ] **Step 3: Run workspace compilation**

Run:

```bash
cargo check --workspace
```

Expected: workspace check succeeds. Existing `smelt-mobile` warnings may still
be printed but must not become errors.

- [ ] **Step 4: Record the known unrelated baseline limitation**

The initial `cargo test --workspace` baseline had 141 passing and 4 failing
`smelt::terminal::damage_gate_tests` because the test runtime could not find an
installed smeltd binary. Do not change terminal code as part of issue #1. If a
test installation path is available, rerun those four tests with that binary;
otherwise report the baseline limitation without claiming the full workspace
test suite passed.

- [ ] **Step 5: Commit plan completion metadata if changed**

If checkbox state or directly related documentation changed during execution:

```bash
git add docs/superpowers
git commit -m "docs: complete ACP strict resume plan

Refs #1

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```
