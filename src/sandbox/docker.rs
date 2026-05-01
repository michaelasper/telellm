use super::{SandboxBackend, SandboxError, SandboxSpec};
use async_trait::async_trait;
use std::path::{Component, Path};
use tokio::process::Command;

#[derive(Debug, Clone, Default)]
pub struct DockerSandboxBackend;

impl DockerSandboxBackend {
    fn validate_spec(spec: &SandboxSpec) -> Result<(), SandboxError> {
        let Some(auth_host_path) = &spec.codex_auth_host_path else {
            return Ok(());
        };

        let metadata = std::fs::metadata(auth_host_path).map_err(|error| {
            SandboxError::InvalidCodexAuthPath {
                path: auth_host_path.clone(),
                reason: format!("is not readable: {error}"),
            }
        })?;

        if !metadata.is_file() {
            return Err(SandboxError::InvalidCodexAuthPath {
                path: auth_host_path.clone(),
                reason: "is not a file".to_owned(),
            });
        }

        std::fs::File::open(auth_host_path).map_err(|error| {
            SandboxError::InvalidCodexAuthPath {
                path: auth_host_path.clone(),
                reason: format!("is not readable: {error}"),
            }
        })?;

        Ok(())
    }

    pub fn run_args(spec: &SandboxSpec) -> Vec<String> {
        let mut args = vec![
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
        ];

        if let Some(auth_host_path) = &spec.codex_auth_host_path {
            args.extend([
                "-v".to_owned(),
                format!(
                    "{}:/run/telellm/codex-auth.json:ro",
                    auth_host_path.display()
                ),
            ]);
        } else {
            args.extend([
                "-e".to_owned(),
                format!("TELELLM_BROKER_HOST={}", spec.broker_host),
                "-e".to_owned(),
                format!("TELELLM_BROKER_PORT={}", spec.broker_port),
                "-e".to_owned(),
                format!("OPENAI_BASE_URL={}", spec.broker_base_url),
                "-e".to_owned(),
                format!("OPENAI_API_KEY={}", spec.broker_token),
            ]);
        }

        args.extend([
            "-w".to_owned(),
            "/workspace".to_owned(),
            spec.image.clone(),
            "sleep".to_owned(),
            "infinity".to_owned(),
        ]);
        args
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

    pub fn exec_no_tty_args(spec: &SandboxSpec, command: &[&str]) -> Vec<String> {
        let mut args = vec![
            "exec".to_owned(),
            "-i".to_owned(),
            "--user".to_owned(),
            "codex".to_owned(),
            spec.sandbox_id.to_string(),
        ];
        args.extend(command.iter().map(|part| (*part).to_owned()));
        args
    }

    pub async fn copy_file_to_workspace(
        &self,
        spec: &SandboxSpec,
        source_path: &Path,
        workspace_path: &str,
    ) -> Result<(), SandboxError> {
        Self::validate_workspace_path(workspace_path)?;
        let metadata = std::fs::metadata(source_path)?;
        if !metadata.is_file() {
            return Err(SandboxError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("source path is not a file: {}", source_path.display()),
            )));
        }

        let remote_path = format!("/workspace/{workspace_path}");
        let remote_parent = remote_path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("/workspace");

