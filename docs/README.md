# Documentation

> 索引用英文，文档正文用中文。产品路线图是 [`product-roadmap.md`](product-roadmap.md)。

Product direction is [`product-roadmap.md`](product-roadmap.md).

## User / product

How the product works today.

- [`workspace.md`](workspace.md) — Desktop workspace: terminals, agents, layout, multi-runtime windows, and shortcuts. Also the mobile client overview.
- [`local-storage.md`](local-storage.md) — The SQLite databases under `~/.smelt/` and what they store.
- [`iroh-relay-deployment.md`](iroh-relay-deployment.md) — When and how to run a private iroh relay.
- [`push-relay-deployment.md`](push-relay-deployment.md) — Running the push relay that forwards iOS notifications to APNs.
- [`release.md`](release.md) — Tags, release artifacts, and stable versus pre-release.
- [`ui-design-language.md`](ui-design-language.md) — Desktop visual language: theme tokens, surfaces, copy restraint, and snapshots.
- [`i18n.md`](i18n.md) — Desktop, runtime, CLI, mobile, and JS plugin localization.
- [`plugins/README.md`](plugins/README.md) — Plugin developer guide: getting started, views, commands, layouts, host state, runtime calls, storage, UI, themes, and the `smelt/sdk` reference.
- [`windows-support.md`](windows-support.md) — Windows desktop, native `smeltd` (named pipe), Inno Setup installer, and in-app updater.
- [`mobile/README.md`](../mobile/README.md) — Building the Flutter mobile client.

## Development

### Architecture

Current behavior. Keep these in sync with the code.

- [`app-plugins.md`](app-plugins.md) — The whole app as NSH JS/TSX plugins: host/capability boundaries, NSH A2 runtime, migrated plugins, and remaining migration order.
- [`control-api.md`](control-api.md) — `smeltd` Control API v3, the short request protocol shared by desktop, mobile, and CLI.
- [`notification-architecture.md`](notification-architecture.md) — Status, attention, and notification model.
- [`event-bus-architecture.md`](event-bus-architecture.md) — In-process ephemeral event bus (`smelt-event-protocol` v7) and `event_subscribe`.
- [`plugin-driven-conversations.md`](plugin-driven-conversations.md) — Creating, prompting, and collecting turn results for plugin-driven conversations.
- [`terminal-rendering-gaps.md`](terminal-rendering-gaps.md) — Living notes versus Zed. Rendering and input/protocol checklists are aligned; remaining edge gaps are tracked here.

### Designs & backlog specs

Each design states its status at the top.

- [`agent-memory.md`](agent-memory.md) — Agent memory. Phase 1 is implemented; Phase 2 and hardening backlog are listed in §12.
- [`subagents.md`](subagents.md) — Pi subagents as independent entities with persisted child sessions. Implemented; mobile UI pending.
- [`pi-session-messaging-design.html`](pi-session-messaging-design.html) — Pi↔Pi session messaging (transcript as inbox). P0 is implemented (#55); P1 and P2 are open.
- [`cross-agent-messaging.md`](cross-agent-messaging.md) — Bidirectional peer messaging across all ACP agents (Pi, Codex, Claude Code, Antigravity, custom profiles) via smeltd bus + ACP `mcpServers` + CLI fallback. Implemented.
- [`agent-groups.md`](agent-groups.md) — Local heterogeneous multi-agent groups built on `peer_messages`, normalized runtime SQLite tables (v43), and the `plugins/groups/` JS plugin. PR 1 implemented; cross-machine groups and isolated blind-round worktrees are planned as follow-up PRs.
- [`collaboration.md`](collaboration.md) — Multiplayer around a live session. Not started.
- [`conversation-history-loading.md`](conversation-history-loading.md) — Conversation data model as turns → items: timeline carries item summaries, full content fetched per item, history paged by turn. Proposal, not started.

## Product / planning

- [`product-roadmap.md`](product-roadmap.md) — Canonical product roadmap.
- [`roadmap.md`](roadmap.md) — Miscellaneous backlog. Not the product roadmap.
