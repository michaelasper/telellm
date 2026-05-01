use anyhow::Context;
use clap::Parser;
use telellm::{app, config::AppConfig};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "telellm")]
#[command(about = "Telegram group chat bridge for sandboxed Codex CLI sessions")]
struct Cli {
    #[arg(long, default_value = "config.toml")]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let config = AppConfig::from_path(&cli.config)
        .with_context(|| format!("failed to load config from {}", cli.config.display()))?;

    app::run(config).await
}
