use crate::{
    bot::telegram::{TelegramError, TelegramSink},
    codex::session::{CodexRequest, CodexSession, CodexSessionError},
    ids::ChatId,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, mpsc};

#[derive(Debug, Clone)]
pub struct GroupWorkItem {
    pub chat_id: ChatId,
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnqueueReceipt {
    pub chat_id: ChatId,
    pub queue_position: usize,
}

pub struct Router<S, T>
where
    S: CodexSession + 'static,
    T: TelegramSink + 'static,
{
    queue_depth: usize,
    sessions: Arc<Mutex<HashMap<ChatId, RegisteredSession<S>>>>,
    senders: Arc<Mutex<HashMap<ChatId, WorkerSender>>>,
    telegram: Arc<T>,
}

struct RegisteredSession<S> {
    generation: u64,
    session: Arc<S>,
}

impl<S> Clone for RegisteredSession<S> {
    fn clone(&self) -> Self {
        Self {
            generation: self.generation,
            session: self.session.clone(),
        }
    }
}

#[derive(Clone)]
struct WorkerSender {
    generation: u64,
    sender: mpsc::Sender<GroupWorkItem>,
    state: Arc<WorkerState>,
}

#[derive(Default)]
struct WorkerState {
    active_items: AtomicUsize,
}

impl WorkerState {
    fn begin_item(&self) {
        self.active_items.fetch_add(1, Ordering::SeqCst);
    }

    fn finish_item(&self) {
        self.active_items.fetch_sub(1, Ordering::SeqCst);
    }
}

impl WorkerSender {
    fn queue_position(&self, queue_depth: usize) -> usize {
        let buffered_items = queue_depth.saturating_sub(self.sender.capacity());
        buffered_items + self.state.active_items.load(Ordering::SeqCst)
    }
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

    pub async fn register_session(&self, chat_id: ChatId, generation: u64, session: Arc<S>) {
        self.sessions.lock().await.insert(
            chat_id,
            RegisteredSession {
                generation,
                session,
            },
        );
        self.senders.lock().await.remove(&chat_id);
    }

    pub async fn enqueue(&self, item: GroupWorkItem) -> Result<EnqueueReceipt, RouterError> {
        let chat_id = item.chat_id;
        let sender = self.sender_for_chat(item.chat_id).await?;
        let queue_position = sender.queue_position(self.queue_depth);
        sender
            .sender
            .send(item)
            .await
            .map_err(|_| RouterError::QueueClosed)?;

        Ok(EnqueueReceipt {
            chat_id,
            queue_position,
        })
    }

    async fn sender_for_chat(&self, chat_id: ChatId) -> Result<WorkerSender, RouterError> {
        let registered = self
            .sessions
            .lock()
            .await
            .get(&chat_id)
            .cloned()
            .ok_or(RouterError::MissingSession(chat_id))?;

        let mut senders = self.senders.lock().await;
        if let Some(sender) = senders.get(&chat_id).cloned() {
            if sender.generation == registered.generation {
                return Ok(sender);
            }
            senders.remove(&chat_id);
        }

        let telegram = self.telegram.clone();
        let (tx, rx) = mpsc::channel(self.queue_depth);
        let state = Arc::new(WorkerState::default());
        Self::spawn_chat_worker(
            registered.session,
            registered.generation,
            self.sessions.clone(),
            telegram,
            state.clone(),
            rx,
        );

        let sender = WorkerSender {
            generation: registered.generation,
            sender: tx,
            state,
        };
        senders.insert(chat_id, sender.clone());
        Ok(sender)
    }

