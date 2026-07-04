use clap::Parser;
use music_cli::cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Classic commands log to stderr as always. The TUI owns the whole
    // terminal (alternate screen + raw mode), where stray stderr writes
    // corrupt the display — its logs go to a file when CRATES_CLI_LOG
    // names one, and are discarded otherwise.
    let env_filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,music=info"))
    };
    if matches!(cli.command, Command::Tui) {
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
