use crate::ids::ChatId;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
    routing::post,
};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use std::{
    collections::HashMap,
    fmt,
    net::SocketAddr,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

const CHAT_ID_HEADER: &str = "x-telellm-chat-id";
const GENERATED_TOKEN_BYTES: usize = 32;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct BrokerToken(String);

impl BrokerToken {
    pub fn generate() -> Self {
        let bytes: [u8; GENERATED_TOKEN_BYTES] = rand::random();
        Self(hex_token(&bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for BrokerToken {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Display for BrokerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for BrokerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BrokerToken(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BrokerLimits {
    pub max_requests_per_window: usize,
    pub request_window: Duration,
    pub max_concurrent_requests: usize,
}

impl Default for BrokerLimits {
    fn default() -> Self {
        Self {
            max_requests_per_window: 120,
            request_window: Duration::from_secs(60),
            max_concurrent_requests: 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrokerConfig {
    pub listen: SocketAddr,
    pub upstream_base_url: String,
    pub upstream_api_key: SecretString,
}

#[derive(Clone)]
pub struct BrokerState {
    client: Client,
    upstream_base_url: String,
    upstream_api_key: SecretString,
    token_registry: BrokerTokenRegistry,
    limits: BrokerLimits,
}

impl BrokerState {
    pub fn new(config: BrokerConfig) -> Self {
        Self::new_with_limits(config, BrokerLimits::default())
    }

    pub fn new_with_limits(config: BrokerConfig, limits: BrokerLimits) -> Self {
        Self::new_with_registry(config, limits, BrokerTokenRegistry::global())
    }

    fn new_with_registry(
        config: BrokerConfig,
        limits: BrokerLimits,
        token_registry: BrokerTokenRegistry,
    ) -> Self {
        Self {
            client: Client::new(),
            upstream_base_url: config.upstream_base_url,
            upstream_api_key: config.upstream_api_key,
            token_registry,
            limits,
        }
    }

    pub async fn register_token(&self, chat_id: ChatId, token: BrokerToken) {
        self.token_registry.register_token(chat_id, token).await;
    }

    pub async fn rotate_token(&self, chat_id: ChatId, token: BrokerToken) {
        self.token_registry.rotate_token(chat_id, token).await;
    }

    #[cfg(test)]
    fn new_for_test(config: BrokerConfig) -> Self {
        Self::new_with_limits_for_test(config, BrokerLimits::default())
    }

    #[cfg(test)]
    fn new_with_limits_for_test(config: BrokerConfig, limits: BrokerLimits) -> Self {
        Self::new_with_registry(config, limits, BrokerTokenRegistry::isolated())
    }
}

#[derive(Clone)]
pub struct BrokerTokenRegistry {
    inner: Arc<BrokerTokenRegistryInner>,
}

impl BrokerTokenRegistry {
    pub fn global() -> Self {
        Self {
            inner: GLOBAL_TOKEN_REGISTRY.clone(),
        }
    }

    #[cfg(test)]
    fn isolated() -> Self {
        Self {
            inner: Arc::new(BrokerTokenRegistryInner::default()),
        }
    }

    pub async fn register_token(&self, chat_id: ChatId, token: BrokerToken) {
        self.replace_chat_token(chat_id, token).await;
    }

    pub async fn rotate_token(&self, chat_id: ChatId, token: BrokerToken) {
        self.replace_chat_token(chat_id, token).await;
    }

    pub async fn unregister_chat(&self, chat_id: ChatId) {
        let removed = {
            let mut active_tokens = self.inner.active_tokens.write().await;
            remove_chat_tokens(&mut active_tokens, chat_id)
        };
        self.remove_limit_states(&removed);
    }

    async fn replace_chat_token(&self, chat_id: ChatId, token: BrokerToken) {
        let removed = {
            let mut active_tokens = self.inner.active_tokens.write().await;
            let removed = remove_chat_tokens(&mut active_tokens, chat_id);
            active_tokens.insert(token.as_str().to_owned(), chat_id);
            removed
        };
        self.remove_limit_states(&removed);
    }

    async fn owner_for_token(&self, token: &str) -> Option<ChatId> {
        self.inner.active_tokens.read().await.get(token).copied()
    }

    fn remove_limit_states(&self, tokens: &[String]) {
        if tokens.is_empty() {
            return;
        }

        if let Ok(mut request_limits) = self.inner.request_limits.lock() {
            for token in tokens {
                request_limits.remove(token);
            }
        }
    }
}

impl Default for BrokerTokenRegistry {
    fn default() -> Self {
        Self::global()
    }
}

pub fn router(state: BrokerState) -> Router {
    Router::new()
        .route("/v1/responses", post(proxy_responses))
        .route("/v1/chat/completions", post(proxy_chat_completions))
        .with_state(Arc::new(state))
}

async fn proxy_responses(
    State(state): State<Arc<BrokerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy_to_upstream(state, headers, body, "/responses").await
}

async fn proxy_chat_completions(
    State(state): State<Arc<BrokerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy_to_upstream(state, headers, body, "/chat/completions").await
}

async fn proxy_to_upstream(
    state: Arc<BrokerState>,
    headers: HeaderMap,
    body: Bytes,
    path: &str,
) -> Response {
    let _permit = match authorize_sandbox_request(&state, &headers).await {
        Ok(permit) => permit,
        Err(response) => return response,
    };

    let url = format!("{}{}", state.upstream_base_url.trim_end_matches('/'), path);
    let result = state
        .client
        .post(url)
        .bearer_auth(state.upstream_api_key.expose_secret())
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await;

    match result {
        Ok(response) => {
            let status = response.status();
            match response.bytes().await {
                Ok(bytes) => (status, bytes).into_response(),
                Err(err) => (
                    StatusCode::BAD_GATEWAY,
                    format!("failed reading upstream response: {err}"),
                )
                    .into_response(),
            }
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            format!("upstream request failed: {err}"),
        )
            .into_response(),
    }
}

async fn authorize_sandbox_request(
    state: &Arc<BrokerState>,
    headers: &HeaderMap,
) -> Result<BrokerRequestPermit, Response> {
    let token = bearer_token(headers).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            "missing or invalid sandbox bearer token",
        )
            .into_response()
    })?;
    let chat_id = chat_id_metadata(headers);
    let owner = state
        .token_registry
        .owner_for_token(&token)
        .await
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "missing or invalid sandbox bearer token",
            )
                .into_response()
        })?;
    if chat_id.is_some_and(|chat_id| owner != chat_id) {
        return Err((StatusCode::FORBIDDEN, "sandbox token is not scoped to chat").into_response());
    }

    state
        .acquire_request_slot(token)
        .map_err(|error| match error {
            BrokerLimitError::TooManyRequests => (
                StatusCode::TOO_MANY_REQUESTS,
                "sandbox request limit exceeded",
            )
                .into_response(),
        })
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(AUTHORIZATION)?;

    let Ok(value) = header.to_str() else {
        return None;
    };

    let mut parts = value.split_ascii_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;

    if parts.next().is_some() || !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }

    Some(token.to_owned())
}

fn chat_id_metadata(headers: &HeaderMap) -> Option<ChatId> {
    let header = headers.get(CHAT_ID_HEADER)?;
    let value = header.to_str().ok()?;
    let raw = value.parse::<i64>().ok()?;
    Some(ChatId(raw))
}

impl BrokerState {
    fn acquire_request_slot(&self, token: String) -> Result<BrokerRequestPermit, BrokerLimitError> {
        if self.limits.max_requests_per_window == 0
            || self.limits.max_concurrent_requests == 0
            || self.limits.request_window.is_zero()
        {
            return Err(BrokerLimitError::TooManyRequests);
        }

        let mut request_limits = self
            .token_registry
            .inner
            .request_limits
            .lock()
            .map_err(|_| BrokerLimitError::TooManyRequests)?;
        let now = Instant::now();
        let request_state = request_limits
            .entry(token.clone())
            .or_insert_with(|| TokenRequestState::new(now));
        if now.duration_since(request_state.window_started) >= self.limits.request_window {
            request_state.window_started = now;
            request_state.requests_in_window = 0;
        }
        if request_state.in_flight >= self.limits.max_concurrent_requests
            || request_state.requests_in_window >= self.limits.max_requests_per_window
        {
            return Err(BrokerLimitError::TooManyRequests);
        }

        request_state.in_flight += 1;
        request_state.requests_in_window += 1;
        Ok(BrokerRequestPermit {
            token,
            registry: self.token_registry.clone(),
        })
    }
}

enum BrokerLimitError {
    TooManyRequests,
}

struct BrokerRequestPermit {
    token: String,
    registry: BrokerTokenRegistry,
}

impl Drop for BrokerRequestPermit {
    fn drop(&mut self) {
        let Ok(mut request_limits) = self.registry.inner.request_limits.lock() else {
            return;
        };
        if let Some(request_state) = request_limits.get_mut(&self.token) {
            request_state.in_flight = request_state.in_flight.saturating_sub(1);
        }
    }
}

#[derive(Default)]
struct BrokerTokenRegistryInner {
    active_tokens: RwLock<HashMap<String, ChatId>>,
    request_limits: Mutex<HashMap<String, TokenRequestState>>,
}

struct TokenRequestState {
    window_started: Instant,
    requests_in_window: usize,
    in_flight: usize,
}

impl TokenRequestState {
    fn new(window_started: Instant) -> Self {
        Self {
            window_started,
            requests_in_window: 0,
            in_flight: 0,
        }
    }
}

fn remove_chat_tokens(active_tokens: &mut HashMap<String, ChatId>, chat_id: ChatId) -> Vec<String> {
    let tokens = active_tokens
        .iter()
        .filter_map(|(token, owner)| (*owner == chat_id).then_some(token.clone()))
        .collect::<Vec<_>>();
    for token in &tokens {
        active_tokens.remove(token);
    }
    tokens
}

fn hex_token(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 0x0f) as usize] as char);
    }
    token
}

