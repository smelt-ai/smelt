[English](README.md) | [简体中文](README.zh-CN.md)

<div align="center">

<img src="assets/icon-1024.png" alt="smelt" width="128">

# smelt

**The AI coding cockpit for macOS — a desktop workspace designed to orchestrate multiple CLI coding agents at once.**

A native application built with [GPUI](https://gpui.rs), with real embedded terminals, multiple projects, and multiple tabs.
Claude Code, GitHub Copilot, Codex, Grok, Cursor, OpenCode, Kiro, Pi, Crush, DSH — any agent that runs in a terminal can be monitored side by side.

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/smelt-ai/smelt)](https://github.com/smelt-ai/smelt/releases)
[![Platform](https://img.shields.io/badge/platform-macOS%20desktop%20%2B%20Linux%20runtime-lightgrey)](https://github.com/smelt-ai/smelt/releases)

> **Status**: working prototype, under active development. The desktop app is macOS (Apple Silicon). The runtime (`smeltd`) also runs headless on Linux.

</div>

---

## Why

AI plugins make editors smarter, but people are still the ones stuck at the keyboard. When an agent can independently read code, edit code, run tests, and commit changes, the human role should shift from “the person who types” to “the person who navigates and gives direction.”

What is needed then is not a smarter editor, but a **cockpit** for keeping an eye on several agents working at the same time. smelt makes the terminal — where agents actually do their work — the primary workspace.

## Installation

Download `Smelt.dmg` from [Releases](https://github.com/smelt-ai/smelt/releases) and drag it into Applications.
The app checks for updates on launch and on demand (stable/beta channel), showing a prompt when a newer version is available.

> The desktop app currently supports **macOS (Apple Silicon)**. A Linux machine can run headless `smeltd` and be paired from the Mac app or the mobile app.

## Features

- **Workspace**: Multiple projects and tabs with real embedded terminals for interactive programs and full-screen TUIs such as `claude`, `codex`, `copilot`, `pi`, `vim`, and `htop`; split panes, a command palette (`Cmd+K`), and customizable quick launchers
- **Keep an eye on agents**: Session state comes from agent hooks (`smelt-notify`) and structured protocols (ACP or provider-native). The session name comes from ACP `session_title` or hook `SessionTitleChanged`, and falls back to the first user prompt; OSC 0/2 is not a session name. OSC 9/777 notifications and bells are informational only. Badges and system notifications highlight sessions that need attention, without depending on any vendor-specific private format
- **Native agent conversations**: Connect Claude Code, GitHub Copilot, Codex, Grok, Cursor, OpenCode, Kiro, Pi, and DSH through ACP or provider-native protocols for structured message streams, tool cards, inline diffs, permission prompts, PLAN progress, and resumable sessions (Pi uses its native JSONL RPC through Smelt's managed Bun runtime; it is not wrapped in ACP)
- **Persistent agents and automations**: The top-level Agents workspace manages reusable definitions, while Automations independently manages triggers, per-run input, and run history. Agent automations get a stable directory under that agent's space (`~/.smelt/agents/<id>/automations/`); shell automations stay in `~/.smelt/workspaces/automations`. Triggers are schedule or an external trigger: schedules cover weekday/daily/interval, while an external trigger is a loopback HTTP URL (`127.0.0.1`) that other programs on the same machine can POST to. The catalog shows “external trigger” without exposing the token. Pi is the first registered engine, and `smeltd` owns scheduled, external-trigger, and manual run lifecycles. Automations run with full access; there is no separate permission policy
- **Read and edit code**: File tree and search, built-in editor, Git diff view, and Markdown/Mermaid rendering
- **Usage and history**: Account usage is queried per provider (Claude, Copilot, Codex, Cursor, Grok) and is not read from transcripts. Historical sessions read each agent's local transcripts; Claude Code is not the only source
- **Remote access** (off by default): Pair another Smelt desktop or the Smelt mobile app to this machine's runtime over iroh. Peers on the same network connect directly; otherwise the connection hole-punches and then uses a relay. Leave the relay blank to use n0's public relay, or fill in your own. The pairing string is `smelt+iroh://<endpoint_id>/?code=<one-time code>` and works once, within 10 minutes. There is no long-lived shared token and no separate "allow write" switch: pairing hands this user's shell to that device. Revoke a device from Settings, or with `smeltd devices revoke`. The app does not open a regular LAN listener.
  **The pairing code grants access. Read the [relay notes](docs/iroh-relay-deployment.md) before turning it on.**
- **More**: Terminal sessions survive GUI exits and crashes and reattach on restart. A terminal cell can stack several terminals (a dock) as well as split. An idle structured conversation suspends its host after 30 minutes and wakes with the previous model, permission mode, and reasoning effort. One window can attach several runtimes, including a headless Linux machine, and open that machine's desktop through noVNC. Details are in [`docs/workspace.md`](docs/workspace.md).

See [`docs/workspace.md`](docs/workspace.md) for the complete feature list, keyboard shortcuts, and architecture details.
See [`docs/product-roadmap.md`](docs/product-roadmap.md) for the product direction.

## Build from source

Rust stable and macOS are required. **A full Xcode installation is not needed** — the project uses the `runtime_shaders` feature in `gpui_platform` to compile Metal shaders at runtime, so Command Line Tools are sufficient.

The `make dist-build` target requires **Python 3.10+** to package a DMG. The `/usr/bin/python3` included with Command Line Tools may still be Python 3.9; if needed, run `brew install python` or set the interpreter explicitly with `SMELT_PYTHON=/opt/homebrew/bin/python3 make dist-build`.

```sh
make run                    # Build the GUI, daemon, and notifier, then run the GUI
cargo run --bin smelt       # Run the GUI only when smeltd is already available
make dist-build             # Build a release and package dist/Smelt.dmg
make help                   # Show all available build targets
```

Run tests and type checks:

```sh
make check-all              # rustfmt + clippy + tests + Pi Agent
make test                   # cargo nextest（与 CI 同一运行器）
```

## Architecture

Rust 2024 + [GPUI](https://github.com/zed-industries/zed) (`gpui-pre` 0.3.9) / [gpui-kit](https://github.com/longbridge/gpui-kit) for the GUI, portable-pty + alacritty_terminal for embedded terminals, tokio, and iroh for remote connections. axum only serves the loopback webhook that starts an automation. Structured state is SQLite under `~/.smelt/`: `smelt.sqlite3` for the runtime and `client.sqlite3` for this desktop. See [`docs/local-storage.md`](docs/local-storage.md).

The workspace contains `smelt` (the main GUI), `smeltd` (the runtime daemon: PTY sessions, ACP hosts, and automations; launched on demand by the GUI, or installed as a systemd user service on Linux), `smelt-notify` (status reports from agent hooks), and `smelt-installer` (the helper that swaps the app in after quit). `smelt-iroh-connect` debugs the runtime's iroh path. Most users only interact with `smelt`; the GUI manages the local `smeltd`.
The mobile client lives in `mobile/` (Flutter). `crates/smelt-mobile` is its Rust core — device identity, pairing, catalog, conversation, terminal, and an offline SQLite cache — and uses the same `smelt-runtime-client` as the desktop. It is not published yet; build it locally, see [`mobile/README.md`](mobile/README.md).

For architecture, directory layout, Linux install, multi-runtime windows, remote desktop, and the idle-suspend behavior, see [`docs/workspace.md`](docs/workspace.md).
For the product roadmap, see [`docs/product-roadmap.md`](docs/product-roadmap.md); for the miscellaneous backlog, see [`docs/roadmap.md`](docs/roadmap.md).

## Documentation

Start at [`docs/README.md`](docs/README.md). Current guides: [`workspace`](docs/workspace.md), [`local storage`](docs/local-storage.md), [`iroh relay`](docs/iroh-relay-deployment.md), [`product roadmap`](docs/product-roadmap.md), [`miscellaneous backlog`](docs/roadmap.md), [`UI design language`](docs/ui-design-language.md), and the [mobile client](mobile/README.md).

## Contributing

Issues and pull requests are welcome. Before submitting, please make sure `make check-all` passes, and follow [Conventional Commits](https://www.conventionalcommits.org/) for commit messages.

## License

[Apache License 2.0](LICENSE)
