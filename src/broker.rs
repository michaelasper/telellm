use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
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
    _headers: HeaderMap,
    body: Bytes,
    path: &str,
) -> Response {
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn router_should_return_bad_gateway_when_upstream_is_unavailable() {
        let listen = "127.0.0.1:0".parse().expect("listen addr should parse");
        let app = router(BrokerState::new(BrokerConfig {
            listen,
            upstream_base_url: "http://127.0.0.1:9/v1".to_owned(),
            upstream_api_key: SecretString::from("test-key".to_owned()),
        }));

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

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
}
