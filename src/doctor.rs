use crate::config::{AppConfig, CodexAuthMode};
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Output,
};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorOptions {
    pub config_path: PathBuf,
    pub create_network: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    fn new() -> Self {
        Self { checks: Vec::new() }
    }

    fn push(&mut self, check: DoctorCheck) {
        self.checks.push(check);
    }

    pub fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorStatus::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub detail: String,
}

impl DoctorCheck {
    fn passed(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorStatus::Passed,
            detail: detail.into(),
        }
    }

    fn failed(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorStatus::Failed,
            detail: detail.into(),
        }
    }

    fn skipped(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorStatus::Skipped,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    Passed,
    Failed,
    Skipped,
}

pub async fn run(options: DoctorOptions) -> DoctorReport {
    let mut report = DoctorReport::new();
    let config = match AppConfig::from_path(&options.config_path) {
        Ok(config) => {
            report.push(DoctorCheck::passed(
                "config",
                format!("loaded {}", options.config_path.display()),
            ));
            config
        }
        Err(error) => {
            report.push(DoctorCheck::failed(
                "config",
                format!("failed to load {}: {error}", options.config_path.display()),
            ));
            push_config_dependent_skips(&mut report);
            return report;
        }
    };

    report.push(env_check(
        "telegram token env",
        &config.telegram.bot_token_env,
    ));
    let auth_file_available = match config.codex.auth_mode {
        CodexAuthMode::BrokerApiKey => {
            let broker = match config.broker_config() {
                Ok(broker) => broker,
                Err(error) => {
                    report.push(DoctorCheck::failed(
                        "broker config",
                        format!("broker config is invalid: {error}"),
                    ));
                    push_docker_dependent_skips(&mut report);
                    return report;
                }
            };
            report.push(env_check(
                "upstream API key env",
                &broker.upstream_api_key_env,
            ));
            None
        }
        CodexAuthMode::ChatgptOauth => {
            let check = codex_auth_file_check(config.codex_auth_host_path());
            let passed = check.status == DoctorStatus::Passed;
            report.push(check);
            Some(passed)
        }
    };

    let docker_available = docker_version_check().await;
    let can_use_docker = docker_available.status == DoctorStatus::Passed;
    report.push(docker_available);

    if !can_use_docker {
        report.push(DoctorCheck::skipped(
            "sandbox image",
            "skipped because Docker CLI did not respond successfully",
        ));
        report.push(DoctorCheck::skipped(
            "docker network",
            "skipped because Docker CLI did not respond successfully",
        ));
        report.push(DoctorCheck::skipped(
            "codex version probe",
            "skipped because Docker CLI did not respond successfully",
        ));
        if config.codex.auth_mode == CodexAuthMode::ChatgptOauth {
            report.push(DoctorCheck::skipped(
                "codex auth probe",
                "skipped because Docker CLI did not respond successfully",
            ));
        }
        return report;
    }

    let image_check = docker_image_check(&config.docker.image).await;
    let image_available = image_check.status == DoctorStatus::Passed;
    report.push(image_check);

    let network_check = docker_network_check(&config.docker.network, options.create_network).await;
    let network_available = network_check.status == DoctorStatus::Passed;
    report.push(network_check);

    if image_available && network_available {
        report.push(codex_version_probe_check(&config.docker.image, &config.docker.network).await);
        if config.codex.auth_mode == CodexAuthMode::ChatgptOauth {
            if auth_file_available == Some(true) {
                let auth_path = config
                    .codex_auth_host_path()
                    .expect("validated OAuth config should have an auth path");
                report.push(
                    codex_auth_probe_check(&config.docker.image, &config.docker.network, auth_path)
                        .await,
                );
            } else {
                report.push(DoctorCheck::skipped(
                    "codex auth probe",
                    "skipped because the Codex auth file is unavailable",
                ));
            }
        }
    } else {
        report.push(DoctorCheck::skipped(
            "codex version probe",
            "skipped because the sandbox image or Docker network is unavailable",
        ));
        if config.codex.auth_mode == CodexAuthMode::ChatgptOauth {
            report.push(DoctorCheck::skipped(
                "codex auth probe",
                "skipped because the sandbox image or Docker network is unavailable",
            ));
        }
    }

    report
}

pub fn docker_network_create_args(network: &str) -> Vec<String> {
    vec![
        "network".to_owned(),
        "create".to_owned(),
        network.to_owned(),
    ]
}

pub fn codex_version_probe_args(image: &str, network: &str) -> Vec<String> {
    vec![
        "run".to_owned(),
        "--rm".to_owned(),
        "--network".to_owned(),
        network.to_owned(),
        "--cap-add".to_owned(),
        "NET_ADMIN".to_owned(),
        "--security-opt".to_owned(),
        "no-new-privileges".to_owned(),
        image.to_owned(),
        "codex".to_owned(),
        "--version".to_owned(),
    ]
}

