[English](README.md) | [简体中文](README.zh-CN.md)

<div align="center">

<img src="assets/icon-1024.png" alt="smelt" width="128">

# smelt

**The AI coding cockpit for macOS — a desktop workspace designed to orchestrate multiple CLI coding agents at once.**

A native application built with [GPUI](https://gpui.rs), with real embedded terminals, multiple projects, and multiple tabs.
Claude Code, Codex, Gemini CLI — any agent that runs in a terminal can be monitored side by side.

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/smelt-ai/smelt)](https://github.com/smelt-ai/smelt/releases)
[![Platform](https://img.shields.io/badge/platform-macOS%20(Apple%20Silicon)-lightgrey)](https://github.com/smelt-ai/smelt/releases)

> **Status**: working prototype, under active development.

</div>

---

## Why

AI plugins make editors smarter, but people are still the ones stuck at the keyboard. When an agent can independently read code, edit code, run tests, and commit changes, the human role should shift from “the person who types” to “the person who navigates and gives direction.”

What is needed then is not a smarter editor, but a **cockpit** for keeping an eye on several agents working at the same time. smelt makes the terminal — where agents actually do their work — the primary workspace.

## Installation

Download `Smelt.dmg` from [Releases](https://github.com/smelt-ai/smelt/releases) and drag it into Applications.
The app includes in-app updates and will automatically check for and silently download later versions.

> Currently supports **macOS (Apple Silicon)** only.

## Features

- **Workspace**: Multiple projects and tabs with real embedded terminals for interactive programs and full-screen TUIs such as `claude`, `codex`, `copilot`, `vim`, and `htop`; split panes, a command palette (`Cmd+K`), and customizable quick launchers
- **Keep an eye on agents**: Session state is inferred from terminal titles (OSC 0/2), OSC 9/777 notifications, and bells — waiting for approval, waiting for input, running, or ready with results. Badges and system notifications highlight sessions that need attention, without depending on any vendor-specific private format
- **Read and edit code**: File tree and search, built-in editor, Git diff view, and Markdown/Mermaid rendering
- **Claude Code integration**: Usage statistics, historical sessions, and memory browsing by reading local `~/.claude/projects/**` transcripts
- **Remote access** (off by default): Use the Smelt mobile app to view and control local agent sessions. Connect directly over the LAN, or use iroh P2P across networks with hole punching and automatic relay fallback. Pairing QR codes remain valid permanently.
  **The pairing code grants access. Read the [remote access documentation](https://github.com/smelt-ai/smelt/blob/main/website/content/docs.md#远程访问) before enabling it.**
- **More**: Persistent terminal sessions that survive GUI exits and crashes and automatically reattach on restart, plus an optional desktop pet powered by an LLM

See [`docs/workspace.md`](docs/workspace.md) for the complete feature list, keyboard shortcuts, and architecture details.
See [`docs/product-roadmap.md`](docs/product-roadmap.md) for the product direction.

## Build from source

Rust stable and macOS are required. **A full Xcode installation is not needed** — the project uses the `runtime_shaders` feature in `gpui_platform` to compile Metal shaders at runtime, so Command Line Tools are sufficient.

The `make dist-build` target requires **Python 3.10+** to package a DMG. The `/usr/bin/python3` included with Command Line Tools may still be Python 3.9; if needed, run `brew install python` or set the interpreter explicitly with `SMELT_PYTHON=/opt/homebrew/bin/python3 make dist-build`.

```sh
cargo run --bin smelt       # Run the GUI directly in development mode
make dist-build             # Build a release and package dist/Smelt.dmg
make help                   # Show all available build targets
```

Run tests and type checks:

```sh
cargo check --all-targets
cargo test
```

## Architecture

Rust 2021 + [GPUI](https://github.com/zed-industries/zed) / [gpui-component](https://github.com/longbridge/gpui-component) for the GUI, portable-pty + alacritty_terminal for embedded terminals, tokio, axum for the remote gateway, and iroh for P2P tunnels.
Configuration is stored in `~/.smelt/`.

The repository produces four binaries. In daily use, you only need the first: `smelt` (the main GUI), `smeltd` (a terminal persistence daemon, similar to tmux, launched on demand by the GUI), `gateway` (a standalone executable for developing and debugging the remote gateway), and `smelt-notify` (a small status-reporting utility called by Claude Code hooks).
The mobile client lives in `mobile/` (Flutter) and reuses the iroh tunnel from the Rust side through `crates/smelt-mobile`.

For detailed architecture, directory structure, and the implemented feature list, see [`docs/workspace.md`](docs/workspace.md).
For the main product roadmap, see [`docs/product-roadmap.md`](docs/product-roadmap.md); for the miscellaneous backlog, see [`docs/roadmap.md`](docs/roadmap.md); and for remote protocol details, see [`docs/remote-ops-roadmap.md`](docs/remote-ops-roadmap.md).

## Contributing

Issues and pull requests are welcome. Before submitting, please make sure `cargo check --all-targets` and `cargo test` pass, and follow [Conventional Commits](https://www.conventionalcommits.org/) for commit messages.

## License

[Apache License 2.0](LICENSE)
