use std::time::Duration;
use telellm::codex::{
    pty::{PtyCodexSession, PtyReadPolicy},
    session::{CodexRequest, CodexSession},
};

fn fast_read_policy() -> PtyReadPolicy {
    PtyReadPolicy {
        first_byte_timeout: Duration::from_secs(2),
        inactivity_timeout: Duration::from_millis(150),
        max_turn_timeout: Duration::from_secs(5),
        max_output_bytes: 64 * 1024,
    }
}

#[tokio::test]
async fn pty_session_should_read_until_output_is_idle() {
    let args = vec![
        "-lc".to_owned(),
        "while IFS= read -r line; do printf 'first:%s\\n' \"$line\"; sleep 0.05; printf 'second:%s\\n' \"$line\"; done".to_owned(),
    ];
    let session = PtyCodexSession::spawn_with_read_policy("sh", &args, fast_read_policy())
        .expect("shell session should spawn");

    let turn = session
        .send(CodexRequest {
            prompt: "hello from pty".to_owned(),
        })
        .await
        .expect("send should succeed");

    assert_eq!(turn.output, "first:hello from pty\nsecond:hello from pty");
}

#[tokio::test]
async fn pty_session_should_wait_past_prompt_echo_before_returning() {
    let args = vec![
        "-lc".to_owned(),
        "while IFS= read -r line; do sleep 0.3; printf 'answer:%s\\n' \"$line\"; done".to_owned(),
    ];
    let session = PtyCodexSession::spawn_with_read_policy("sh", &args, fast_read_policy())
        .expect("shell session should spawn");

    let turn = session
        .send(CodexRequest {
            prompt: "delayed answer".to_owned(),
        })
        .await
        .expect("send should wait for delayed answer");

    assert_eq!(turn.output, "answer:delayed answer");
}

#[tokio::test]
async fn pty_session_should_collect_more_than_one_read_buffer() {
    let args = vec![
        "-lc".to_owned(),
        "while IFS= read -r line; do python3 - <<'PY'\nprint('x' * 5000)\nprint('done')\nPY\ndone"
            .to_owned(),
    ];
    let session = PtyCodexSession::spawn_with_read_policy("sh", &args, fast_read_policy())
        .expect("shell session should spawn");

    let turn = session
        .send(CodexRequest {
            prompt: "large".to_owned(),
        })
        .await
        .expect("send should succeed");

    assert!(turn.output.len() > 4096);
    assert!(turn.output.contains("done"));
}