pub fn codex_auth_probe_args(image: &str, network: &str, auth_host_path: &Path) -> Vec<String> {
    vec![
        "run".to_owned(),
        "--rm".to_owned(),
        "--network".to_owned(),
        network.to_owned(),
        "--cap-add".to_owned(),
        "NET_ADMIN".to_owned(),
        "--security-opt".to_owned(),
        "no-new-privileges".to_owned(),
        "-v".to_owned(),
        format!(
            "{}:/run/telellm/codex-auth.json:ro",
            auth_host_path.display()
        ),
        image.to_owned(),
        "codex".to_owned(),
        "login".to_owned(),
        "status".to_owned(),
    ]
}

fn docker_image_inspect_args(image: &str) -> Vec<String> {
    vec!["image".to_owned(), "inspect".to_owned(), image.to_owned()]
}

fn docker_network_inspect_args(network: &str) -> Vec<String> {
    vec![
        "network".to_owned(),
        "inspect".to_owned(),
        network.to_owned(),
    ]
}

fn push_config_dependent_skips(report: &mut DoctorReport) {
    for check_name in [
        "telegram token env",
        "upstream API key env",
        "codex auth file",
        "Docker CLI",
        "sandbox image",
        "docker network",
        "codex version probe",
        "codex auth probe",
    ] {
        report.push(DoctorCheck::skipped(
            check_name,
            "skipped because the config did not load",
        ));
    }
}

fn push_docker_dependent_skips(report: &mut DoctorReport) {
    for check_name in [
        "Docker CLI",
        "sandbox image",
        "docker network",
        "codex version probe",
    ] {
        report.push(DoctorCheck::skipped(
            check_name,
            "skipped because auth-dependent config did not load",
        ));
    }
}

fn env_check(name: &str, env_name: &str) -> DoctorCheck {
    match std::env::var(env_name) {
        Ok(value) if value.is_empty() => DoctorCheck::failed(
            name,
            format!("environment variable `{env_name}` is set but empty"),
        ),
        Ok(_) => DoctorCheck::passed(name, format!("environment variable `{env_name}` is set")),
        Err(std::env::VarError::NotPresent) => DoctorCheck::failed(
            name,
            format!("environment variable `{env_name}` is not set"),
        ),
        Err(std::env::VarError::NotUnicode(_)) => DoctorCheck::failed(
            name,
            format!("environment variable `{env_name}` is not valid Unicode"),
        ),
    }
}

fn codex_auth_file_check(path: Result<&Path, crate::config::ConfigError>) -> DoctorCheck {
    let path = match path {
        Ok(path) => path,
        Err(error) => {
            return DoctorCheck::failed("codex auth file", error.to_string());
        }
    };

    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => DoctorCheck::failed(
            "codex auth file",
            format!("auth path `{}` is not a file", path.display()),
        ),
        Ok(metadata) if metadata.len() == 0 => DoctorCheck::failed(
            "codex auth file",
            format!("auth file `{}` is empty", path.display()),
        ),
        Ok(_) => match std::fs::File::open(path) {
            Ok(_) => DoctorCheck::passed(
                "codex auth file",
                format!("auth file `{}` exists and is readable", path.display()),
            ),
            Err(error) => DoctorCheck::failed(
                "codex auth file",
                format!("auth file `{}` is not readable: {error}", path.display()),
            ),
        },
        Err(error) => DoctorCheck::failed(
            "codex auth file",
            format!("auth file `{}` is not readable: {error}", path.display()),
        ),
    }
}

async fn docker_version_check() -> DoctorCheck {
    match docker_output(["version"]).await {
        Ok(output) if output.status.success() => {
            DoctorCheck::passed("Docker CLI", "docker version succeeded")
        }
        Ok(output) => DoctorCheck::failed("Docker CLI", output_failure_detail(&output)),
        Err(error) => DoctorCheck::failed("Docker CLI", format!("failed to run docker: {error}")),
    }
}

async fn docker_image_check(image: &str) -> DoctorCheck {
    match docker_output(docker_image_inspect_args(image)).await {
        Ok(output) if output.status.success() => {
            DoctorCheck::passed("sandbox image", format!("image `{image}` exists"))
        }
        Ok(output) => DoctorCheck::failed(
            "sandbox image",
            format!(
                "image `{image}` is not inspectable: {}",
                output_failure_detail(&output)
            ),
        ),
        Err(error) => DoctorCheck::failed(
            "sandbox image",
            format!("failed to inspect image `{image}`: {error}"),
        ),
    }
}

async fn docker_network_check(network: &str, create_network: bool) -> DoctorCheck {
    match docker_output(docker_network_inspect_args(network)).await {
        Ok(output) if output.status.success() => {
            DoctorCheck::passed("docker network", format!("network `{network}` exists"))
        }
        Ok(output) if create_network && output_mentions_missing_network(&output) => {
            create_docker_network(network).await
        }
        Ok(output) => DoctorCheck::failed(
            "docker network",
            format!(
                "network `{network}` is not inspectable: {}",
                output_failure_detail(&output)
            ),
        ),
        Err(error) => DoctorCheck::failed(
            "docker network",
            format!("failed to inspect network `{network}`: {error}"),
        ),
    }
}

