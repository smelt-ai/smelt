# Documentation

> 索引用英文，文档正文用中文。产品路线图是 [`product-roadmap.md`](product-roadmap.md)；[`archive/`](archive/) 里的都是历史快照，不要拿来开工。

Product direction is [`product-roadmap.md`](product-roadmap.md). Everything under [`archive/`](archive/) is a historical snapshot, not current planning.

## User / product

How the product works today.

- [`workspace.md`](workspace.md) — Desktop workspace: terminals, agents, layout, multi-runtime windows, and shortcuts. Also the mobile client overview.
- [`local-storage.md`](local-storage.md) — The two SQLite databases under `~/.smelt/` and what they store.
- [`iroh-relay-deployment.md`](iroh-relay-deployment.md) — When and how to run a private iroh relay.
- [`release.md`](release.md) — Tags, release artifacts, and stable versus pre-release.
- [`ui-design-language.md`](ui-design-language.md) — Desktop visual language: theme tokens, surfaces, and chrome.
- [`mobile/README.md`](../mobile/README.md) — Building the Flutter mobile client.

## Development

### Architecture

Current behavior. Keep these in sync with the code.

- [`control-api.md`](control-api.md) — smeltd Control API v3, the short request protocol shared by desktop, mobile, and CLI.
- [`notification-architecture.md`](notification-architecture.md) — Status, attention, and notification model.
- [`event-bus-architecture.md`](event-bus-architecture.md) — Event bus and `event_subscribe`. Plugin-host sections describe a removed design.
- [`terminal-rendering-gaps.md`](terminal-rendering-gaps.md) — Living notes versus Zed. The main render path is aligned; a few input and platform gaps remain.

### Designs

Each design states its status at the top.

- [`agent-memory.md`](agent-memory.md) — Agent memory. Phase 1 is implemented; later work is listed in the doc.
- [`subagents.md`](subagents.md) — Pi subagents as entities with their own session files. Implemented; the mobile UI is not.
- [`pi-session-messaging-design.html`](pi-session-messaging-design.html) — Pi↔Pi session messaging (transcript as inbox). P0 is implemented (#55); P1 and P2 are not.
- [`pi-runtime-compile-design.html`](pi-runtime-compile-design.html) — Compile the Pi runtime into the single `smelt-pi` binary. Implemented (#62); see its "落地差异" section.
- [`mobile-redesign.md`](mobile-redesign.md) — Mobile rebuilt as a multi-machine command board. Implemented (#69).
- [`windows-support.md`](windows-support.md) — Windows desktop and a native smeltd. Implemented (#74, #83): x86_64 portable zip, unsigned, no installer and no auto-update.
- [`collaboration.md`](collaboration.md) — Multiplayer around a live session. Not started.

## Product / planning

- [`product-roadmap.md`](product-roadmap.md) — Canonical product roadmap.
- [`roadmap.md`](roadmap.md) — Miscellaneous backlog. Not the product roadmap.

## Archive

Historical snapshots in [`archive/`](archive/). Paths, protocols, and plans in them may no longer match the tree.

Reviews:

- [`REVIEW_INDEX.md`](archive/REVIEW_INDEX.md), [`ARCHITECTURE_REVIEW_SUMMARY.md`](archive/ARCHITECTURE_REVIEW_SUMMARY.md), [`ARCHITECTURE_REVIEW.md`](archive/ARCHITECTURE_REVIEW.md) — The old architecture review. Paths and the gateway no longer match the tree.
- [`IMPLEMENTATION_ROADMAP.md`](archive/IMPLEMENTATION_ROADMAP.md) — 13-week plan written from that review. Not current planning.
- [`SMELT_CORE_REFACTORING_GUIDE.md`](archive/SMELT_CORE_REFACTORING_GUIDE.md) — Proposed `smelt-core` split that was not carried out.
- [`code-review-2026-10-19.md`](archive/code-review-2026-10-19.md) — Stale comments and dead code found on 2026-10-19.

Superseded designs and finished plans:

- [`remote-ops-roadmap.md`](archive/remote-ops-roadmap.md) — Removed JSON-WS gateway. Not the current remote protocol.
- [`remote-runtime.html`](archive/remote-runtime.html) — Design record for the runtime cutover.
- [`mobile-runtime-p4.md`](archive/mobile-runtime-p4.md) — Mobile runtime cutover. Landed; the steps are not a todo list.
- [`mobile-ux-redesign.md`](archive/mobile-ux-redesign.md), [`mobile-agents-ux.md`](archive/mobile-agents-ux.md) — Earlier mobile information architecture and agent UX. Superseded by `mobile-redesign.md`.
- [`agent-state-notification-design.md`](archive/agent-state-notification-design.md) — Design motivation. Current behavior is `notification-architecture.md`.
- [`git-changes-panel-plan.md`](archive/git-changes-panel-plan.md), [`project-report.md`](archive/project-report.md), [`session-migration-plan.md`](archive/session-migration-plan.md), [`share.md`](archive/share.md), [`state-channel-plan.md`](archive/state-channel-plan.md), [`terminal-selection-plan.md`](archive/terminal-selection-plan.md) — Older plans and reports.
- [`superpowers/`](archive/superpowers/) — Dated ACP lifecycle hardening and strict-resume notes (2026-07).

Mockups:

- [`conversation-projection.html`](archive/conversation-projection.html) — Conversation projection.
- [`mockups/`](archive/mockups/) — HTML prototypes: `agent-chat-and-cron.html`, `new-session-picker.html`, `agent-definition-v1.html`, `composer-shell-v1.html`, `mobile-ux-redesign-v1.html`.
