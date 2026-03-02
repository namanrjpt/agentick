# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is agentick?

A terminal UI (TUI) session manager for AI coding agents. It manages multiple AI tool sessions (Claude, Gemini, Codex, OpenCode, Cursor, Aider, shell) via tmux, showing live previews, status detection, and context window usage in a single dashboard. Requires tmux at runtime.

## Build & Test Commands

```bash
cargo build                    # debug build
cargo build --release          # optimized release build (LTO + strip)
cargo test                     # run all tests
cargo test session::instance   # run tests in a specific module
cargo test -- --nocapture      # see stdout in test output
cargo clippy                   # lint
```

Rust edition is 2024. No workspace — single crate at the repo root.

## Architecture

### Module structure

- **`src/main.rs`** — CLI entry point (clap). Subcommands: `list`, `add`. No subcommand launches the TUI.
- **`src/session/`** — Core data model
  - `instance.rs` — `Session`, `Tool` (enum of supported AI tools), `Status` (Active/Waiting/Done/Idle/Dead), `Group`. Status is `#[serde(skip)]` — it's computed at runtime, not persisted.
  - `store.rs` — `SessionStore` (Vec of sessions + groups), serialized as JSON to `~/.agentick/sessions.json`.
  - `tokens.rs` — Token/context extraction from AI tool session files. Reads Claude JSONL (`~/.claude/projects/`), Gemini session JSON (`~/.gemini/tmp/`), Codex rollout JSONL (`~/.codex/`). Uses tail-read (last 64KB) + mtime cache to avoid blocking the UI.
- **`src/tmux/`** — All tmux interaction
  - `client.rs` — Subprocess-based tmux commands (create/kill/attach/capture-pane/send-keys/list-sessions). Includes timeout protection and batch helpers (`refresh_all_pane_data`).
  - `control.rs` — `TmuxControlClient`: persistent `tmux -C` control-mode connection for low-latency keystroke forwarding (~0.07ms pipe write vs ~5.5ms fork+exec). Background reader thread parses `%output` protocol.
  - `detector.rs` — 5-layer status detection pipeline: Dead → Hooks → Title (braille spinner) → Content (busy indicators, prompt patterns) → Timestamps. Tool-specific pattern matching.
- **`src/tui/`** — UI layer (ratatui + crossterm)
  - `app.rs` — Main event loop (`run()`). Manages `AppMode` (Normal, NewSession, ConfirmDelete, ConfirmKill, Search, GroupDialog), focus pane (Left=list, Right=interactive terminal), tick-based refresh, and all keyboard handling.
  - `views/dashboard.rs` — Main dashboard: top status bar, grouped session list with tree connectors, preview pane (shows live tmux capture with ANSI colors), scrollback support, help bar.
  - `views/new_session.rs` — New session dialog with zoxide-powered directory picker, tool selector, title/group fields.
  - `views/search.rs` — Fuzzy search bar overlay.
  - `views/group_dialog.rs` — Group creation dialog.
  - `theme.rs` — Tokyo Night dark color theme. Status and tool colors.
  - `keymap.rs` — Maps crossterm `KeyEvent` to tmux key representations (Literal/Special/Ignore).
  - `zoxide.rs` — Loads and fuzzy-filters directories from zoxide using nucleo-matcher.
- **`src/hooks/`** — Claude Code hook integration
  - `setup.rs` — Auto-installs `~/.agentick/bin/hook-handler.sh` and injects hook entries into `~/.claude/settings.json` on startup.
  - `mod.rs` — Reads hook status JSON files from `~/.agentick/hooks/`, with 2-minute freshness window.
- **`src/config/`** — Config from `~/.agentick/config.toml`. Data dir is `~/.agentick/`.

### Key patterns

- **Status is runtime-only**: `Session.status` is `#[serde(skip)]` and computed each tick via the detector pipeline. Never persisted.
- **Tmux session naming**: All tmux sessions use `agentick_{sanitized_title}_{id_prefix}` format via `sanitize_session_name()`.
- **Dual tmux communication**: Batch commands via subprocess (`client.rs`) for status checks; persistent control-mode pipe (`control.rs`) for real-time keystroke forwarding in interactive mode.
- **Token cache**: `TokenCache` (HashMap<PathBuf, (SystemTime, TokenData)>) is passed through the app to avoid re-parsing unchanged session files.
- **Test framework**: Unit tests use `#[cfg(test)]` inline modules. `insta` for snapshot tests, `tempfile` for filesystem tests.
