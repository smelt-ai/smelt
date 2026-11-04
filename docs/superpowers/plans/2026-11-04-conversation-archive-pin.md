# Conversation archive and pin implementation plan

> **For agentic workers:** Use executing-plans to implement this plan task-by-task.

**Goal:** Persist conversation-level pin/archive state in the runtime and expose archive/restore in the desktop sidebar, with consistent mobile filtering and pin ordering.

**Architecture:** Add nullable `archived_at` and `pinned_at` to `session_group`, owned exclusively by dedicated runtime commands. Archive stops processes without deleting the directory record. Clients consume authoritative directory events; archived conversations are never materialized as live views.

**Tech stack:** Rust, SQLite/rusqlite, GPUI, existing runtime control protocol.

## Execution constraints

- Work on `feat/conversation-archive-pin`; preserve unrelated Android IDE files.
- Repository instructions require humans to run builds/tests. Supply focused commands at handoff.
- No synchronous runtime IO on the UI thread, no IO during render.
- Stop failures must be returned explicitly, not logged as successful archival.
- Archive requests must be idempotent and reject automation-owned sessions.
- Maintain runtime-qualified IDs, including archive restore/delete actions.

## Tasks

- [ ] Storage and protocol.
  Update `smelt-store/src/{schema,lib,snapshots,workspace}.rs`,
  `smelt-model/src/{workspace_directory,control_api}.rs`,
  `smelt-runtime-client/src/protocol/control.rs`, and
  `smelt-core/src/workspace_catalog.rs`.
  Migrate schema 33 to 34; new conversations have neither flag; updates preserve
  authoritative flags. Dedicated archive/pin calls return workspace revisions.
  Add focused persistence tests: archive clears pin, restore preserves identity,
  stale client updates cannot change flags, duplicate requests are no-ops.
- [ ] Runtime lifecycle.
  Update `smeltd/src/{control,workspace_directory}.rs`.
  Validate IDs/automation ownership, stop only existing processes, archive only
  after successful stop, and publish directory updates. Restoring does not eagerly
  start processes. Refuse starts of archived sessions.
- [ ] Desktop synchronization and commands.
  Update `smelt/src/{workspace_directory,workspace_persist,workspace_sessions}.rs`.
  Cache archived records and pin state per runtime. Keep full directory membership
  distinct from active membership so stale updates cannot resurrect archived items.
  Consume archive events using the existing non-deleting local removal path.
  Send commands asynchronously, using runtime capabilities and visible error reporting.
  Add tests for cold-start archives, remote archive removal, restore, and stale updates.
- [ ] Desktop sidebar.
  Add a focused archive sidebar module alongside `session_list.rs`.
  Add pin/unpin and archive menus to conversation and split-terminal rows.
  Pin within each existing display group, retaining relative order.
  Add a default-collapsed archive section at project-section level, with project/runtime
  labels and explicit restore/delete actions. Archive running/waiting-for-input
  conversations requires confirmation. Persist collapse state in client schema 8.
- [ ] Mobile projection and compatibility.
  Update `smelt-mobile/src/catalog/{projection,tests}.rs`.
  Filter archived conversations from active lists; stable-partition pinned conversations
  within projects. Cover terminal and ACP records and ensure automation rows remain intact.
  Update all affected Rust struct literals and method matches across the workspace.
- [ ] Documentation and final inspection.
  Update `docs/workspace.md` and the design where implementation details were corrected.
  Inspect diffs and whitespace; hand off focused test/format/check commands for human execution.

## PR follow-up

- [x] Reproduce all six client-store migration fixture failures, pin the legacy
  `workspace_ui` definition in `create_v7_schema`, and add a direct v7 upgrade test.
  `cargo test --release --locked -p smelt-client-store --lib -- --test-threads=1`:
  21 tests passed.
- [x] In `session_list/archive.rs`, replace the restore label with an icon-only
  action and add an icon-only permanent-delete action using the existing confirmation.
  Both actions must have tooltips and accessibility names; deletion must not touch
  project files or provider-owned history.
- [x] In `session_list/row.rs`, share `pinned_marker` across project, session,
  and pane titles: 11px Pin, `text_faint`, no shrinking, after the title.
- [x] Extend the daemon archive regression to verify that permanent deletion
  removes directory and persisted resume identity, and subsequent restore fails.
- [x] Compile the desktop and run the focused client-store and archive regressions:
  the release GUI build succeeded, all 21 client-store tests passed, and the archive
  lifecycle test passed with permanent-deletion assertions.
- [ ] Submit the validated changes to the existing PR branch; CI on other platforms
  and manual acceptance of the updated icons remain to be checked.

## Human verification

Implementation and focused test cases have been written on the feature branch.
With explicit user authorization, the release GUI, daemon, helpers, and embedded
agent have been built and the local signed application has been packaged.
The isolated runtime uses `~/.smelt-dev/conversation-archive-pin`; the installed
production application remains running.

Startup exposed an omitted strict `session_group` column allowlist update:
schema 34 was rejected even for a newly created database. The allowlist now
includes both columns. The repaired build initializes the database and responds
to `system.describe`, project opening, and directory queries. Opening the current
workspace by control RPC succeeded; a subsequent GUI thread sample showed the
normal AppKit event loop. The system-folder-picker interaction and archive/pin
UI acceptance still require validation. Automated tests have not been run.
Existing unrelated Android IDE files are left untouched.

User acceptance subsequently exposed a terminal identity loss: archive reused
terminal deletion, removing the provider conversation ID. A regression test
first failed with a missing Copilot resume command. Archive now retires the
runtime while persisting its exact provider ID, separately from deletion.
With explicit authorization, 43 focused daemon tests passed (29 session-directory,
7 terminal-resume, 7 workspace-directory), including stopping a live PTY,
disk reload, late-exit protection, repeated archive, restore, and deletion.
The repaired release has been built and repackaged; real Copilot TUI continuation
still requires user acceptance. The broad automated suites remain unrun.

```sh
cargo test -p smelt-store -p smelt-core -p smelt-runtime-client -p smelt-client-store
cargo test -p smelt workspace_directory
cargo test -p smelt-mobile catalog
cargo test -p smeltd workspace_directory_tests
cargo check -p smelt -p smeltd -p smelt-mobile
cargo fmt --all -- --check
```

Manual checks: archive/restore ACP and split-terminal conversations; confirm interrupting
active work; archive pinned items; restart both clients; test two runtimes with identical
paths and verify old runtime methods remain unavailable without disabling other features.
