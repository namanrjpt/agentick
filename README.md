# agentick

A terminal UI session manager for AI coding agents. Manage multiple AI tool sessions in a single tmux-powered dashboard with live previews, status detection, and context window tracking.

[![License](https://img.shields.io/github/license/namanrjpt/agentick)](https://github.com/namanrjpt/agentick/blob/main/LICENSE)
[![Crates.io](https://img.shields.io/crates/v/agentick)](https://crates.io/crates/agentick)
[![Downloads](https://img.shields.io/crates/d/agentick)](https://crates.io/crates/agentick)
[![GitHub release](https://img.shields.io/github/v/release/namanrjpt/agentick)](https://github.com/namanrjpt/agentick/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/namanrjpt/agentick/release.yml)](https://github.com/namanrjpt/agentick/actions)
[![GitHub stars](https://img.shields.io/github/stars/namanrjpt/agentick)](https://github.com/namanrjpt/agentick/stargazers)

## Demo

https://github.com/user-attachments/assets/5d9dd8e4-2eb7-4ccf-a8d9-b511bac62d38

## Features

- **Multi-agent dashboard** -- run Claude, Gemini, Codex, Cursor, Aider, and more side by side
- **Live terminal preview** -- see real-time output from each session without switching
- **Auto status detection** -- sessions are classified as Active, Idle, or Dead via a multi-layer detection pipeline
- **Context window usage bars** -- track token consumption for Claude, Gemini, and Codex sessions
- **Quick-create sessions** -- launch new agent sessions with a single hotkey per tool
- **Configurable key bindings** -- customize quick-create keys and other shortcuts via config
- **Fuzzy directory search** -- pick project directories with zoxide integration
- **Interactive mode** -- attach to any session and forward keystrokes in real time
- **Scrollback history** -- scroll through captured terminal output without entering the session
- **Auto-generated session titles** -- sensible defaults based on tool and directory

## Install

### One-liner

```sh
curl -fsSL https://raw.githubusercontent.com/namanrjpt/agentick/main/install.sh | sh
```

### Cargo

```sh
cargo install agentick
```

### Build from source

```sh
git clone https://github.com/namanrjpt/agentick.git
cd agentick
cargo build --release
# binary is at target/release/agentick
```

## Requirements

- **tmux** (required) -- agentick manages all sessions through tmux
- At least one AI CLI tool is optional. The built-in `shell` tool (bash) always works, so you can start using agentick immediately.

## Quick Start

```sh
# Launch the dashboard
agentick

# Or list existing sessions
agentick list

# Add a session from the command line
agentick add --tool claude --dir ~/projects/myapp
```

Once inside the TUI:

1. Press `n` to create a new session
2. Navigate sessions with `j` / `k`
3. Press `Enter` to attach and interact with a session
4. Press `Ctrl+q` to detach back to the dashboard

## Keybindings

| Key | Action |
|---|---|
| `n` | New session |
| `N` | New session in a different directory |
| `d` | Delete session |
| `Enter` | Attach to session (interactive mode) |
| `j` / `k` | Navigate session list |
| `/` | Search sessions |
| `f` | Filter sessions by status |
| `Tab` | Switch focus between list and preview pane |
| `Ctrl+q` | Detach from interactive session |
| `q` | Quit agentick |

## Configuration

agentick reads its configuration from `~/.agentick/config.toml`. All fields are optional and have sensible defaults.

```toml
# ~/.agentick/config.toml

# Default tool when creating a new session via CLI
# default_tool = "claude"

# UI refresh rate in milliseconds (default: 500)
# refresh_rate_ms = 500

# Show context window usage bars (default: true)
# show_token_usage = true

# Max lines in preview pane (0 = unlimited, default: 0)
# preview_lines = 0

# Path to tmux binary (default: "tmux")
# tmux_path = "tmux"

# Seconds of inactivity before marking a session idle (default: 60)
# idle_timeout_secs = 60

# How long hook status files stay valid in seconds (default: 120)
# hook_freshness_secs = 120

# Check GitHub for new versions on startup (default: true)
# check_for_updates = true

# Quick-create keybindings — press a key to instantly launch a tool session
# [quick_create_keys]
# c = "claude"
# g = "gemini"
# x = "codex"
# a = "aider"
# s = "shell"
```

The `[quick_create_keys]` table lets you bind single keys to instantly launch a session with a given tool in your current working directory, skipping the new-session dialog entirely.

## Supported Tools

| Tool | Quick Key | Binary |
|---|---|---|
| Claude | `c` | `claude` |
| Codex | `x` | `codex` |
| Gemini | `g` | `gemini` |
| Cursor | -- | `cursor-agent` |
| Vibe | -- | `vibe` |
| Aider | `a` | `aider` |
| OpenCode | -- | `opencode` |
| Shell | `s` | `bash` |

Quick keys shown above are the defaults when `[quick_create_keys]` is configured. The binary column indicates which CLI executable agentick will invoke inside the tmux session.

## License

MIT
