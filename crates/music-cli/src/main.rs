use clap::Parser;
use music_cli::cli::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,music=info")),
        )
        .init();

    let cli = Cli::parse();
    music_cli::app::run(cli, None).await
}
