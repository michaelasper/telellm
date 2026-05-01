use telellm::codex::{
    pty::PtyCodexSession,
    session::{CodexRequest, CodexSession},
};

#[tokio::test]
async fn pty_session_should_round_trip_input_to_cat_process() {
    let session = PtyCodexSession::spawn("cat", &[]).expect("cat session should spawn");

    let turn = session
        .send(CodexRequest {
            prompt: "hello from pty".to_owned(),
        })
        .await
        .expect("send should succeed");

    assert!(turn.output.contains("hello from pty"));
}