    fn spawn_chat_worker(
        session: Arc<S>,
        generation: u64,
        sessions: Arc<Mutex<HashMap<ChatId, RegisteredSession<S>>>>,
        telegram: Arc<T>,
        state: Arc<WorkerState>,
        mut rx: mpsc::Receiver<GroupWorkItem>,
    ) {
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                let GroupWorkItem { chat_id, prompt } = item;
                if Self::worker_is_stale(&sessions, chat_id, generation).await {
                    break;
                }

                state.begin_item();
                let result = session.send(CodexRequest { prompt }).await;

                if Self::worker_is_stale(&sessions, chat_id, generation).await {
                    state.finish_item();
                    break;
                }

                match result {
                    Ok(turn) => {
                        if let Err(err) = telegram.send_message(chat_id, &turn.output).await {
                            tracing::warn!(
                                error = %err,
                                chat_id = ?chat_id,
                                "failed to send Codex response to Telegram"
                            );
                        }
                    }
                    Err(err) => {
                        let text = format!("Codex session failed: {err}");
                        if let Err(send_err) = telegram.send_message(chat_id, &text).await {
                            tracing::warn!(
                                error = %send_err,
                                chat_id = ?chat_id,
                                "failed to send Codex failure to Telegram"
                            );
                        }
                    }
                }
                state.finish_item();
            }
        });
    }

    async fn worker_is_stale(
        sessions: &Arc<Mutex<HashMap<ChatId, RegisteredSession<S>>>>,
        chat_id: ChatId,
        generation: u64,
    ) -> bool {
        sessions
            .lock()
            .await
            .get(&chat_id)
            .is_none_or(|session| session.generation != generation)
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

    struct SlowSession {
        reply_prefix: &'static str,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    impl SlowSession {
        fn new(reply_prefix: &'static str) -> Self {
            Self {
                reply_prefix,
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            }
        }
    }

    #[async_trait]
    impl CodexSession for SlowSession {
        async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
            self.entered.notify_one();
            self.release.notified().await;

            Ok(CodexTurn {
                output: format!("{}: {}", self.reply_prefix, request.prompt),
            })
        }

        async fn restart(&self) -> Result<(), CodexSessionError> {
            Ok(())
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
        router.register_session(ChatId(1), 1, session).await;

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
        router.register_session(ChatId(1), 1, session.clone()).await;

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
    async fn enqueue_should_report_queue_position_when_busy() {
        let (telegram, _messages) = fake_telegram();
        let session = Arc::new(SlowSession::new("reply"));
        let router = Router::new(4, telegram);
        router.register_session(ChatId(1), 1, session.clone()).await;

        let first_entered = session.entered.notified();
        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "one".to_owned(),
            })
            .await
            .expect("first enqueue should work");
        first_entered.await;

        let receipt = router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "two".to_owned(),
            })
            .await
            .expect("second enqueue should work");

        assert_eq!(receipt.queue_position, 1);
    }

    #[tokio::test]
    async fn register_session_should_route_future_work_to_replacement_session() {
        let (telegram, mut messages) = fake_telegram();
        let first_session = Arc::new(FakeSession::with_reply_prefix("first"));
        let second_session = Arc::new(FakeSession::with_reply_prefix("second"));
        let router = Router::new(4, telegram);
        router.register_session(ChatId(1), 1, first_session).await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "before".to_owned(),
            })
            .await
            .expect("first enqueue should work");
        let first_message = receive_message(&mut messages).await;
        assert_eq!(first_message, "first: before");

        router.register_session(ChatId(1), 2, second_session).await;
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
    async fn register_session_should_not_send_stale_generation_after_replacement() {
        let (telegram, mut messages) = fake_telegram();
        let first_session = Arc::new(SlowSession::new("first"));
        let second_session = Arc::new(SlowSession::new("second"));
        let router = Router::new(4, telegram);
        router
            .register_session(ChatId(1), 1, first_session.clone())
            .await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "stale".to_owned(),
            })
            .await
            .expect("first enqueue should work");
        first_session.entered.notified().await;

        router
            .register_session(ChatId(1), 2, second_session.clone())
            .await;
        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "fresh".to_owned(),
            })
            .await
            .expect("second enqueue should work");
        second_session.entered.notified().await;
        first_session.release.notify_one();
        second_session.release.notify_one();

        let message = receive_message(&mut messages).await;
        assert_eq!(message, "second: fresh");
        assert!(
            timeout(Duration::from_millis(50), messages.recv())
                .await
                .is_err(),
            "stale generation should not publish a Telegram response"
        );
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
