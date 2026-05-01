use super::{SandboxBackend, SandboxError, SandboxSpec};
use async_trait::async_trait;
use tokio::process::Command;

#[derive(Debug, Clone, Default)]
pub struct DockerSandboxBackend;

impl DockerSandboxBackend {
    pub fn run_args(spec: &SandboxSpec) -> Vec<String> {
        vec![
            "run".to_owned(),
            "-d".to_owned(),
            "--name".to_owned(),
            spec.sandbox_id.to_string(),
            "--network".to_owned(),
            spec.network.clone(),
            "--cap-add".to_owned(),
            "NET_ADMIN".to_owned(),
            "--security-opt".to_owned(),
            "no-new-privileges".to_owned(),
            "-v".to_owned(),
            format!("{}:/workspace", spec.workspace_volume),
            "-e".to_owned(),
            format!("TELELLM_BROKER_HOST={}", spec.broker_host),
            "-e".to_owned(),
            format!("TELELLM_BROKER_PORT={}", spec.broker_port),
            "-e".to_owned(),
            format!("OPENAI_BASE_URL={}", spec.broker_base_url),
            "-e".to_owned(),
            format!("OPENAI_API_KEY={}", spec.broker_token),
            "-w".to_owned(),
            "/workspace".to_owned(),
            spec.image.clone(),
            "sleep".to_owned(),
            "infinity".to_owned(),
        ]
    }

    pub fn exec_args(spec: &SandboxSpec, command: &[&str]) -> Vec<String> {
        let mut args = vec![
            "exec".to_owned(),
            "-i".to_owned(),
            "-t".to_owned(),
            "--user".to_owned(),
            "codex".to_owned(),
            spec.sandbox_id.to_string(),
        ];
        args.extend(command.iter().map(|part| (*part).to_owned()));
        args
    }

    async fn docker_output(args: &[String]) -> Result<std::process::Output, SandboxError> {
        Command::new("docker")
            .args(args)
            .output()
            .await
            .map_err(SandboxError::Io)
    }

    async fn docker(args: &[String]) -> Result<(), SandboxError> {
        let output = Self::docker_output(args).await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(SandboxError::Docker(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }

    async fn container_running(spec: &SandboxSpec) -> Result<Option<bool>, SandboxError> {
        let output = Self::docker_output(&[
            "inspect".to_owned(),
            "--format".to_owned(),
            "{{.State.Running}}".to_owned(),
            spec.sandbox_id.to_string(),
        ])
        .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("No such object") || stderr.contains("No such container") {
                return Ok(None);
            }
            return Err(SandboxError::Docker(stderr.into_owned()));
        }

        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim() == "true",
        ))
    }

    async fn start_existing(spec: &SandboxSpec) -> Result<(), SandboxError> {
        Self::docker(&["start".to_owned(), spec.sandbox_id.to_string()]).await
    }

    async fn docker_ignore_missing(
        args: &[String],
        missing_marker: &str,
    ) -> Result<(), SandboxError> {
        match Self::docker(args).await {
            Ok(()) => Ok(()),
            Err(SandboxError::Docker(message)) if message.contains(missing_marker) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[async_trait]
impl SandboxBackend for DockerSandboxBackend {
    async fn ensure_started(&self, spec: &SandboxSpec) -> Result<(), SandboxError> {
        match Self::container_running(spec).await? {
            Some(true) => Ok(()),
            Some(false) => Self::start_existing(spec).await,
            None => Self::docker(&Self::run_args(spec)).await,
        }
    }

    async fn restart(&self, spec: &SandboxSpec) -> Result<(), SandboxError> {
        Self::docker(&["restart".to_owned(), spec.sandbox_id.to_string()]).await
    }

    async fn rebuild(&self, spec: &SandboxSpec, clear_workspace: bool) -> Result<(), SandboxError> {
        Self::docker_ignore_missing(
            &[
                "rm".to_owned(),
                "-f".to_owned(),
                spec.sandbox_id.to_string(),
            ],
            "No such container",
        )
        .await?;
        if clear_workspace {
            Self::docker_ignore_missing(
                &[
                    "volume".to_owned(),
                    "rm".to_owned(),
                    spec.workspace_volume.clone(),
                ],
                "No such volume",
            )
            .await?;
        }
        self.ensure_started(spec).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ChatId;

    #[test]
    fn run_args_should_not_mount_host_paths_or_docker_socket() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::run_args(&spec).join(" ");

        assert!(!args.contains("/var/run/docker.sock"));
    }

    #[test]
    fn run_args_should_mount_only_named_workspace_volume() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::run_args(&spec);

        assert!(args.contains(&"vol_telellm-chat-1:/workspace".to_owned()));
    }

    #[test]
    fn run_args_should_expose_only_the_broker_exception_to_entrypoint() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::run_args(&spec);

        assert!(args.contains(&"TELELLM_BROKER_HOST=host.docker.internal".to_owned()));
    }

    #[test]
    fn run_args_should_point_codex_at_host_broker() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::run_args(&spec);

        assert!(args.contains(&"OPENAI_BASE_URL=http://host.docker.internal:8189/v1".to_owned()));
    }

    #[test]
    fn exec_args_should_run_commands_as_codex_user() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::exec_args(&spec, &["codex"]);

        assert!(
            args.windows(2)
                .any(|window| window[0] == "--user" && window[1] == "codex")
        );
    }

    #[test]
    fn exec_args_should_allocate_interactive_tty() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::exec_args(&spec, &["codex"]);
        let stdin_arg = args.iter().position(|arg| arg == "-i");
        let tty_arg = args.iter().position(|arg| arg == "-t");
        let user_arg = args.iter().position(|arg| arg == "--user");

        assert!(matches!(
            (stdin_arg, tty_arg, user_arg),
            (Some(stdin), Some(tty), Some(user)) if stdin < tty && tty < user
        ));
    }
}
