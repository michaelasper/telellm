use anyhow::Context;
use clap::Parser;
use telellm::{
    app,
    config::AppConfig,
    doctor::{self, DoctorOptions},
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "telellm")]
#[command(about = "Telegram group chat bridge for sandboxed Codex CLI sessions")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    Run {
        #[arg(long, default_value = "config.toml")]
        config: std::path::PathBuf,
    },
    Doctor {
        #[arg(long, default_value = "config.toml")]
        config: std::path::PathBuf,
        #[arg(long)]
        create_network: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run {
        config: std::path::PathBuf::from("config.toml"),
    }) {
        Command::Run { config } => run_app(&config).await,
        Command::Doctor {
            config,
            create_network,
        } => run_doctor(config, create_network).await,
    }
}

async fn run_app(config_path: &std::path::Path) -> anyhow::Result<()> {
    let config = AppConfig::from_path(config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;

    app::run(config).await
}

async fn run_doctor(config: std::path::PathBuf, create_network: bool) -> anyhow::Result<()> {
    let report = doctor::run(DoctorOptions {
        config_path: config,
        create_network,
    })
    .await;

    print!("{report}");
    if report.has_failures() {
        std::process::exit(1);
    }

    Ok(())
}