async fn create_docker_network(network: &str) -> DoctorCheck {
    match docker_output(docker_network_create_args(network)).await {
        Ok(output) if output.status.success() => {
            DoctorCheck::passed("docker network", format!("created network `{network}`"))
        }
        Ok(output) => DoctorCheck::failed(
            "docker network",
            format!(
                "failed to create network `{network}`: {}",
                output_failure_detail(&output)
            ),
        ),
        Err(error) => DoctorCheck::failed(
            "docker network",
            format!("failed to create network `{network}`: {error}"),
        ),
    }
}

async fn codex_version_probe_check(image: &str, network: &str) -> DoctorCheck {
    match docker_output(codex_version_probe_args(image, network)).await {
        Ok(output) if output.status.success() => DoctorCheck::passed(
            "codex version probe",
            format!(
                "codex --version succeeded{}",
                output_first_line(&output)
                    .map(|line| format!(": {line}"))
                    .unwrap_or_default()
            ),
        ),
        Ok(output) => DoctorCheck::failed("codex version probe", output_failure_detail(&output)),
        Err(error) => DoctorCheck::failed(
            "codex version probe",
            format!("failed to run codex version probe: {error}"),
        ),
    }
}

async fn codex_auth_probe_check(image: &str, network: &str, auth_host_path: &Path) -> DoctorCheck {
    match docker_output(codex_auth_probe_args(image, network, auth_host_path)).await {
        Ok(output) if output.status.success() => {
            DoctorCheck::passed("codex auth probe", "codex login status succeeded")
        }
        Ok(output) => DoctorCheck::failed("codex auth probe", output_failure_detail(&output)),
        Err(error) => DoctorCheck::failed(
            "codex auth probe",
            format!("failed to run codex auth probe: {error}"),
        ),
    }
}

async fn docker_output<I, S>(args: I) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new("docker").args(args).output().await
}

fn output_mentions_missing_network(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains("No such network") || stderr.contains("network not found")
}

fn output_failure_detail(output: &Output) -> String {
    let status = output.status.code().map_or_else(
        || "terminated by signal".to_owned(),
        |code| format!("exit code {code}"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        return format!("{status}: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        return format!("{status}: {stdout}");
    }

    status
}

fn output_first_line(output: &Output) -> Option<String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

impl fmt::Display for DoctorReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "telellm doctor report")?;
        for check in &self.checks {
            writeln!(
                formatter,
                "[{}] {}: {}",
                check.status, check.name, check.detail
            )?;
        }

        let failed = self
            .checks
            .iter()
            .filter(|check| check.status == DoctorStatus::Failed)
            .count();
        let skipped = self
            .checks
            .iter()
            .filter(|check| check.status == DoctorStatus::Skipped)
            .count();
        if failed == 0 && skipped == 0 {
            writeln!(formatter, "all checks passed")
        } else {
            writeln!(formatter, "{failed} failed, {skipped} skipped")
        }
    }
}

impl fmt::Display for DoctorStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DoctorStatus::Passed => formatter.write_str("pass"),
            DoctorStatus::Failed => formatter.write_str("fail"),
            DoctorStatus::Skipped => formatter.write_str("skip"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_network_create_args_should_use_configured_network() {
        assert_eq!(
            docker_network_create_args("telellm_public"),
            vec!["network", "create", "telellm_public"]
        );
    }

    #[test]
    fn codex_version_probe_args_should_use_configured_image_and_network() {
        let args = codex_version_probe_args("telellm-sandbox:local", "telellm_public");

        assert!(
            args.windows(2)
                .any(|window| window == ["--network", "telellm_public"])
        );
        assert!(args.contains(&"telellm-sandbox:local".to_owned()));
        assert!(args.ends_with(&["codex".to_owned(), "--version".to_owned()]));
    }

    #[test]
    fn codex_auth_probe_args_should_mount_auth_file_and_check_login_status() {
        let args = codex_auth_probe_args(
            "telellm-sandbox:local",
            "telellm_public",
            Path::new("/Users/michaelasper/.codex/auth.json"),
        );

        assert!(
            args.windows(2)
                .any(|window| window == ["--network", "telellm_public"])
        );
        assert!(args.windows(2).any(|window| window[0] == "-v"
            && window[1]
                == "/Users/michaelasper/.codex/auth.json:/run/telellm/codex-auth.json:ro"));
        assert!(args.ends_with(&["codex".to_owned(), "login".to_owned(), "status".to_owned()]));
    }

    #[test]
    fn codex_auth_file_check_should_reject_empty_file() {
        let path = std::env::temp_dir().join(format!("telellm-empty-auth-{}", std::process::id()));
        std::fs::write(&path, "").expect("empty auth fixture should be written");

        let check = codex_auth_file_check(Ok(&path));

        std::fs::remove_file(&path).expect("empty auth fixture should be removed");
        assert_eq!(check.status, DoctorStatus::Failed);
        assert!(check.detail.contains("is empty"));
    }
}
