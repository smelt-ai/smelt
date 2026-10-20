# Documentation

> 索引用英文。现行中文文档只改了搬家后的链接。产品路线图是 [`product-roadmap.md`](product-roadmap.md)，不是 `IMPLEMENTATION_ROADMAP.md`，也不是曾经放在仓库根目录的审查稿。那些文件在 [`archive/`](archive/)，不要拿来开工。

Product direction is [`product-roadmap.md`](product-roadmap.md). Do not treat [`archive/IMPLEMENTATION_ROADMAP.md`](archive/IMPLEMENTATION_ROADMAP.md) or the archived root review files as current planning.

## User / product

How the product works today.

- [`workspace.md`](workspace.md) — Desktop workspace: terminals, agents, layout, multi-runtime windows, and shortcuts.
- [`local-storage.md`](local-storage.md) — The two SQLite databases under `~/.smelt/` and what they store.
- [`iroh-relay-deployment.md`](iroh-relay-deployment.md) — When and how to run a private iroh relay.
- [`release.md`](release.md) — Tags, release artifacts, and stable versus pre-release.
- [`ui-design-language.md`](ui-design-language.md) — Desktop visual language: theme tokens, surfaces, and chrome.
- [`mobile/README.md`](../mobile/README.md) — Building the Flutter mobile client.
- [`product-roadmap.md`](product-roadmap.md) — Product direction. Also the planning entry below.

## Development

Contributor-facing notes that are still current.

- [`control-api.md`](control-api.md) — smeltd Control API v1, the short request protocol shared by desktop, mobile, and CLI.
- [`collaboration.md`](collaboration.md) — Design for multiplayer around a live session. Not implemented.
- [`pi-session-messaging-design.html`](pi-session-messaging-design.html) — Pi↔Pi session messaging design (transcript-as-inbox; not implemented).
- [`pi-runtime-compile-design.html`](pi-runtime-compile-design.html) — Compile the Pi runtime into one Bun binary. Implementation lands on the same PR.
- [`notification-architecture.md`](notification-architecture.md) — Current status, attention, and notification model.
- [`event-bus-architecture.md`](event-bus-architecture.md) — Event bus and `event_subscribe`. Plugin-host sections describe a removed design.
- [`agent-memory.md`](agent-memory.md) — Agent memory. Phase 1 is implemented; later work is listed in the doc.
- [`terminal-rendering-gaps.md`](terminal-rendering-gaps.md) — Living notes versus Zed. The main render path is aligned; a few input and platform gaps remain.

## Product / planning

- [`product-roadmap.md`](product-roadmap.md) — Canonical product roadmap.
- [`roadmap.md`](roadmap.md) — Miscellaneous backlog. Not the product roadmap.

## Archive

Historical snapshots live in [`archive/`](archive/). They are not the place to start work.

Moved in from the repo root:

- [`REVIEW_INDEX.md`](archive/REVIEW_INDEX.md) — Index of the old architecture review.
- [`ARCHITECTURE_REVIEW_SUMMARY.md`](archive/ARCHITECTURE_REVIEW_SUMMARY.md) — One-page summary of that review.

Moved in from `docs/`:

- [`ARCHITECTURE_REVIEW.md`](archive/ARCHITECTURE_REVIEW.md) — Full architecture review. Paths and the gateway no longer match the tree.
- [`IMPLEMENTATION_ROADMAP.md`](archive/IMPLEMENTATION_ROADMAP.md) — 13-week plan written from that review. Not current planning.
- [`SMELT_CORE_REFACTORING_GUIDE.md`](archive/SMELT_CORE_REFACTORING_GUIDE.md) — Proposed `smelt-core` split that was not carried out.
- [`remote-ops-roadmap.md`](archive/remote-ops-roadmap.md) — Removed JSON-WS gateway. Not the current remote protocol.
- [`agent-state-notification-design.md`](archive/agent-state-notification-design.md) — Design motivation. Current behavior is `notification-architecture.md`.
- [`mobile-runtime-p4.md`](archive/mobile-runtime-p4.md) — Mobile runtime cutover. Landed; the steps are not a todo list.
- [`mobile-ux-redesign.md`](archive/mobile-ux-redesign.md) — Construction notes from an earlier mobile information architecture.
- [`mobile-agents-ux.md`](archive/mobile-agents-ux.md) — Mobile agent UX phase notes. Slices 5 and 7 were still open in the doc, but it still describes the removed gateway, so it is not the current entry.
- [`conversation-projection.html`](archive/conversation-projection.html) — HTML mock of conversation projection.
- [`remote-runtime.html`](archive/remote-runtime.html) — HTML design record for the runtime cutover.
- [`mockups/`](archive/mockups/) — HTML prototypes (`agent-chat-and-cron.html`, `new-session-picker.html`), moved here with the other mockups.

Already in `archive/` before this index: `git-changes-panel-plan.md`, `project-report.md`, `session-migration-plan.md`, `share.md`, `state-channel-plan.md`, `terminal-selection-plan.md`.

Left in place, and also historical:

- [`superpowers/archive/`](superpowers/archive/) — Dated ACP lifecycle notes. Not moved.
- [`assets/`](assets/) — Static HTML mockups. Not moved. The mobile redesign mock is [`assets/mobile-ux-redesign-v1.html`](assets/mobile-ux-redesign-v1.html).
