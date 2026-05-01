use crate::sandbox::SANDBOX_BEARER_TOKEN;
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
use std::{net::SocketAddr, sync::Arc};

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
}

impl BrokerState {
    pub fn new(config: BrokerConfig) -> Self {
        Self {
            client: Client::new(),
            upstream_base_url: config.upstream_base_url,
            upstream_api_key: config.upstream_api_key,
        }
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
    if !has_valid_sandbox_token(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            "missing or invalid sandbox bearer token",
        )
            .into_response();
    }

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

fn has_valid_sandbox_token(headers: &HeaderMap) -> bool {
    let Some(header) = headers.get(AUTHORIZATION) else {
        return false;
    };

    let Ok(value) = header.to_str() else {
        return false;
    };

    let mut parts = value.split_ascii_whitespace();
    let Some(scheme) = parts.next() else {
        return false;
    };
    let Some(token) = parts.next() else {
        return false;
    };

    parts.next().is_none() && scheme.eq_ignore_ascii_case("Bearer") && token == SANDBOX_BEARER_TOKEN
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tower::ServiceExt;

    fn app_for_upstream(upstream_base_url: String) -> Router {
        let listen = "127.0.0.1:0".parse().expect("listen addr should parse");
        router(BrokerState::new(BrokerConfig {
            listen,
            upstream_base_url,
            upstream_api_key: SecretString::from("test-key".to_owned()),
        }))
    }

    #[tokio::test]
    async fn router_should_return_bad_gateway_when_upstream_is_unavailable() {
        let app = app_for_upstream("http://127.0.0.1:9/v1".to_owned());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer telellm-sandbox-token")
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
        let app = app_for_upstream("http://127.0.0.1:9/v1".to_owned());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
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
        let app = app_for_upstream("http://127.0.0.1:9/v1".to_owned());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer wrong-token")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request should build"),
            )
            .await
            .expect("response should be returned");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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

        let app = app_for_upstream(format!("http://{upstream_addr}/v1"));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer telellm-sandbox-token")
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
}
