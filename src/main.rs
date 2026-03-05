mod config;
mod hooks;
mod session;
mod tmux;
mod tui;

use clap::Parser;
use color_eyre::Result;

#[derive(Parser)]
#[command(name = "agentick", version, about = "Beautiful TUI session manager for AI coding agents")]
struct Cli {
    /// Subcommand
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// List all sessions
    List,
    /// Add a new session
    Add {
        /// Project directory path
        path: String,
        /// Tool to use (claude, gemini, codex, opencode, cursor, aider, shell)
        #[arg(short, long, default_value = "claude")]
        tool: String,
    },
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::List) => {
            let store = session::store::SessionStore::load()?;
            for s in &store.sessions {
                println!("{} {} {} {}", s.status.indicator(), s.title, s.tool, s.short_path());
            }
            Ok(())
        }
        Some(Commands::Add { path, tool }) => {
            let mut store = session::store::SessionStore::load()?;
            let tool = session::instance::Tool::from_command(&tool);
            let project_path = std::path::PathBuf::from(&path);
            let session = session::instance::Session::new(String::new(), project_path, tool);
            println!("Created session: {}", session.id);
            store.add_session(session);
            store.save()?;
            Ok(())
        }
        None => {
            // Launch TUI

            if !tmux::client::tmux_available() {
                eprintln!("Error: tmux is required but not found in PATH.");
                eprintln!("Install it: brew install tmux");
                std::process::exit(1);
            }
            // Enable mouse capture so scroll events go to the TUI, not the outer terminal.
            // Enable keyboard enhancement so modifiers on Enter, etc. are detected.
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::EnableMouseCapture,
                crossterm::event::PushKeyboardEnhancementFlags(
                    crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                )
            );
            let mut terminal = ratatui::init();
            let result = tui::app::run(&mut terminal);
            ratatui::restore();
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::PopKeyboardEnhancementFlags,
                crossterm::event::DisableMouseCapture
            );
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_no_args_is_none_command() {
        let cli = Cli::try_parse_from(["agentick"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn cli_list_subcommand() {
        let cli = Cli::try_parse_from(["agentick", "list"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::List)));
    }

    #[test]
    fn cli_add_with_defaults() {
        let cli = Cli::try_parse_from(["agentick", "add", "/tmp/project"]).unwrap();
        match cli.command {
            Some(Commands::Add { path, tool }) => {
                assert_eq!(path, "/tmp/project");
                assert_eq!(tool, "claude"); // default
            }
            _ => panic!("expected Add command"),
        }
    }

    #[test]
    fn cli_add_with_tool_flag() {
        let cli = Cli::try_parse_from(["agentick", "add", "/tmp/project", "--tool", "gemini"]).unwrap();
        match cli.command {
            Some(Commands::Add { path, tool }) => {
                assert_eq!(path, "/tmp/project");
                assert_eq!(tool, "gemini");
            }
            _ => panic!("expected Add command"),
        }
    }

    #[test]
    fn cli_add_with_short_tool_flag() {
        let cli = Cli::try_parse_from(["agentick", "add", "/tmp/project", "-t", "codex"]).unwrap();
        match cli.command {
            Some(Commands::Add { tool, .. }) => assert_eq!(tool, "codex"),
            _ => panic!("expected Add command"),
        }
    }

    #[test]
    fn cli_invalid_subcommand_errors() {
        assert!(Cli::try_parse_from(["agentick", "nonexistent"]).is_err());
    }

    #[test]
    fn cli_add_missing_path_errors() {
        assert!(Cli::try_parse_from(["agentick", "add"]).is_err());
    }
}
