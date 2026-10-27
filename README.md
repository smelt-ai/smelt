[English](README.md) | [简体中文](README.zh-CN.md)

<div align="center">

<img src="assets/icon-1024.png" alt="smelt" width="128">

# smelt

**The AI coding cockpit for macOS and Windows — a desktop workspace designed to orchestrate multiple CLI coding agents at once.**

A native application built with [GPUI](https://gpui.rs), with real embedded terminals, multiple projects, and multiple tabs.
Claude Code, GitHub Copilot, Codex, Grok, Cursor, OpenCode, Kiro, Pi, Crush — any agent that runs in a terminal can be monitored side by side.

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/smelt-ai/smelt)](https://github.com/smelt-ai/smelt/releases)
[![Platform](https://img.shields.io/badge/platform-macOS%20%2B%20Windows%20desktop%20%2B%20Linux%20runtime-lightgrey)](https://github.com/smelt-ai/smelt/releases)

> **Status**: working prototype, under active development. The desktop app runs on macOS (Apple Silicon) and Windows 10 1803+ (x86_64). The runtime (`smeltd`) also runs headless on Linux. The Windows build is an unsigned installer without automatic updates.

</div>

---

## Why

AI plugins make editors smarter, but people are still the ones stuck at the keyboard. When an agent can independently read code, edit code, run tests, and commit changes, the human role should shift from “the person who types” to “the person who navigates and gives direction.”

What is needed then is not a smarter editor, but a **cockpit** for keeping an eye on several agents working at the same time. smelt makes the terminal — where agents actually do their work — the primary workspace.

## Installation

### macOS (Apple Silicon)

Download `Smelt.dmg` from [Releases](https://github.com/smelt-ai/smelt/releases) and drag it into Applications.
The app checks for updates on launch and on demand (stable/beta channel), showing a prompt when a newer version is available.

### Windows (x86_64)

Download `smelt-<version>-windows-x86_64-setup.exe` from the same [Releases](https://github.com/smelt-ai/smelt/releases) page and run it. It installs for the current user without admin rights, into `%LOCALAPPDATA%\Programs\Smelt` by default, adds a Start menu entry, and can put the `smelt` command on your PATH. To upgrade, close Smelt and run the newer installer.

Data lives in `%USERPROFILE%\.smelt`.

This first Windows build:

- x86_64 only, on Windows 10 version 1803 or later.
- The installer and executables are unsigned. SmartScreen may warn about an unknown publisher; choose to run anyway.
- There is no automatic update.
- The interactive shell is `cmd.exe` (ConPTY), or the program named by `COMSPEC`.
- There is no menu-bar icon, Dock badge, or system notification. Those stay macOS-only.

### Linux headless runtime

A Linux machine can run headless `smeltd` and be paired from the macOS app, the Windows app, or the mobile app.

```sh
curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash
curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash -s -- --with-desktop
```

Or download `smeltd-<version>-linux-<arch>.tar.gz` and run `./install.sh`. `--with-desktop` also installs TigerVNC, XFCE, and a resident Chrome, then runs `smeltd mcp add playwright`. `--relay=<url>` sets the initial iroh relay. `--apt-mirror=<mirror>` selects an apt mirror for this install.

```sh
smeltd status
smeltd pair
smeltd devices
smeltd relay [<url>|off]     # show, set, or clear the custom iroh relay
smeltd mcp add <id>          # playwright, figma, rea, browser-mcp
```

## Features

- **Workspace**: Multiple projects and tabs with real embedded terminals for interactive programs and full-screen TUIs such as `claude`, `codex`, `copilot`, `pi`, `vim`, and `htop`; split panes, a command palette (`Cmd+K`), and customizable quick launchers
- **Keep an eye on agents**: Session state comes from agent hooks (`smelt-notify`) and structured protocols (ACP or provider-native). The session name comes from ACP `session_title` or hook `SessionTitleChanged`, and falls back to the first user prompt; OSC 0/2 is not a session name. OSC 9/777 notifications and bells are informational only. Badges and system notifications highlight sessions that need attention, without depending on any vendor-specific private format
- **Native agent conversations**: Connect Claude Code, GitHub Copilot, Codex, Grok, Cursor, OpenCode, Kiro, Crush, and Pi through ACP or provider-native protocols for structured message streams, tool cards, inline diffs, permission prompts, PLAN progress, and resumable sessions. Pi is the single binary `smelt-pi` (`~/.smelt/bin/smelt-pi`) and speaks Pi's official JSONL RPC; it is not wrapped in ACP. Managed Bun (`~/.smelt/runtime`) starts ACP adapters and MCP servers on demand
- **Persistent agents and automations**: The top-level Agents workspace manages reusable definitions, while Automations independently manages triggers, per-run input, and run history. Agent automations get a stable directory under that agent's space (`~/.smelt/agents/<id>/automations/`); shell automations stay in `~/.smelt/workspaces/automations`. Triggers are schedule or an external trigger: schedules cover weekday/daily/interval, while an external trigger is a loopback HTTP URL (`127.0.0.1`) that other programs on the same machine can POST to. The catalog shows “external trigger” without exposing the token. Pi is the first registered engine, and `smeltd` owns scheduled, external-trigger, and manual run lifecycles. Automations run with full access; there is no separate permission policy
- **Read and edit code**: File tree and search, built-in editor, Git diff view, and Markdown/Mermaid rendering
- **Usage and history**: Account usage is queried per provider (Claude, Copilot, Codex, Cursor, Grok) and is not read from transcripts. Historical sessions read each agent's local transcripts; Claude Code is not the only source
- **Remote access** (off by default): Pair another Smelt desktop or the Smelt mobile app to this machine's runtime over iroh. Peers on the same network connect directly; otherwise the connection hole-punches and then uses a relay. Leave the relay blank to use n0's public relay, or fill in your own. The pairing string is `smelt+iroh://<endpoint_id>/?code=<one-time code>` and works once, within 10 minutes. There is no long-lived shared token and no separate "allow write" switch: pairing hands this user's shell to that device. Revoke a device from Settings, or with `smeltd devices revoke`. The app does not open a regular LAN listener.
  **The pairing code grants access. Read the [relay notes](docs/iroh-relay-deployment.md) before turning it on.**
- **More**: Terminal sessions survive GUI exits and crashes and reattach on restart. Upgrading or restarting the local `smeltd` stops it on every platform: running terminal sessions close and in-progress turns are interrupted; conversation history is kept. A terminal cell can stack several terminals (a dock) as well as split. An idle structured conversation suspends its host after 30 minutes and wakes with the previous model, permission mode, and reasoning effort. One window can attach several runtimes, including a headless Linux machine, and open that machine's desktop through noVNC. Details are in [`docs/workspace.md`](docs/workspace.md).

See [`docs/workspace.md`](docs/workspace.md) for the complete feature list, keyboard shortcuts, and architecture details.
See [`docs/product-roadmap.md`](docs/product-roadmap.md) for the product direction.

## Build from source

Rust stable is required. Building the macOS app needs macOS. **A full Xcode installation is not needed** — the project uses the `runtime_shaders` feature in `gpui_platform` to compile Metal shaders at runtime, so Command Line Tools are sufficient.

The `make dist-build` target requires **Python 3.10+** to package a DMG. The `/usr/bin/python3` included with Command Line Tools may still be Python 3.9; if needed, run `brew install python` or set the interpreter explicitly with `SMELT_PYTHON=/opt/homebrew/bin/python3 make dist-build`.

```sh
make dev                    # Build a release and install it to /Applications
cargo run --bin smelt       # Run the GUI only when smeltd is already available
make dist-build             # Build a release and package dist/Smelt.dmg
make help                   # Show all available build targets
```

The Windows installer is built on Windows, not by `make dist-build`. The release job runs `cargo build --release -p smelt -p smelt-cli -p smeltd --locked`, then `bash scripts/build-smelt-pi.sh`. See [`docs/release.md`](docs/release.md).

Run tests and type checks:

```sh
make check-all              # rustfmt + clippy + tests + Pi Agent
make test                   # cargo nextest（与 CI 同一运行器）
```

## Architecture

Rust 2024 + [GPUI](https://github.com/zed-industries/zed) (`gpui-pre` 0.3.9) / [gpui-kit](https://github.com/longbridge/gpui-kit) for the GUI, portable-pty + alacritty_terminal for embedded terminals, tokio, and iroh for remote connections. axum for the automation webhook server (loopback only). Structured state is SQLite under `~/.smelt/` (on Windows, `%USERPROFILE%\.smelt`): `smelt.sqlite3` for the runtime and `client.sqlite3` for this desktop. See [`docs/local-storage.md`](docs/local-storage.md).

The workspace contains `smelt` (the main GUI), `smeltd` (the runtime daemon: PTY sessions, ACP hosts, and automations; the macOS and Windows desktops launch it on demand, and on Linux it can be installed as a systemd user service), `smelt-pi` (the Pi runtime binary), `smelt-notify` (status reports from agent hooks), and `smelt-installer` (the helper that swaps the macOS app in after quit). `smelt-iroh-connect` debugs the runtime's iroh path. Most users only interact with `smelt`; the GUI manages the local `smeltd`.
On Windows, Rust clients (the desktop, `smelt-notify`, and the runtime client) connect to `%USERPROFILE%\.smelt\smeltd.sock` (AF_UNIX via `uds_windows`). Bun and Pi use the current user's named pipe. Child processes sit in a Job Object (`KILL_ON_JOB_CLOSE`). Managed Bun is downloaded as `bun-windows-x64.zip` into `%USERPROFILE%\.smelt\runtime`.
The mobile client lives in `mobile/` (Flutter) and is the cross-machine pass called 出菜口. On launch it connects to every paired machine (recorded in `smelt-mobile.sqlite3`). The first screen is the decisions waiting on you and what each machine is doing. Navigation is four tabs — pass, sessions, agents, settings — with a three-column layout on tablets, in Chinese and English. `crates/smelt-mobile` is its Rust core and uses the same `smelt-runtime-client` as the desktop. Each GitHub Release includes a signed `Smelt-<version>-android-arm64.apk`. iOS TestFlight archives are built with `make ios` (`scripts/build-ios.sh`). See [`mobile/README.md`](mobile/README.md).

For architecture, directory layout, Linux install, multi-runtime windows, remote desktop, and the idle-suspend behavior, see [`docs/workspace.md`](docs/workspace.md).
For the product roadmap, see [`docs/product-roadmap.md`](docs/product-roadmap.md); for the miscellaneous backlog, see [`docs/roadmap.md`](docs/roadmap.md).

## Documentation

Start at [`docs/README.md`](docs/README.md). Current guides: [`workspace`](docs/workspace.md), [`local storage`](docs/local-storage.md), [`iroh relay`](docs/iroh-relay-deployment.md), [`Windows`](docs/windows-support.md), [`product roadmap`](docs/product-roadmap.md), [`miscellaneous backlog`](docs/roadmap.md), [`UI design language`](docs/ui-design-language.md), and the [mobile client](mobile/README.md).

## Contributing

Issues and pull requests are welcome. Before submitting, please make sure `make check-all` passes, and follow [Conventional Commits](https://www.conventionalcommits.org/) for commit messages.

## License

[Apache License 2.0](LICENSE)
