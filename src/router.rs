use crate::{
    bot::telegram::{TelegramError, TelegramSink},
    codex::session::{CodexRequest, CodexSession, CodexSessionError},
    ids::ChatId,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, mpsc};

#[derive(Debug, Clone)]
pub struct GroupWorkItem {
    pub chat_id: ChatId,
    pub prompt: String,
}

pub struct Router<S, T>
where
    S: CodexSession + 'static,
    T: TelegramSink + 'static,
{
    queue_depth: usize,
    sessions: Arc<Mutex<HashMap<ChatId, Arc<S>>>>,
    senders: Arc<Mutex<HashMap<ChatId, mpsc::Sender<GroupWorkItem>>>>,
    telegram: Arc<T>,
}

impl<S, T> Router<S, T>
where
    S: CodexSession + 'static,
    T: TelegramSink + 'static,
{
    pub fn new(queue_depth: usize, telegram: Arc<T>) -> Self {
        Self {
            queue_depth: queue_depth.max(1),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            telegram,
        }
    }

    pub fn telegram(&self) -> Arc<T> {
        self.telegram.clone()
    }

    pub async fn register_session(&self, chat_id: ChatId, session: Arc<S>) {
        self.sessions.lock().await.insert(chat_id, session);
        self.senders.lock().await.remove(&chat_id);
    }

    pub async fn enqueue(&self, item: GroupWorkItem) -> Result<(), RouterError> {
        let sender = self.sender_for_chat(item.chat_id).await?;
        sender
            .send(item)
            .await
            .map_err(|_| RouterError::QueueClosed)
    }

    async fn sender_for_chat(
        &self,
        chat_id: ChatId,
    ) -> Result<mpsc::Sender<GroupWorkItem>, RouterError> {
        if let Some(sender) = self.senders.lock().await.get(&chat_id).cloned() {
            return Ok(sender);
        }

        let session = self
            .sessions
            .lock()
            .await
            .get(&chat_id)
            .cloned()
            .ok_or(RouterError::MissingSession(chat_id))?;

        let mut senders = self.senders.lock().await;
        if let Some(sender) = senders.get(&chat_id).cloned() {
            return Ok(sender);
        }

        let telegram = self.telegram.clone();
        let (tx, rx) = mpsc::channel(self.queue_depth);
        Self::spawn_chat_worker(session, telegram, rx);

        senders.insert(chat_id, tx.clone());
        Ok(tx)
    }

    fn spawn_chat_worker(session: Arc<S>, telegram: Arc<T>, mut rx: mpsc::Receiver<GroupWorkItem>) {
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                match session
                    .send(CodexRequest {
                        prompt: item.prompt,
                    })
                    .await
                {
                    Ok(turn) => {
                        if let Err(err) = telegram.send_message(item.chat_id, &turn.output).await {
                            tracing::warn!(
                                error = %err,
                                chat_id = ?item.chat_id,
                                "failed to send Codex response to Telegram"
                            );
                        }
                    }
                    Err(err) => {
                        let text = format!("Codex session failed: {err}");
                        if let Err(send_err) = telegram.send_message(item.chat_id, &text).await {
                            tracing::warn!(
                                error = %send_err,
                                chat_id = ?item.chat_id,
                                "failed to send Codex failure to Telegram"
                            );
                        }
                    }
                }
            }
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("no Codex session registered for chat {0:?}")]
    MissingSession(ChatId),
    #[error("group queue is closed")]
    QueueClosed,
    #[error("Codex error: {0}")]
    Codex(#[from] CodexSessionError),
    #[error("Telegram error: {0}")]
    Telegram(#[from] TelegramError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::session::CodexTurn;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{
        sync::{Mutex, mpsc},
        time::{Duration, timeout},
    };

    struct FakeSession {
        reply_prefix: &'static str,
        active_sends: AtomicUsize,
        max_active_sends: AtomicUsize,
        prompts: Mutex<Vec<String>>,
    }

    impl Default for FakeSession {
        fn default() -> Self {
            Self::with_reply_prefix("reply")
        }
    }

    impl FakeSession {
        fn with_reply_prefix(reply_prefix: &'static str) -> Self {
            Self {
                reply_prefix,
                active_sends: AtomicUsize::new(0),
                max_active_sends: AtomicUsize::new(0),
                prompts: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl CodexSession for FakeSession {
        async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
            let active = self.active_sends.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active_sends.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.prompts.lock().await.push(request.prompt.clone());
            self.active_sends.fetch_sub(1, Ordering::SeqCst);

            Ok(CodexTurn {
                output: format!("{}: {}", self.reply_prefix, request.prompt),
            })
        }

        async fn restart(&self) -> Result<(), CodexSessionError> {
            Ok(())
        }
    }

    struct FakeTelegram {
        messages: mpsc::UnboundedSender<String>,
    }

    #[async_trait]
    impl TelegramSink for FakeTelegram {
        async fn send_message(&self, _chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
            self.messages
                .send(text.to_owned())
                .map_err(|err| TelegramError::Send(err.to_string()))
        }
    }

    fn fake_telegram() -> (Arc<FakeTelegram>, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(FakeTelegram { messages: tx }), rx)
    }

    async fn receive_message(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
        timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("telegram message should arrive before timeout")
            .expect("telegram sender should stay open")
    }

    #[tokio::test]
    async fn enqueue_should_send_work_to_registered_session() {
        let (telegram, mut messages) = fake_telegram();
        let session = Arc::new(FakeSession::default());
        let router = Router::new(4, telegram);
        router.register_session(ChatId(1), session).await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "hello".to_owned(),
            })
            .await
            .expect("enqueue should work");

        let message = receive_message(&mut messages).await;
        assert_eq!(message, "reply: hello");
    }

    #[tokio::test]
    async fn enqueue_should_serialize_concurrent_work_for_a_chat() {
        let (telegram, mut messages) = fake_telegram();
        let session = Arc::new(FakeSession::default());
        let router = Arc::new(Router::new(4, telegram));
        router.register_session(ChatId(1), session.clone()).await;

        let first_router = router.clone();
        let first = tokio::spawn(async move {
            first_router
                .enqueue(GroupWorkItem {
                    chat_id: ChatId(1),
                    prompt: "one".to_owned(),
                })
                .await
        });
        let second_router = router;
        let second = tokio::spawn(async move {
            second_router
                .enqueue(GroupWorkItem {
                    chat_id: ChatId(1),
                    prompt: "two".to_owned(),
                })
                .await
        });

        first
            .await
            .expect("first enqueue task should join")
            .expect("first enqueue should work");
        second
            .await
            .expect("second enqueue task should join")
            .expect("second enqueue should work");
        let _first_message = receive_message(&mut messages).await;
        let _second_message = receive_message(&mut messages).await;

        assert_eq!(session.max_active_sends.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn register_session_should_route_future_work_to_replacement_session() {
        let (telegram, mut messages) = fake_telegram();
        let first_session = Arc::new(FakeSession::with_reply_prefix("first"));
        let second_session = Arc::new(FakeSession::with_reply_prefix("second"));
        let router = Router::new(4, telegram);
        router.register_session(ChatId(1), first_session).await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "before".to_owned(),
            })
            .await
            .expect("first enqueue should work");
        let first_message = receive_message(&mut messages).await;
        assert_eq!(first_message, "first: before");

        router.register_session(ChatId(1), second_session).await;
        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "after".to_owned(),
            })
            .await
            .expect("second enqueue should work");
        let second_message = receive_message(&mut messages).await;
        assert_eq!(second_message, "second: after");
    }

    #[tokio::test]
    async fn enqueue_should_return_error_when_session_is_missing() {
        let (telegram, _messages) = fake_telegram();
        let router = Router::<FakeSession, FakeTelegram>::new(4, telegram);

        let err = router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(404),
                prompt: "hello".to_owned(),
            })
            .await
            .expect_err("enqueue should fail without a registered session");

        assert_eq!(
            err.to_string(),
            "no Codex session registered for chat ChatId(404)"
        );
    }
}