static GLOBAL_TOKEN_REGISTRY: LazyLock<Arc<BrokerTokenRegistryInner>> =
    LazyLock::new(|| Arc::new(BrokerTokenRegistryInner::default()));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ChatId;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tower::ServiceExt;

    fn state_for_upstream(upstream_base_url: String) -> BrokerState {
        let listen = "127.0.0.1:0".parse().expect("listen addr should parse");
        BrokerState::new_for_test(BrokerConfig {
            listen,
            upstream_base_url,
            upstream_api_key: SecretString::from("test-key".to_owned()),
        })
    }

    fn app_for_state(state: BrokerState) -> Router {
        router(state)
    }

    async fn registered_app_for_upstream(
        upstream_base_url: String,
        chat_id: ChatId,
        token: BrokerToken,
    ) -> Router {
        let state = state_for_upstream(upstream_base_url);
        state.register_token(chat_id, token).await;
        app_for_state(state)
    }

    fn bearer(token: &BrokerToken) -> String {
        format!("Bearer {}", token.as_str())
    }

    #[tokio::test]
    async fn router_should_return_bad_gateway_when_upstream_is_unavailable() {
        let token = BrokerToken::from("test-chat-token");
        let app = registered_app_for_upstream(
            "http://127.0.0.1:9/v1".to_owned(),
            ChatId(1),
            token.clone(),
        )
        .await;

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", bearer(&token))
                    .header("x-telellm-chat-id", "1")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("response should be returned");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn router_should_reject_missing_sandbox_token() {
        let app = app_for_state(state_for_upstream("http://127.0.0.1:9/v1".to_owned()));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("x-telellm-chat-id", "1")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("response should be returned");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn router_should_reject_invalid_sandbox_token() {
        let app = app_for_state(state_for_upstream("http://127.0.0.1:9/v1".to_owned()));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer wrong-token")
                    .header("x-telellm-chat-id", "1")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("response should be returned");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn router_should_reject_token_for_another_chat() {
        let state = state_for_upstream("http://127.0.0.1:9/v1".to_owned());
        let chat_a_token = BrokerToken::from("chat-a-token");
        let chat_b_token = BrokerToken::from("chat-b-token");
        state.register_token(ChatId(1), chat_a_token).await;
        state.register_token(ChatId(2), chat_b_token.clone()).await;
        let app = app_for_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", bearer(&chat_b_token))
                    .header("x-telellm-chat-id", "1")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("response should be returned");

        assert!(matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ));
    }

    #[tokio::test]
    async fn router_should_reject_stale_token_after_rotation() {
        let upstream = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream listener should bind");
        let upstream_addr = upstream
            .local_addr()
            .expect("upstream listener should have local addr");
        let upstream_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(50), upstream.accept()).await
        });

        let state = state_for_upstream(format!("http://{upstream_addr}/v1"));
        let old_token = BrokerToken::from("old-chat-token");
        let new_token = BrokerToken::from("new-chat-token");
        state.register_token(ChatId(1), old_token.clone()).await;
        state.rotate_token(ChatId(1), new_token).await;
        let app = app_for_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", bearer(&old_token))
                    .header("x-telellm-chat-id", "1")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("response should be returned");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            upstream_task
                .await
                .expect("upstream task should join")
                .is_err(),
            "upstream should not receive stale-token requests"
        );
    }

    #[tokio::test]
    async fn router_should_rate_limit_sandbox_requests() {
        let upstream = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream listener should bind");
        let upstream_addr = upstream
            .local_addr()
            .expect("upstream listener should have local addr");

        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream
                .accept()
                .await
                .expect("upstream should receive first request");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let bytes_read = stream
                    .read(&mut buffer)
                    .await
                    .expect("upstream request should be readable");
                if bytes_read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..bytes_read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\ncontent-type: application/json\r\n\r\n{}",
                )
                .await
                .expect("upstream response should write");
        });

        let state = BrokerState::new_with_limits_for_test(
            BrokerConfig {
                listen: "127.0.0.1:0".parse().expect("listen addr should parse"),
                upstream_base_url: format!("http://{upstream_addr}/v1"),
                upstream_api_key: SecretString::from("test-key".to_owned()),
            },
            BrokerLimits {
                max_requests_per_window: 1,
                request_window: Duration::from_secs(60),
                max_concurrent_requests: 1,
            },
        );
        let token = BrokerToken::from("limited-chat-token");
        state.register_token(ChatId(1), token.clone()).await;
        let app = app_for_state(state);

        let first_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", bearer(&token))
                    .header("x-telellm-chat-id", "1")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("first response should be returned");

        assert_eq!(first_response.status(), StatusCode::OK);
        upstream_task.await.expect("upstream task should complete");

        let second_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", bearer(&token))
                    .header("x-telellm-chat-id", "1")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("second response should be returned");

        assert_eq!(second_response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn router_should_forward_valid_sandbox_token_with_host_credential() {
        let upstream = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream listener should bind");
        let upstream_addr = upstream
            .local_addr()
            .expect("upstream listener should have local addr");

        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream
                .accept()
                .await
                .expect("upstream should receive request");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let bytes_read = stream
                    .read(&mut buffer)
                    .await
                    .expect("upstream request should be readable");
                if bytes_read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..bytes_read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request).to_string();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\ncontent-type: application/json\r\n\r\n{}",
                )
                .await
                .expect("upstream response should write");
            request
        });

        let token = BrokerToken::from("test-chat-token");
        let app = registered_app_for_upstream(
            format!("http://{upstream_addr}/v1"),
            ChatId(1),
            token.clone(),
        )
        .await;

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", bearer(&token))
                    .header("x-telellm-chat-id", "1")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("response should be returned");

        assert_eq!(response.status(), StatusCode::OK);

        let upstream_request = upstream_task.await.expect("upstream task should complete");
        assert!(upstream_request.starts_with("POST /v1/responses HTTP/1.1"));
        assert!(upstream_request.contains("authorization: Bearer test-key"));
    }

    #[tokio::test]
    async fn router_should_accept_valid_token_without_chat_metadata() {
        let upstream = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream listener should bind");
        let upstream_addr = upstream
            .local_addr()
            .expect("upstream listener should have local addr");

        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream
                .accept()
                .await
                .expect("upstream should receive request");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let bytes_read = stream
                    .read(&mut buffer)
                    .await
                    .expect("upstream request should be readable");
                if bytes_read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..bytes_read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\ncontent-type: application/json\r\n\r\n{}",
                )
                .await
                .expect("upstream response should write");
        });

        let token = BrokerToken::from("test-chat-token");
        let app = registered_app_for_upstream(
            format!("http://{upstream_addr}/v1"),
            ChatId(1),
            token.clone(),
        )
        .await;

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", bearer(&token))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("response should be returned");

        assert_eq!(response.status(), StatusCode::OK);
        upstream_task.await.expect("upstream task should complete");
    }
}
