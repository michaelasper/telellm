#[test]
#[ignore = "requires Docker/Colima, telellm-sandbox:local, broker setup, and OPENAI_API_KEY"]
fn codex_sandbox_should_report_version() {
    let output = std::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "--cap-add",
            "NET_ADMIN",
            "telellm-sandbox:local",
            "codex",
            "--version",
        ])
        .output()
        .expect("docker should run");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
