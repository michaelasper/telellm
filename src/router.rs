use crate::{
    bot::telegram::{TelegramError, TelegramMessageHandle, TelegramSendOptions, TelegramSink},
    codex::session::{
        CodexRequest, CodexSession, CodexSessionError, CodexTurn, CodexTurnEvent, GeneratedFile,
    },
    config::{TelegramStreamingMode, TelegramUxConfig},
    ids::{ChatId, MessageId},
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct GroupWorkItem {
    pub chat_id: ChatId,
    pub prompt: String,
    pub audio_reply: Option<crate::audio::AudioReplyRequest>,
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
    telegram_ux: TelegramUxConfig,
    audio_replies: Option<Arc<dyn crate::audio::AudioReplySynthesizer>>,
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
    sender: mpsc::Sender<QueuedWorkItem>,
    state: Arc<WorkerState>,
}

struct QueuedWorkItem {
    item: GroupWorkItem,
    queued_typing: Option<TypingHandle>,
}

struct ChatWorker<S, T>
where
    S: CodexSession + 'static,
    T: TelegramSink + 'static,
{
    session: Arc<S>,
    generation: u64,
    sessions: Arc<Mutex<HashMap<ChatId, RegisteredSession<S>>>>,
    telegram: Arc<T>,
    telegram_ux: TelegramUxConfig,
    audio_replies: Option<Arc<dyn crate::audio::AudioReplySynthesizer>>,
    state: Arc<WorkerState>,
}

struct TypingHandle {
    stop: oneshot::Sender<()>,
}

impl TypingHandle {
    fn stop(self) {
        let _ = self.stop.send(());
    }
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
        Self::new_with_telegram_ux(queue_depth, telegram, TelegramUxConfig::default())
    }

    pub fn new_with_telegram_ux(
        queue_depth: usize,
        telegram: Arc<T>,
        telegram_ux: TelegramUxConfig,
    ) -> Self {
        Self {
            queue_depth: queue_depth.max(1),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            telegram,
            telegram_ux,
            audio_replies: None,
        }
    }

    pub fn new_with_audio_replies(
        queue_depth: usize,
        telegram: Arc<T>,
        telegram_ux: TelegramUxConfig,
        audio_replies: Arc<dyn crate::audio::AudioReplySynthesizer>,
    ) -> Self {
        Self {
            queue_depth: queue_depth.max(1),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            telegram,
            telegram_ux,
            audio_replies: Some(audio_replies),
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

    pub async fn mark_session_stale(&self, chat_id: ChatId) {
        self.sessions.lock().await.remove(&chat_id);
        self.senders.lock().await.remove(&chat_id);
    }

    pub async fn enqueue(&self, item: GroupWorkItem) -> Result<EnqueueReceipt, RouterError> {
        let chat_id = item.chat_id;
        let sender = self.sender_for_chat(item.chat_id).await?;
        let queue_position = sender.queue_position(self.queue_depth);
        let queued_typing = (self.telegram_ux.typing_indicator_enabled
            && self.telegram_ux.typing_for_queued_items
            && queue_position > 0)
            .then(|| {
                Self::spawn_typing_loop(
                    self.telegram.clone(),
                    chat_id,
                    Duration::from_secs(self.telegram_ux.typing_refresh_secs),
                )
            });
        sender
            .sender
            .send(QueuedWorkItem {
                item,
                queued_typing,
            })
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
        let telegram_ux = self.telegram_ux.clone();
        let audio_replies = self.audio_replies.clone();
        let (tx, rx) = mpsc::channel(self.queue_depth);
        let state = Arc::new(WorkerState::default());
        Self::spawn_chat_worker(
            ChatWorker {
                session: registered.session,
                generation: registered.generation,
                sessions: self.sessions.clone(),
                telegram,
                telegram_ux,
                audio_replies,
                state: state.clone(),
            },
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

    fn spawn_chat_worker(worker: ChatWorker<S, T>, mut rx: mpsc::Receiver<QueuedWorkItem>) {
        let ChatWorker {
            session,
            generation,
            sessions,
            telegram,
            telegram_ux,
            audio_replies,
            state,
        } = worker;
        tokio::spawn(async move {
            while let Some(queued_item) = rx.recv().await {
                if let Some(typing) = queued_item.queued_typing {
                    typing.stop();
                }
                let item = queued_item.item;
                let GroupWorkItem {
                    chat_id,
                    prompt,
                    audio_reply,
                } = item;
                if Self::worker_is_stale(&sessions, chat_id, generation).await {
                    break;
                }

                state.begin_item();
                let active_typing = telegram_ux.typing_indicator_enabled.then(|| {
                    Self::spawn_typing_loop(
                        telegram.clone(),
                        chat_id,
                        Duration::from_secs(telegram_ux.typing_refresh_secs),
                    )
                });
                let (result, streamed_message) = Self::run_session_with_streaming(
                    session.clone(),
                    telegram.clone(),
                    chat_id,
                    prompt,
                    telegram_ux.clone(),
                    sessions.clone(),
                    generation,
                )
                .await;
                if let Some(typing) = active_typing {
                    typing.stop();
                }

                if Self::worker_is_stale(&sessions, chat_id, generation).await {
                    if let Ok(turn) = result {
                        Self::cleanup_generated_files(turn.generated_files).await;
                    }
                    state.finish_item();
                    break;
                }

                match result {
                    Ok(turn) => {
                        let send_result = if let Some(message_id) = streamed_message {
                            telegram
                                .finish_streamed_message(
                                    chat_id,
                                    message_id,
                                    &turn.output,
                                    telegram_send_options(&telegram_ux),
                                )
                                .await
                        } else {
                            telegram
                                .send_message_with_options(
                                    chat_id,
                                    &turn.output,
                                    telegram_send_options(&telegram_ux),
                                )
                                .await
                                .map(|_| ())
                        };
                        let text_sent = send_result.is_ok();
                        if let Err(err) = send_result {
                            tracing::warn!(
                                error = %err,
                                chat_id = ?chat_id,
                                "failed to send Codex response to Telegram"
                            );
                        }
                        Self::send_generated_files(&telegram, chat_id, turn.generated_files).await;
                        if text_sent {
                            Self::send_audio_reply(
                                &telegram,
                                &sessions,
                                generation,
                                audio_replies.clone(),
                                chat_id,
                                audio_reply,
                                &turn.output,
                            )
                            .await;
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

    async fn run_session_with_streaming(
        session: Arc<S>,
        telegram: Arc<T>,
        chat_id: ChatId,
        prompt: String,
        telegram_ux: TelegramUxConfig,
        sessions: Arc<Mutex<HashMap<ChatId, RegisteredSession<S>>>>,
        generation: u64,
    ) -> (Result<CodexTurn, CodexSessionError>, Option<MessageId>) {
        if !telegram_ux.streaming_enabled {
            let result = session.send(CodexRequest { prompt }).await;
            return (result, None);
        }

        match telegram_ux.streaming_mode {
            TelegramStreamingMode::EditMessage => {}
        }

        let (event_tx, mut event_rx) = mpsc::channel(16);
        let request = CodexRequest { prompt };
        let mut session_task =
            tokio::spawn(async move { session.send_with_events(request, Some(event_tx)).await });
        let mut stream = StreamingState::new(&telegram_ux);
        let mut events_closed = false;

        loop {
            tokio::select! {
                event = event_rx.recv(), if !events_closed => {
                    match event {
                        Some(CodexTurnEvent::OutputSnapshot { output, is_final }) => {
                            Self::handle_streaming_event(
                                &mut stream,
                                &telegram,
                                &sessions,
                                chat_id,
                                generation,
                                &output,
                                is_final,
                            )
                            .await;
                        }
                        None => {
                            events_closed = true;
                        }
                    }
                }
                result = &mut session_task => {
                    let result = match result {
                        Ok(result) => result,
                        Err(err) => Err(CodexSessionError::Process(err.to_string())),
                    };
                    while let Ok(CodexTurnEvent::OutputSnapshot { output, is_final }) =
                        event_rx.try_recv()
                    {
                        Self::handle_streaming_event(
                            &mut stream,
                            &telegram,
                            &sessions,
                            chat_id,
                            generation,
                            &output,
                            is_final,
                        )
                        .await;
                    }
                    return (result, stream.message_id());
                }
            }
        }
    }

    async fn handle_streaming_event(
        stream: &mut StreamingState,
        telegram: &Arc<T>,
        sessions: &Arc<Mutex<HashMap<ChatId, RegisteredSession<S>>>>,
        chat_id: ChatId,
        generation: u64,
        output: &str,
        is_final: bool,
    ) {
        if is_final && stream.message_id().is_some() {
            return;
        }
        if Self::worker_is_stale(sessions, chat_id, generation).await {
            return;
        }
        stream.update(telegram, chat_id, output).await;
    }

    fn spawn_typing_loop(telegram: Arc<T>, chat_id: ChatId, refresh: Duration) -> TypingHandle {
        let (stop_tx, mut stop_rx) = oneshot::channel();
        tokio::spawn(async move {
            loop {
                match stop_rx.try_recv() {
                    Ok(()) | Err(oneshot::error::TryRecvError::Closed) => break,
                    Err(oneshot::error::TryRecvError::Empty) => {}
                }

                if let Err(err) = telegram.send_typing_action(chat_id).await {
                    tracing::debug!(
                        error = %err,
                        chat_id = ?chat_id,
                        "failed to send Telegram typing action"
                    );
                }

                tokio::select! {
                    _ = tokio::time::sleep(refresh) => {}
                    _ = &mut stop_rx => break,
                }
            }
        });
        TypingHandle { stop: stop_tx }
    }

    async fn send_generated_files(telegram: &Arc<T>, chat_id: ChatId, files: Vec<GeneratedFile>) {
        for file in files {
            let caption = format!("Generated file: @{}", file.workspace_path);
            if let Err(err) = telegram
                .send_document(chat_id, &file.host_path, &file.file_name, Some(&caption))
                .await
            {
                tracing::warn!(
                    error = %err,
                    chat_id = ?chat_id,
                    workspace_path = %file.workspace_path,
                    bytes = file.bytes,
                    "failed to send generated file to Telegram"
                );
            }
            if let Err(err) = tokio::fs::remove_file(&file.host_path).await {
                log_generated_file_cleanup_error(&file, err);
            }
        }
    }

    async fn send_audio_reply(
        telegram: &Arc<T>,
        sessions: &Arc<Mutex<HashMap<ChatId, RegisteredSession<S>>>>,
        generation: u64,
        audio_replies: Option<Arc<dyn crate::audio::AudioReplySynthesizer>>,
        chat_id: ChatId,
        audio_reply: Option<crate::audio::AudioReplyRequest>,
        output: &str,
    ) {
        let (Some(audio_replies), Some(request)) = (audio_replies, audio_reply) else {
            return;
        };
        if Self::worker_is_stale(sessions, chat_id, generation).await {
            return;
        }

        let mut request = request.clone();
        request.text = output.to_owned();
        let audio = match audio_replies.synthesize_reply(request).await {
            Ok(Some(audio)) => audio,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    chat_id = ?chat_id,
                    "failed to synthesize spoken Telegram reply"
                );
                return;
            }
        };
        if Self::worker_is_stale(sessions, chat_id, generation).await {
            remove_synthesized_audio_file(&audio).await;
            return;
        }

        if let Err(err) = telegram
            .send_audio_file(
                chat_id,
                &audio.host_path,
                &audio.file_name,
                audio.send_as.telegram_kind(),
            )
            .await
        {
            tracing::warn!(
                error = %err,
                chat_id = ?chat_id,
                path = %audio.host_path.display(),
                "failed to send spoken Telegram reply"
            );
        }
        if let Err(err) = tokio::fs::remove_file(&audio.host_path).await {
            tracing::debug!(
                error = %err,
                path = %audio.host_path.display(),
                "failed to remove temporary spoken reply file"
            );
        }
    }

    async fn cleanup_generated_files(files: Vec<GeneratedFile>) {
        for file in files {
            if let Err(err) = tokio::fs::remove_file(&file.host_path).await {
                log_generated_file_cleanup_error(&file, err);
            }
        }
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

struct StreamingState {
    message_id: Option<MessageId>,
    last_sent_at: Option<Instant>,
    last_sent_chars: usize,
    update_interval: Duration,
    min_delta_chars: usize,
    max_chars: usize,
    options: TelegramSendOptions,
}

impl StreamingState {
    fn new(telegram_ux: &TelegramUxConfig) -> Self {
        Self {
            message_id: None,
            last_sent_at: None,
            last_sent_chars: 0,
            update_interval: Duration::from_millis(telegram_ux.streaming_update_interval_millis),
            min_delta_chars: telegram_ux.streaming_min_delta_chars,
            max_chars: telegram_ux.streaming_max_chars,
            options: telegram_send_options(telegram_ux),
        }
    }

    fn message_id(&self) -> Option<MessageId> {
        self.message_id
    }

    async fn update<T>(&mut self, telegram: &Arc<T>, chat_id: ChatId, output: &str)
    where
        T: TelegramSink + 'static,
    {
        let preview = streaming_preview(output, self.max_chars);
        if !self.should_update(&preview) {
            return;
        }

        let result = if let Some(message_id) = self.message_id {
            telegram
                .edit_message_with_options(chat_id, message_id, &preview, self.options)
                .await
                .map(|_| None)
        } else {
            telegram
                .send_message_with_options(chat_id, &preview, self.options)
                .await
                .map(first_message_id)
        };

        match result {
            Ok(Some(message_id)) => {
                self.message_id = Some(message_id);
                self.record_update(&preview);
            }
            Ok(None) => {
                self.record_update(&preview);
            }
            Err(err) => {
                tracing::debug!(
                    error = %err,
                    chat_id = ?chat_id,
                    "failed to stream Telegram Codex snapshot"
                );
            }
        }
    }

    fn should_update(&self, preview: &str) -> bool {
        if preview.trim().is_empty() {
            return false;
        }

        let chars = preview.chars().count();
        if self.message_id.is_none() {
            return chars >= self.min_delta_chars;
        }

        if chars.saturating_sub(self.last_sent_chars) < self.min_delta_chars {
            return false;
        }

        self.last_sent_at
            .is_none_or(|sent_at| sent_at.elapsed() >= self.update_interval)
    }

    fn record_update(&mut self, preview: &str) {
        self.last_sent_at = Some(Instant::now());
        self.last_sent_chars = preview.chars().count();
    }
}

fn first_message_id(handles: Vec<TelegramMessageHandle>) -> Option<MessageId> {
    handles.first().map(|handle| handle.message_id)
}

fn telegram_send_options(telegram_ux: &TelegramUxConfig) -> TelegramSendOptions {
    TelegramSendOptions {
        formatting_mode: telegram_ux.formatting_mode,
        formatting_escape: telegram_ux.formatting_escape,
        formatting_fallback_to_plain: telegram_ux.formatting_fallback_to_plain,
    }
}

fn log_generated_file_cleanup_error(file: &GeneratedFile, err: std::io::Error) {
    tracing::debug!(
        error = %err,
        path = %file.host_path.display(),
        workspace_path = %file.workspace_path,
        "failed to remove temporary generated file"
    );
}

async fn remove_synthesized_audio_file(audio: &crate::audio::SynthesizedAudio) {
    if let Err(err) = tokio::fs::remove_file(&audio.host_path).await {
        tracing::debug!(
            error = %err,
            path = %audio.host_path.display(),
            "failed to remove temporary spoken reply file"
        );
    }
}

fn streaming_preview(output: &str, max_chars: usize) -> String {
    let mut preview = String::new();
    let keep_chars = max_chars.saturating_sub(4);
    for (index, ch) in output.chars().enumerate() {
        if index >= keep_chars {
            preview.push_str("\n...");
            return preview;
        }
        preview.push(ch);
    }
    preview
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
    use crate::codex::session::{CodexEventSender, CodexTurn};
    use crate::{
        audio::{AudioError, AudioReplyRequest, AudioReplySynthesizer, SynthesizedAudio},
        config::AudioSendAs,
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
    use tokio::{
        sync::{Mutex, mpsc},
        time::{Duration, timeout},
    };

    type FakeTelegramWithAudio = (
        Arc<FakeTelegram>,
        mpsc::UnboundedReceiver<String>,
        mpsc::UnboundedReceiver<ChatId>,
        mpsc::UnboundedReceiver<String>,
    );
    type FakeTelegramWithDocumentsAndAudio = (
        Arc<FakeTelegram>,
        mpsc::UnboundedReceiver<String>,
        mpsc::UnboundedReceiver<ChatId>,
        mpsc::UnboundedReceiver<String>,
        mpsc::UnboundedReceiver<String>,
    );

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

    struct StreamingSession {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    impl StreamingSession {
        fn new() -> Self {
            Self {
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            }
        }
    }

    struct CompletionSnapshotSession;

    #[async_trait]
    impl CodexSession for StreamingSession {
        async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
            self.send_with_events(request, None).await
        }

        async fn send_with_events(
            &self,
            _request: CodexRequest,
            events: Option<CodexEventSender>,
        ) -> Result<CodexTurn, CodexSessionError> {
            if let Some(events) = events {
                let _ = events.try_send(CodexTurnEvent::OutputSnapshot {
                    output: "partial answer from codex".to_owned(),
                    is_final: false,
                });
            }
            self.entered.notify_one();
            self.release.notified().await;

            Ok(CodexTurn::text("final answer from codex"))
        }

        async fn restart(&self) -> Result<(), CodexSessionError> {
            Ok(())
        }
    }

    #[async_trait]
    impl CodexSession for CompletionSnapshotSession {
        async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
            self.send_with_events(request, None).await
        }

        async fn send_with_events(
            &self,
            request: CodexRequest,
            events: Option<CodexEventSender>,
        ) -> Result<CodexTurn, CodexSessionError> {
            let output = format!("snapshot: {}", request.prompt);
            if let Some(events) = events {
                let _ = events.try_send(CodexTurnEvent::OutputSnapshot {
                    output: output.clone(),
                    is_final: false,
                });
            }
            Ok(CodexTurn::text(output))
        }

        async fn restart(&self) -> Result<(), CodexSessionError> {
            Ok(())
        }
    }

    #[async_trait]
    impl CodexSession for SlowSession {
        async fn send(&self, request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
            self.entered.notify_one();
            self.release.notified().await;

            Ok(CodexTurn::text(format!(
                "{}: {}",
                self.reply_prefix, request.prompt
            )))
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

            Ok(CodexTurn::text(format!(
                "{}: {}",
                self.reply_prefix, request.prompt
            )))
        }

        async fn restart(&self) -> Result<(), CodexSessionError> {
            Ok(())
        }
    }

    struct FileSession {
        host_path: std::path::PathBuf,
    }

    #[async_trait]
    impl CodexSession for FileSession {
        async fn send(&self, _request: CodexRequest) -> Result<CodexTurn, CodexSessionError> {
            Ok(CodexTurn {
                output: "Created @telegram_outputs/report.pdf".to_owned(),
                generated_files: vec![GeneratedFile {
                    workspace_path: "telegram_outputs/report.pdf".to_owned(),
                    host_path: self.host_path.clone(),
                    file_name: "report.pdf".to_owned(),
                    bytes: 4,
                }],
            })
        }

        async fn restart(&self) -> Result<(), CodexSessionError> {
            Ok(())
        }
    }

    struct FakeTelegram {
        messages: mpsc::UnboundedSender<String>,
        typing_actions: mpsc::UnboundedSender<ChatId>,
        documents: mpsc::UnboundedSender<String>,
        audio_files: mpsc::UnboundedSender<String>,
        next_message_id: AtomicI32,
    }

    struct FakeAudioReplySynthesizer {
        prefix: String,
    }

    impl FakeAudioReplySynthesizer {
        fn new(prefix: impl Into<String>) -> Self {
            Self {
                prefix: prefix.into(),
            }
        }
    }

    #[async_trait]
    impl AudioReplySynthesizer for FakeAudioReplySynthesizer {
        async fn synthesize_reply(
            &self,
            request: AudioReplyRequest,
        ) -> Result<Option<SynthesizedAudio>, AudioError> {
            let file = tempfile::NamedTempFile::new().expect("temp audio file should be created");
            let path = file.path().to_path_buf();
            let bytes = format!("{}:{}", self.prefix, request.text);
            tokio::fs::write(&path, bytes.as_bytes())
                .await
                .expect("temp audio file should be writable");
            let path = file
                .into_temp_path()
                .keep()
                .expect("temp audio file should persist for Telegram send");
            Ok(Some(SynthesizedAudio {
                host_path: path,
                file_name: "reply.ogg".to_owned(),
                send_as: AudioSendAs::Voice,
                bytes: bytes.len() as u64,
            }))
        }
    }

    struct FailingAudioReplySynthesizer;

    #[async_trait]
    impl AudioReplySynthesizer for FailingAudioReplySynthesizer {
        async fn synthesize_reply(
            &self,
            _request: AudioReplyRequest,
        ) -> Result<Option<SynthesizedAudio>, AudioError> {
            Err(AudioError::EmptyOutput)
        }
    }

    struct BlockingAudioReplySynthesizer {
        entered: mpsc::UnboundedSender<std::path::PathBuf>,
        release: tokio::sync::Notify,
    }

    impl BlockingAudioReplySynthesizer {
        fn new() -> (Self, mpsc::UnboundedReceiver<std::path::PathBuf>) {
            let (entered_tx, entered_rx) = mpsc::unbounded_channel();
            (
                Self {
                    entered: entered_tx,
                    release: tokio::sync::Notify::new(),
                },
                entered_rx,
            )
        }

        fn release(&self) {
            self.release.notify_waiters();
        }
    }

    #[async_trait]
    impl AudioReplySynthesizer for BlockingAudioReplySynthesizer {
        async fn synthesize_reply(
            &self,
            request: AudioReplyRequest,
        ) -> Result<Option<SynthesizedAudio>, AudioError> {
            let file = tempfile::NamedTempFile::new().expect("temp audio file should be created");
            let path = file.path().to_path_buf();
            tokio::fs::write(&path, request.text.as_bytes())
                .await
                .expect("temp audio file should be writable");
            let path = file
                .into_temp_path()
                .keep()
                .expect("temp audio file should persist for Telegram send");
            self.entered
                .send(path.clone())
                .expect("test should receive synthesize start");
            self.release.notified().await;

            Ok(Some(SynthesizedAudio {
                host_path: path,
                file_name: "reply.ogg".to_owned(),
                send_as: AudioSendAs::Voice,
                bytes: request.text.len() as u64,
            }))
        }
    }

    #[async_trait]
    impl TelegramSink for FakeTelegram {
        async fn send_message(&self, _chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
            self.messages
                .send(text.to_owned())
                .map_err(|err| TelegramError::Send(err.to_string()))
        }

        async fn send_message_with_options(
            &self,
            chat_id: ChatId,
            text: &str,
            _options: TelegramSendOptions,
        ) -> Result<Vec<TelegramMessageHandle>, TelegramError> {
            let message_id = MessageId(self.next_message_id.fetch_add(1, Ordering::SeqCst));
            self.messages
                .send(text.to_owned())
                .map_err(|err| TelegramError::Send(err.to_string()))?;
            Ok(vec![TelegramMessageHandle {
                chat_id,
                message_id,
            }])
        }

        async fn edit_message_with_options(
            &self,
            _chat_id: ChatId,
            message_id: MessageId,
            text: &str,
            _options: TelegramSendOptions,
        ) -> Result<(), TelegramError> {
            self.messages
                .send(format!("edit:{}:{text}", message_id.0))
                .map_err(|err| TelegramError::Edit(err.to_string()))
        }

        async fn send_typing_action(&self, chat_id: ChatId) -> Result<(), TelegramError> {
            self.typing_actions
                .send(chat_id)
                .map_err(|err| TelegramError::ChatAction(err.to_string()))
        }

        async fn send_document(
            &self,
            _chat_id: ChatId,
            path: &std::path::Path,
            file_name: &str,
            caption: Option<&str>,
        ) -> Result<TelegramMessageHandle, TelegramError> {
            let text = format!(
                "{}:{}:{}",
                file_name,
                path.exists(),
                caption.unwrap_or_default()
            );
            self.documents
                .send(text)
                .map_err(|err| TelegramError::DocumentSend(err.to_string()))?;
            Ok(TelegramMessageHandle {
                chat_id: ChatId(1),
                message_id: MessageId(self.next_message_id.fetch_add(1, Ordering::SeqCst)),
            })
        }

        async fn send_audio_file(
            &self,
            _chat_id: ChatId,
            path: &std::path::Path,
            file_name: &str,
            kind: crate::bot::telegram::TelegramAudioSendKind,
        ) -> Result<TelegramMessageHandle, TelegramError> {
            let kind = match kind {
                crate::bot::telegram::TelegramAudioSendKind::Voice => "voice",
                crate::bot::telegram::TelegramAudioSendKind::Audio => "audio",
            };
            let contents = tokio::fs::read_to_string(path).await.unwrap_or_default();
            let text = format!("{kind}:{file_name}:{}:{contents}", path.exists());
            self.audio_files
                .send(text)
                .map_err(|err| TelegramError::AudioSend(err.to_string()))?;
            Ok(TelegramMessageHandle {
                chat_id: ChatId(1),
                message_id: MessageId(self.next_message_id.fetch_add(1, Ordering::SeqCst)),
            })
        }
    }

    impl FakeTelegram {
        fn new() -> FakeTelegramWithAudio {
            let (telegram, messages, typing_actions, _documents, audio_files) =
                Self::new_with_documents();
            (telegram, messages, typing_actions, audio_files)
        }

        fn new_with_documents() -> FakeTelegramWithDocumentsAndAudio {
            let (tx, rx) = mpsc::unbounded_channel();
            let (typing_tx, typing_rx) = mpsc::unbounded_channel();
            let (document_tx, document_rx) = mpsc::unbounded_channel();
            let (audio_tx, audio_rx) = mpsc::unbounded_channel();
            (
                Arc::new(FakeTelegram {
                    messages: tx,
                    typing_actions: typing_tx,
                    documents: document_tx,
                    audio_files: audio_tx,
                    next_message_id: AtomicI32::new(1),
                }),
                rx,
                typing_rx,
                document_rx,
                audio_rx,
            )
        }
    }

    fn fake_telegram() -> (Arc<FakeTelegram>, mpsc::UnboundedReceiver<String>) {
        let (telegram, messages, _typing_actions, _audio_files) = FakeTelegram::new();
        (telegram, messages)
    }

    fn fake_telegram_with_actions() -> (
        Arc<FakeTelegram>,
        mpsc::UnboundedReceiver<String>,
        mpsc::UnboundedReceiver<ChatId>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let (telegram, messages, typing_actions, documents, _audio_files) =
            FakeTelegram::new_with_documents();
        (telegram, messages, typing_actions, documents)
    }

    async fn receive_message(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
        timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("telegram message should arrive before timeout")
            .expect("telegram sender should stay open")
    }

    async fn receive_typing_action(rx: &mut mpsc::UnboundedReceiver<ChatId>) -> ChatId {
        timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("typing action should arrive before timeout")
            .expect("typing sender should stay open")
    }

    async fn receive_document(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
        timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("document should arrive before timeout")
            .expect("telegram sender should stay open")
    }

    async fn receive_audio_file(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
        timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("audio file should arrive before timeout")
            .expect("telegram sender should stay open")
    }

    async fn wait_for_removed(path: &std::path::Path) {
        timeout(Duration::from_secs(1), async {
            while path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("temporary exported file should be removed after send attempt");
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
                audio_reply: None,
            })
            .await
            .expect("enqueue should work");

        let message = receive_message(&mut messages).await;
        assert_eq!(message, "reply: hello");
    }

    #[tokio::test]
    async fn enqueue_should_send_generated_files_after_text_response() {
        let (telegram, mut messages, _typing_actions, mut documents) = fake_telegram_with_actions();
        let temp = tempfile::NamedTempFile::new().expect("temp file should be created");
        std::fs::write(temp.path(), b"test").expect("temp file should be writable");
        let session = Arc::new(FileSession {
            host_path: temp.path().to_path_buf(),
        });
        let router = Router::new(4, telegram);
        router.register_session(ChatId(1), 1, session).await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "make file".to_owned(),
                audio_reply: None,
            })
            .await
            .expect("enqueue should work");

        assert_eq!(
            receive_message(&mut messages).await,
            "Created @telegram_outputs/report.pdf"
        );
        assert_eq!(
            receive_document(&mut documents).await,
            "report.pdf:true:Generated file: @telegram_outputs/report.pdf"
        );
        wait_for_removed(temp.path()).await;
    }

    #[tokio::test]
    async fn router_should_send_spoken_reply_after_text_for_audio_turn() {
        let (telegram, mut messages, _typing, mut audio_files) = FakeTelegram::new();
        let synthesizer = Arc::new(FakeAudioReplySynthesizer::new("reply audio"));
        let router =
            Router::new_with_audio_replies(4, telegram, TelegramUxConfig::default(), synthesizer);
        router
            .register_session(ChatId(1), 1, Arc::new(FakeSession::default()))
            .await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "hello".to_owned(),
                audio_reply: Some(AudioReplyRequest {
                    chat_id: ChatId(1),
                    trigger_message_id: MessageId(7),
                    text: String::new(),
                }),
            })
            .await
            .expect("enqueue should work");

        let text = receive_message(&mut messages).await;
        let audio = receive_audio_file(&mut audio_files).await;

        assert!(text.contains("reply: hello"));
        assert!(audio.contains("voice"));
        assert!(audio.contains("reply: hello"));
    }

    #[tokio::test]
    async fn router_should_drop_audio_reply_when_session_stales_during_synthesis() {
        let (telegram, mut messages, _typing, mut audio_files) = FakeTelegram::new();
        let (synthesizer, mut synthesized_paths) = BlockingAudioReplySynthesizer::new();
        let synthesizer = Arc::new(synthesizer);
        let router = Router::new_with_audio_replies(
            4,
            telegram,
            TelegramUxConfig::default(),
            synthesizer.clone(),
        );
        router
            .register_session(ChatId(1), 1, Arc::new(FakeSession::default()))
            .await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "hello".to_owned(),
                audio_reply: Some(AudioReplyRequest {
                    chat_id: ChatId(1),
                    trigger_message_id: MessageId(7),
                    text: String::new(),
                }),
            })
            .await
            .expect("enqueue should work");

        assert!(
            receive_message(&mut messages)
                .await
                .contains("reply: hello")
        );
        let synthesized_path = timeout(Duration::from_secs(1), synthesized_paths.recv())
            .await
            .expect("synthesis should start")
            .expect("synthesized path should be reported");
        router.mark_session_stale(ChatId(1)).await;
        synthesizer.release();
        wait_for_removed(&synthesized_path).await;

        assert!(matches!(
            audio_files.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn router_should_keep_text_when_audio_synthesis_fails() {
        let (telegram, mut messages, _typing, mut audio_files) = FakeTelegram::new();
        let synthesizer = Arc::new(FailingAudioReplySynthesizer);
        let router =
            Router::new_with_audio_replies(4, telegram, TelegramUxConfig::default(), synthesizer);
        router
            .register_session(ChatId(1), 1, Arc::new(FakeSession::default()))
            .await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "hello".to_owned(),
                audio_reply: Some(AudioReplyRequest {
                    chat_id: ChatId(1),
                    trigger_message_id: MessageId(7),
                    text: String::new(),
                }),
            })
            .await
            .expect("enqueue should work");

        assert!(
            receive_message(&mut messages)
                .await
                .contains("reply: hello")
        );
        assert!(matches!(
            audio_files.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
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
                    audio_reply: None,
                })
                .await
        });
        let second_router = router;
        let second = tokio::spawn(async move {
            second_router
                .enqueue(GroupWorkItem {
                    chat_id: ChatId(1),
                    prompt: "two".to_owned(),
                    audio_reply: None,
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
                audio_reply: None,
            })
            .await
            .expect("first enqueue should work");
        first_entered.await;

        let receipt = router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "two".to_owned(),
                audio_reply: None,
            })
            .await
            .expect("second enqueue should work");

        assert_eq!(receipt.queue_position, 1);
    }

    #[tokio::test]
    async fn enqueue_should_send_typing_action_while_turn_is_active() {
        let (telegram, _messages, mut typing_actions, _documents) = fake_telegram_with_actions();
        let session = Arc::new(SlowSession::new("reply"));
        let router = Router::new_with_telegram_ux(
            4,
            telegram,
            TelegramUxConfig {
                streaming_enabled: false,
                typing_refresh_secs: 1,
                ..TelegramUxConfig::default()
            },
        );
        router.register_session(ChatId(1), 1, session.clone()).await;

        let first_entered = session.entered.notified();
        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "one".to_owned(),
                audio_reply: None,
            })
            .await
            .expect("enqueue should work");
        first_entered.await;

        assert_eq!(receive_typing_action(&mut typing_actions).await, ChatId(1));
        session.release.notify_one();
    }

    #[tokio::test]
    async fn enqueue_should_stream_snapshot_then_edit_final_message() {
        let (telegram, mut messages) = fake_telegram();
        let session = Arc::new(StreamingSession::new());
        let router = Router::new_with_telegram_ux(
            4,
            telegram,
            TelegramUxConfig {
                typing_indicator_enabled: false,
                streaming_min_delta_chars: 1,
                streaming_update_interval_millis: 1,
                ..TelegramUxConfig::default()
            },
        );
        router.register_session(ChatId(1), 1, session.clone()).await;

        let first_entered = session.entered.notified();
        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "stream please".to_owned(),
                audio_reply: None,
            })
            .await
            .expect("enqueue should work");
        first_entered.await;

        assert_eq!(
            receive_message(&mut messages).await,
            "partial answer from codex"
        );
        session.release.notify_one();
        assert_eq!(
            receive_message(&mut messages).await,
            "edit:1:final answer from codex"
        );
    }

    #[tokio::test]
    async fn enqueue_should_stream_final_only_default_snapshot_then_edit_final_message() {
        let (telegram, mut messages) = fake_telegram();
        let session = Arc::new(FakeSession::default());
        let router = Router::new_with_telegram_ux(
            4,
            telegram,
            TelegramUxConfig {
                typing_indicator_enabled: false,
                streaming_min_delta_chars: 1,
                streaming_update_interval_millis: 1,
                ..TelegramUxConfig::default()
            },
        );
        router.register_session(ChatId(1), 1, session).await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "exec-style".to_owned(),
                audio_reply: None,
            })
            .await
            .expect("enqueue should work");

        assert_eq!(receive_message(&mut messages).await, "reply: exec-style");
        assert_eq!(
            receive_message(&mut messages).await,
            "edit:1:reply: exec-style"
        );
    }

    #[tokio::test]
    async fn enqueue_should_drain_snapshot_queued_at_session_completion() {
        let (telegram, mut messages) = fake_telegram();
        let session = Arc::new(CompletionSnapshotSession);
        let router = Router::new_with_telegram_ux(
            4,
            telegram,
            TelegramUxConfig {
                typing_indicator_enabled: false,
                streaming_min_delta_chars: 1,
                streaming_update_interval_millis: 1,
                ..TelegramUxConfig::default()
            },
        );
        router.register_session(ChatId(1), 1, session).await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "complete-now".to_owned(),
                audio_reply: None,
            })
            .await
            .expect("enqueue should work");

        assert_eq!(
            receive_message(&mut messages).await,
            "snapshot: complete-now"
        );
        assert_eq!(
            receive_message(&mut messages).await,
            "edit:1:snapshot: complete-now"
        );
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
                audio_reply: None,
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
                audio_reply: None,
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
                audio_reply: None,
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
                audio_reply: None,
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
    async fn mark_session_stale_should_suppress_in_flight_response_before_replacement() {
        let (telegram, mut messages) = fake_telegram();
        let first_session = Arc::new(SlowSession::new("first"));
        let router = Router::new(4, telegram);
        router
            .register_session(ChatId(1), 1, first_session.clone())
            .await;

        router
            .enqueue(GroupWorkItem {
                chat_id: ChatId(1),
                prompt: "stale".to_owned(),
                audio_reply: None,
            })
            .await
            .expect("first enqueue should work");
        first_session.entered.notified().await;

        router.mark_session_stale(ChatId(1)).await;
        first_session.release.notify_one();

        assert!(
            timeout(Duration::from_millis(50), messages.recv())
                .await
                .is_err(),
            "stale session should not publish after lifecycle replacement starts"
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
                audio_reply: None,
            })
            .await
            .expect_err("enqueue should fail without a registered session");

        assert_eq!(
            err.to_string(),
            "no Codex session registered for chat ChatId(404)"
        );
    }
}