        Self::docker(&[
            "exec".to_owned(),
            "--user".to_owned(),
            "root".to_owned(),
            spec.sandbox_id.to_string(),
            "mkdir".to_owned(),
            "-p".to_owned(),
            remote_parent.to_owned(),
        ])
        .await?;
        Self::docker(&[
            "cp".to_owned(),
            source_path.display().to_string(),
            format!("{}:{remote_path}", spec.sandbox_id),
        ])
        .await?;
        Self::docker(&[
            "exec".to_owned(),
            "--user".to_owned(),
            "root".to_owned(),
            spec.sandbox_id.to_string(),
            "chown".to_owned(),
            "codex:codex".to_owned(),
            remote_path,
        ])
        .await
    }

    pub async fn export_file_from_workspace(
        &self,
        spec: &SandboxSpec,
        workspace_path: &str,
        destination: &Path,
        max_bytes: u64,
    ) -> Result<u64, SandboxError> {
        Self::validate_workspace_path(workspace_path)?;
        let remote_path = format!("/workspace/{workspace_path}");
        let bytes = Self::workspace_file_size(spec, workspace_path, &remote_path).await?;
        if bytes > max_bytes {
            return Err(SandboxError::WorkspaceFileTooLarge {
                path: workspace_path.to_owned(),
                bytes,
                max_bytes,
            });
        }

        Self::docker(&[
            "cp".to_owned(),
            format!("{}:{remote_path}", spec.sandbox_id),
            destination.display().to_string(),
        ])
        .await?;
        let metadata = std::fs::metadata(destination)?;
        if !metadata.is_file() {
            return Err(SandboxError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("exported workspace path is not a file: {workspace_path}"),
            )));
        }
        Ok(metadata.len())
    }

    async fn workspace_file_size(
        spec: &SandboxSpec,
        workspace_path: &str,
        remote_path: &str,
    ) -> Result<u64, SandboxError> {
        Self::docker(&[
            "exec".to_owned(),
            "--user".to_owned(),
            "root".to_owned(),
            spec.sandbox_id.to_string(),
            "test".to_owned(),
            "-f".to_owned(),
            remote_path.to_owned(),
        ])
        .await?;
        let output = Self::docker_output(&[
            "exec".to_owned(),
            "--user".to_owned(),
            "root".to_owned(),
            spec.sandbox_id.to_string(),
            "stat".to_owned(),
            "-c".to_owned(),
            "%s".to_owned(),
            remote_path.to_owned(),
        ])
        .await?;
        if !output.status.success() {
            return Err(SandboxError::Docker(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u64>()
            .map_err(|error| {
                SandboxError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid size for workspace file {workspace_path}: {error}"),
                ))
            })
    }

    fn validate_workspace_path(workspace_path: &str) -> Result<(), SandboxError> {
        let path = Path::new(workspace_path);
        if path.is_absolute() {
            return Err(SandboxError::InvalidWorkspacePath {
                path: workspace_path.to_owned(),
                reason: "must be relative".to_owned(),
            });
        }

        let mut has_normal_component = false;
        for component in path.components() {
            match component {
                Component::Normal(_) => has_normal_component = true,
                _ => {
                    return Err(SandboxError::InvalidWorkspacePath {
                        path: workspace_path.to_owned(),
                        reason: "must not contain parent, root, or prefix components".to_owned(),
                    });
                }
            }
        }

        if !has_normal_component {
            return Err(SandboxError::InvalidWorkspacePath {
                path: workspace_path.to_owned(),
                reason: "must contain a path component".to_owned(),
            });
        }

        Ok(())
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
            if docker_error_mentions_missing_object(&stderr, "No such container") {
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
            Err(SandboxError::Docker(message))
                if docker_error_mentions_missing_object(&message, missing_marker) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

fn docker_error_mentions_missing_object(message: &str, missing_marker: &str) -> bool {
    message.contains(missing_marker) || message.to_ascii_lowercase().contains("no such object")
}

#[async_trait]
impl SandboxBackend for DockerSandboxBackend {
    async fn ensure_started(&self, spec: &SandboxSpec) -> Result<(), SandboxError> {
        Self::validate_spec(spec)?;
        match Self::container_running(spec).await? {
            Some(true) => Ok(()),
            Some(false) => Self::start_existing(spec).await,
            None => Self::docker(&Self::run_args(spec)).await,
        }
    }

    async fn restart(&self, spec: &SandboxSpec) -> Result<(), SandboxError> {
        Self::validate_spec(spec)?;
        Self::docker(&["restart".to_owned(), spec.sandbox_id.to_string()]).await
    }

    async fn rebuild(&self, spec: &SandboxSpec, clear_workspace: bool) -> Result<(), SandboxError> {
        Self::validate_spec(spec)?;
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
    fn run_args_should_mount_codex_auth_file_for_chatgpt_oauth() {
        let mut spec =
            SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");
        spec.codex_auth_host_path = Some("/home/user/.codex/auth.json".into());

        let args = DockerSandboxBackend::run_args(&spec);

        assert!(args.windows(2).any(|window| window[0] == "-v"
            && window[1] == "/home/user/.codex/auth.json:/run/telellm/codex-auth.json:ro"));
    }

    #[test]
    fn run_args_should_not_set_broker_env_for_chatgpt_oauth() {
        let mut spec =
            SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");
        spec.codex_auth_host_path = Some("/home/user/.codex/auth.json".into());

        let args = DockerSandboxBackend::run_args(&spec);

        assert!(!args.iter().any(|arg| arg.starts_with("OPENAI_BASE_URL=")));
        assert!(!args.iter().any(|arg| arg.starts_with("OPENAI_API_KEY=")));
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("TELELLM_BROKER_HOST="))
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("TELELLM_BROKER_PORT="))
        );
    }

    #[test]
    fn run_args_should_not_mount_codex_auth_file_for_broker_api_key() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::run_args(&spec).join(" ");

        assert!(!args.contains("/run/telellm/codex-auth.json"));
    }

    #[test]
    fn validate_spec_should_reject_missing_codex_auth_file() {
        let mut spec =
            SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");
        spec.codex_auth_host_path = Some(unique_temp_path("missing-auth"));

        let err = DockerSandboxBackend::validate_spec(&spec).expect_err("spec should be invalid");

        assert!(err.to_string().contains("Codex auth host path"));
        assert!(err.to_string().contains("is not readable"));
    }

    #[test]
    fn validate_spec_should_reject_codex_auth_directory() {
        let mut spec =
            SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");
        let path = unique_temp_path("auth-dir");
        std::fs::create_dir(&path).expect("auth fixture directory should be created");
        spec.codex_auth_host_path = Some(path.clone());

        let err = DockerSandboxBackend::validate_spec(&spec).expect_err("spec should be invalid");

        std::fs::remove_dir(&path).expect("auth fixture directory should be removed");
        assert!(err.to_string().contains("Codex auth host path"));
        assert!(err.to_string().contains("is not a file"));
    }

    #[test]
    fn validate_spec_should_accept_existing_codex_auth_file() {
        let mut spec =
            SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");
        let path = unique_temp_path("auth-file");
        std::fs::write(&path, "{}").expect("auth fixture file should be written");
        spec.codex_auth_host_path = Some(path.clone());

        DockerSandboxBackend::validate_spec(&spec).expect("spec should be valid");

        std::fs::remove_file(&path).expect("auth fixture file should be removed");
    }

    #[test]
    fn missing_object_matcher_should_accept_docker_no_such_object_errors() {
        let message = "error: no such object: telellm-chat-1472569";

        assert!(docker_error_mentions_missing_object(
            message,
            "No such container"
        ));
    }

    #[tokio::test]
    async fn ensure_started_should_reject_missing_codex_auth_file_before_docker() {
        let mut spec =
            SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");
        spec.codex_auth_host_path = Some(unique_temp_path("missing-auth"));

        let err = DockerSandboxBackend
            .ensure_started(&spec)
            .await
            .expect_err("sandbox start should fail before Docker");

        assert!(matches!(err, SandboxError::InvalidCodexAuthPath { .. }));
    }

    #[tokio::test]
    async fn rebuild_should_reject_missing_codex_auth_file_before_docker() {
        let mut spec =
            SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");
        spec.codex_auth_host_path = Some(unique_temp_path("missing-auth"));

        let err = DockerSandboxBackend
            .rebuild(&spec, false)
            .await
            .expect_err("sandbox rebuild should fail before Docker");

        assert!(matches!(err, SandboxError::InvalidCodexAuthPath { .. }));
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

    #[test]
    fn exec_no_tty_args_should_omit_tty_for_noninteractive_commands() {
        let spec = SandboxSpec::new(ChatId(1), "telellm-sandbox:local", "telellm_public", "vol");

        let args = DockerSandboxBackend::exec_no_tty_args(&spec, &["codex", "exec"]);

        assert!(args.contains(&"-i".to_owned()));
        assert!(!args.contains(&"-t".to_owned()));
        assert!(args.contains(&"exec".to_owned()));
    }

    #[test]
    fn validate_workspace_path_should_accept_nested_relative_paths() {
        DockerSandboxBackend::validate_workspace_path("telegram_uploads/msg-1/photo.jpg")
            .expect("workspace path should be valid");
    }

    #[test]
    fn validate_workspace_path_should_reject_parent_components() {
        let err = DockerSandboxBackend::validate_workspace_path("../secret.txt")
            .expect_err("workspace path should be invalid");

        assert!(matches!(err, SandboxError::InvalidWorkspacePath { .. }));
    }

    fn unique_temp_path(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("telellm-{label}-{}-{nanos}", std::process::id()))
    }
}
