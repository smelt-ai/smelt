# Project Repositories Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace encoded text inputs with folder binding, Git URL cloning, and repository checkboxes using the existing project plus menu.

**Architecture:** Runtime-owned commands serialize repository mutations and persist owned symlink paths in SQLite. A dedicated desktop modal reuses the runtime directory picker and async request pattern; Git operations remain outside render.

**Tech Stack:** Rust, GPUI/gpui-component, Git CLI, SQLite, existing typed control API.

---

### Task 1: Repository lifecycle

Files: `crates/smelt-git/src/project_repository.rs` (new), `crates/smelt-git/src/lib.rs`, `crates/smelt-git/src/worktree.rs`, `crates/smelt-git/tests/project_repository.rs` (new).

- [x] Add focused temporary-repository cases for external symlinks, existing internal repositories, root repositories, duplicate targets, clone failures, and removal that preserves source directories.
- [x] Implement root validation using `git rev-parse --show-toplevel` plus canonical paths; reject subdirectories masquerading as roots.
- [x] Bind external repositories using an exclusive symlink creation; clone with argument-separated Git commands into an exclusively reserved empty destination.
- [x] Permit worksets inside Git project roots and add local excludes for managed directories.

Implementation adjustment: reserve the final clone directory with `create_dir` rather than stage-and-rename. Git accepts an existing empty destination; this avoids platform-dependent rename overwrite behavior. A failed nonempty clone directory is retained and reported, never recursively deleted.

### Task 2: Persistence and runtime commands

Files: `crates/smelt-model/src/{workspace_directory.rs,control_api.rs}`, `crates/smelt-runtime-client/src/protocol/control.rs`, `crates/smelt-store/src/{snapshots.rs,schema.rs,workspace.rs,lib.rs}`, `crates/smelt-core/src/workspace_catalog.rs`, `crates/smeltd/src/{workset_control.rs,control.rs,workspace_directory.rs}`.

- [x] Persist `managed_link: Option<String>` alongside repository root, with a v38 migration for current v37 databases and migration chaining from older versions.
- [x] Introduce one repository action RPC for initializing/discovering roots, binding, cloning, and removing; return the authoritative directory.
- [x] Serialize filesystem/catalog updates, reject duplicate canonical roots and repositories referenced by worksets; safely roll back only owned links on persistence failure. Clone network IO does not hold the catalog lock.
- [x] Automatically register a root repository when first opening an unconfigured project; persist a dedicated initialization marker so explicit removal also survives restart and reopening.

### Task 3: Desktop modal and picker

Files: `crates/smelt/src/project_repositories.rs` (new), `crates/smelt/src/{main.rs,overlay.rs,dir_picker.rs,session_list.rs,workspace_sessions.rs,workspace_view.rs}`, `crates/smelt/src/session_list/new_session.rs`.

- [x] Add a modal state with repository list, bind/clone fields, selected repository names, busy flag, and inline error.
- [x] Extend the existing directory picker purpose with project repository selection and pass chosen paths to the modal.
- [x] Add root/inside/external source labels, folder selection, clone URL/name form, safe remove actions, and empty-state guidance.
- [x] Create worksets from a task input and checkbox list, displaying generated branch/path; submit asynchronously and preserve failed form inputs.
- [x] Add project actions to the existing plus popover and right-click menu; remove delimiter parsing variants and old text RPC call sites.

### Task 4: Static review and validation handoff

- [x] Inspect all enum matches, record constructors, migration shape checks, overlay dismissal and async modal lifetime paths.
- [x] Run only `rustfmt --check` and `git diff --check`; do not run builds/tests under AGENTS.md. No shell scripts changed in this iteration.
- [x] Provide commands for user-run verification and packaging.

### User-run verification

After explicit user authorization, `make dist-build` completed successfully on 2026-11-10. The build exposed a missing `ProjectRepository` arm in the remote directory picker's confirmation-label match; it was added before rebuilding. The signed `dist/Smelt.app` and `dist/Smelt.dmg` passed signature and disk-image integrity verification. No test suites were run, and the currently running E2E instance was not restarted.

```bash
cd /Users/pengxu.liu/workspace/SMELT/worktrees/workspace-session-binding/smelt
cargo check -p smelt -p smeltd
cargo test -p smelt-git --test project_repository --test worktree
cargo test -p smelt-store repositor
cargo test -p smelt-store workset_
cargo test -p smeltd repository
cargo test -p smelt project_repositories
```

To regenerate the local E2E package after further changes:

```bash
SMELT_CODESIGN_IDENTITY=937DA55D75980DA036BB3B1D29AE7A7513954C9A make dist-build
```
