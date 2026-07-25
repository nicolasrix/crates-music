use std::io::IsTerminal;

use clap::{CommandFactory, Parser};
use music_cli::cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Bare invocation: humans on a terminal get the interactive UI; pipes
    // and scripts (agents) get the help text like any missing-subcommand
    // clap error, so nothing ever blocks waiting for keys it can't get.
    let tui_mode = match &cli.command {
        None => {
            if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
                true
            } else {
                Cli::command().print_help()?;
                std::process::exit(2);
            }
        }
        Some(Command::Tui) => true,
        Some(_) => false,
    };

    // Classic commands log to stderr as always. The TUI owns the whole
    // terminal (alternate screen + raw mode), where stray stderr writes
    // corrupt the display — its logs go to a file when CRATES_CLI_LOG
    // names one, and are discarded otherwise.
    let env_filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,music=info"))
    };
    if tui_mode {
        if let Some(path) = std::env::var_os("CRATES_CLI_LOG") {
            let file = std::fs::File::create(&path)?;
            tracing_subscriber::fmt()
                .with_env_filter(env_filter())
                .with_writer(file)
                .with_ansi(false)
                .init();
        } else {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter())
                .with_writer(std::io::sink)
                .init();
        }
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter()).init();
    }

    music_cli::app::run(cli, None).await
}
