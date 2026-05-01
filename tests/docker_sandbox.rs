use std::process::Command;

#[test]
#[ignore = "requires Docker/Colima and the telellm-sandbox:local image"]
fn sandbox_should_block_private_lan_address() {
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--cap-add",
            "NET_ADMIN",
            "telellm-sandbox:local",
            "sh",
            "-lc",
            "curl --max-time 2 http://192.168.1.1",
        ])
        .output()
        .expect("docker should run");

    assert!(!output.status.success());
}

#[test]
#[ignore = "requires Docker/Colima and the telellm-sandbox:local image"]
fn sandbox_should_allow_public_internet() {
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--cap-add",
            "NET_ADMIN",
            "telellm-sandbox:local",
            "sh",
            "-lc",
            "curl --max-time 10 -fsS https://api.openai.com",
        ])
        .output()
        .expect("docker should run");

    assert!(output.status.success());
}
