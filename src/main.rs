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
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[arg(long)]
    config: Option<std::path::PathBuf>,
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

impl Cli {
    fn into_command(self) -> Command {
        self.command.unwrap_or(Command::Run {
            config: self.config.unwrap_or_else(default_config_path),
        })
    }
}

fn default_config_path() -> std::path::PathBuf {
    std::path::PathBuf::from("config.toml")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    match cli.into_command() {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_command<const N: usize>(args: [&str; N]) -> Command {
        Cli::try_parse_from(args).unwrap().into_command()
    }

    #[test]
    fn no_subcommand_should_default_to_run_with_default_config() {
        match parse_command(["telellm"]) {
            Command::Run { config } => {
                assert_eq!(config, std::path::PathBuf::from("config.toml"));
            }
            other => panic!("expected no subcommand to default to run, got {other:?}"),
        }
    }

    #[test]
    fn legacy_config_flag_should_parse_as_run() {
        match parse_command(["telellm", "--config", "legacy.toml"]) {
            Command::Run { config } => {
                assert_eq!(config, std::path::PathBuf::from("legacy.toml"));
            }
            other => panic!("expected legacy config flag to parse as run, got {other:?}"),
        }
    }

    #[test]
    fn run_subcommand_should_keep_own_config_flag() {
        match parse_command(["telellm", "run", "--config", "run.toml"]) {
            Command::Run { config } => {
                assert_eq!(config, std::path::PathBuf::from("run.toml"));
            }
            other => panic!("expected run subcommand, got {other:?}"),
        }
    }

    #[test]
    fn doctor_subcommand_should_keep_config_and_create_network_flags() {
        match parse_command([
            "telellm",
            "doctor",
            "--config",
            "doctor.toml",
            "--create-network",
        ]) {
            Command::Doctor {
                config,
                create_network,
            } => {
                assert_eq!(config, std::path::PathBuf::from("doctor.toml"));
                assert!(create_network);
            }
            other => panic!("expected doctor subcommand, got {other:?}"),
        }
    }
}
