use crate::{config::UrlIngestionConfig, ids::MessageId};
use async_trait::async_trait;
use reqwest::{
    Url,
    header::{CONTENT_TYPE, LOCATION},
};
use scraper::{Html, Selector};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPage {
    pub title: Option<String>,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlSnapshot {
    pub original_url: String,
    pub final_url: String,
    pub title: Option<String>,
    pub status: u16,
    pub content_type: Option<String>,
    pub bytes: usize,
    pub workspace_path: String,
    pub host_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum UrlIngestionError {
    #[error("unsupported URL scheme")]
    UnsupportedScheme,
    #[error("unsupported URL content type: {0}")]
    UnsupportedContentType(String),
    #[error("URL host is blocked by safety policy")]
    BlockedHost,
    #[error("URL fetch failed: {0}")]
    Fetch(String),
    #[error("URL content exceeded {0} bytes")]
    TooLarge(usize),
    #[error("URL snapshot write failed: {0}")]
    SnapshotWrite(std::io::Error),
}

#[derive(Clone)]
pub struct UrlIngestor {
    config: UrlIngestionConfig,
}

impl UrlIngestor {
    pub fn new(config: UrlIngestionConfig) -> Result<Self, UrlIngestionError> {
        build_client(&config, None)?;

        Ok(Self { config })
    }

    pub async fn fetch_snapshot(
        &self,
        message_id: MessageId,
        index: usize,
        url: &Url,
    ) -> Result<UrlSnapshot, UrlIngestionError> {
        self.fetch_snapshot_with_resolver(message_id, index, url, &SystemUrlAddressResolver)
            .await
    }

    async fn fetch_snapshot_with_resolver<R>(
        &self,
        message_id: MessageId,
        index: usize,
        url: &Url,
        resolver: &R,
    ) -> Result<UrlSnapshot, UrlIngestionError>
    where
        R: UrlAddressResolver + ?Sized,
    {
        let original_url = url.clone();
        let mut current_url = url.clone();
        let mut redirects = 0;

        let mut response = loop {
            let hop = resolve_fetch_hop(&current_url, resolver).await?;
            let client = build_client(&self.config, Some((&hop.host, &hop.addresses)))?;
            let response = client
                .get(current_url.clone())
                .send()
                .await
                .map_err(|err| UrlIngestionError::Fetch(err.to_string()))?;

            if !response.status().is_redirection() {
                break response;
            }

            if redirects >= MAX_REDIRECTS {
                return Err(UrlIngestionError::Fetch(
                    "too many URL redirects".to_owned(),
                ));
            }

            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| UrlIngestionError::Fetch("redirect missing Location".to_owned()))?
                .to_str()
                .map_err(|err| UrlIngestionError::Fetch(err.to_string()))?;
            current_url = current_url
                .join(location)
                .map_err(|err| UrlIngestionError::Fetch(err.to_string()))?;
            redirects += 1;
        };

        let status = response.status().as_u16();
        let final_url = current_url;
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = read_limited_body(&mut response, self.config.max_fetch_bytes).await?;
        let extracted = extract_response_text(final_url.as_str(), content_type.as_deref(), &bytes)?;
        let workspace_path =
            snapshot_workspace_path(&self.config.workspace_dir, message_id, index, &final_url);
        let host_path = temp_snapshot_path(message_id, index, &final_url);
        let snapshot_markdown = render_snapshot_markdown(
            original_url.as_str(),
            final_url.as_str(),
            status,
            content_type.as_deref(),
            &extracted,
        );

        tokio::fs::write(&host_path, snapshot_markdown)
            .await
            .map_err(UrlIngestionError::SnapshotWrite)?;

        Ok(UrlSnapshot {
            original_url: original_url.to_string(),
            final_url: final_url.to_string(),
            title: extracted.title,
            status,
            content_type,
            bytes: bytes.len(),
            workspace_path,
            host_path,
        })
    }
}

struct FetchHop {
    host: String,
    addresses: Vec<SocketAddr>,
}

#[async_trait]
trait UrlAddressResolver: Send + Sync {
    async fn resolve_validated_addresses(
        &self,
        url: &Url,
    ) -> Result<Vec<SocketAddr>, UrlIngestionError>;
}

struct SystemUrlAddressResolver;

#[async_trait]
impl UrlAddressResolver for SystemUrlAddressResolver {
    async fn resolve_validated_addresses(
        &self,
        url: &Url,
    ) -> Result<Vec<SocketAddr>, UrlIngestionError> {
        let host = url.host_str().ok_or(UrlIngestionError::UnsupportedScheme)?;
        let port = url
            .port_or_known_default()
            .ok_or(UrlIngestionError::UnsupportedScheme)?;
        let addresses: Vec<_> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|err| UrlIngestionError::Fetch(err.to_string()))?
            .collect();
        validate_resolved_addresses(&addresses)?;
        Ok(addresses)
    }
}

#[async_trait]
pub trait UrlContextProvider: Send + Sync {
    async fn snapshots_for_text(
        &self,
        message_id: MessageId,
        text: &str,
    ) -> Vec<Result<UrlSnapshot, UrlIngestionError>>;
}

#[async_trait]
impl UrlContextProvider for UrlIngestor {
    async fn snapshots_for_text(
        &self,
        message_id: MessageId,
        text: &str,
    ) -> Vec<Result<UrlSnapshot, UrlIngestionError>> {
        if !self.config.enabled {
            return Vec::new();
        }

        let urls = detect_urls(text);
        let mut snapshots = Vec::with_capacity(urls.len().min(self.config.max_urls_per_message));
        for (index, url) in urls
            .iter()
            .take(self.config.max_urls_per_message)
            .enumerate()
        {
            snapshots.push(self.fetch_snapshot(message_id, index, url).await);
        }
        snapshots
    }
}

pub fn detect_urls(text: &str) -> Vec<Url> {
    text.split_whitespace()
        .filter_map(|token| {
            let trimmed = token.trim_end_matches(['.', ',', ')', ';', ']', '}', '>']);
            let url = Url::parse(trimmed).ok()?;
            if matches!(url.scheme(), "http" | "https") {
                Some(url)
            } else {
                None
            }
        })
        .collect()
}

pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !is_public_ipv4(ip),
        IpAddr::V6(ip) => !is_public_ipv6(ip),
    }
}

pub fn extract_readable_html(
    _final_url: &str,
    bytes: &[u8],
) -> Result<ExtractedPage, UrlIngestionError> {
    let html = String::from_utf8_lossy(bytes);
    let document = Html::parse_document(html.as_ref());
    let title_selector = Selector::parse("title").expect("static title selector should parse");
    let main_selector = Selector::parse("main").expect("static main selector should parse");
    let body_selector = Selector::parse("body").expect("static body selector should parse");

    let title = document
        .select(&title_selector)
        .next()
        .map(|element| normalize_whitespace(&element.text().collect::<Vec<_>>().join(" ")))
        .filter(|title| !title.is_empty());

    let mut readable_text = collect_selector_text(&document, &main_selector);
    if readable_text.is_empty() {
        readable_text = collect_selector_text(&document, &body_selector);
    }
    if readable_text.is_empty() {
        readable_text = normalize_whitespace(html.as_ref());
    }

    let mut markdown = String::new();
    if let Some(title) = &title {
        markdown.push_str("# ");
        markdown.push_str(title);
    }
    if !readable_text.is_empty() {
        if !markdown.is_empty() {
            markdown.push_str("\n\n");
        }
        markdown.push_str(&readable_text);
    }

    Ok(ExtractedPage { title, markdown })
}

pub fn snapshot_workspace_path(
    workspace_dir: &str,
    message_id: MessageId,
    index: usize,
    url: &Url,
) -> String {
    let workspace_dir = workspace_dir.trim_matches('/');
    let host = url.host_str().unwrap_or("url");
    let host_file_name = crate::bot::message::sanitize_file_name(host);
    format!(
        "{workspace_dir}/msg-{}/{}-{host_file_name}.md",
        message_id.0,
        index + 1
    )
}

async fn resolve_fetch_hop<R>(url: &Url, resolver: &R) -> Result<FetchHop, UrlIngestionError>
where
    R: UrlAddressResolver + ?Sized,
{
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlIngestionError::UnsupportedScheme);
    }

    let host = url
        .host_str()
        .ok_or(UrlIngestionError::UnsupportedScheme)?
        .to_owned();
    let addresses = resolver.resolve_validated_addresses(url).await?;
    if addresses.is_empty() {
        return Err(UrlIngestionError::Fetch(
            "URL host did not resolve to an address".to_owned(),
        ));
    }

    Ok(FetchHop { host, addresses })
}

fn validate_resolved_addresses(addresses: &[SocketAddr]) -> Result<(), UrlIngestionError> {
    if addresses.is_empty() {
        return Err(UrlIngestionError::Fetch(
            "URL host did not resolve to an address".to_owned(),
        ));
    }

    for address in addresses {
        if is_blocked_ip(address.ip()) {
            return Err(UrlIngestionError::BlockedHost);
        }
    }

    Ok(())
}

fn build_client(
    config: &UrlIngestionConfig,
    resolved: Option<(&str, &[SocketAddr])>,
) -> Result<reqwest::Client, UrlIngestionError> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(config.timeout_secs))
        .user_agent(config.user_agent.clone());

    if let Some((host, addresses)) = resolved {
        builder = builder.resolve_to_addrs(host, addresses);
    }

    builder
        .build()
        .map_err(|err| UrlIngestionError::Fetch(err.to_string()))
}

async fn read_limited_body(
    response: &mut reqwest::Response,
    max_fetch_bytes: usize,
) -> Result<Vec<u8>, UrlIngestionError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| UrlIngestionError::Fetch(err.to_string()))?
    {
        if bytes.len() + chunk.len() > max_fetch_bytes {
            return Err(UrlIngestionError::TooLarge(max_fetch_bytes));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn extract_response_text(
    final_url: &str,
    content_type: Option<&str>,
    bytes: &[u8],
) -> Result<ExtractedPage, UrlIngestionError> {
    let normalized = content_type.unwrap_or_default().to_ascii_lowercase();
    if normalized.contains("text/html") {
        return extract_readable_html(final_url, bytes);
    }

    if normalized.starts_with("text/") {
        return Ok(ExtractedPage {
            title: None,
            markdown: String::from_utf8_lossy(bytes).into_owned(),
        });
    }

    Err(UrlIngestionError::UnsupportedContentType(
        content_type.unwrap_or("unknown").to_owned(),
    ))
}

fn collect_selector_text(document: &Html, selector: &Selector) -> String {
    let mut raw_text = String::new();
    for element in document.select(selector) {
        for text in element.text() {
            if !raw_text.is_empty() {
                raw_text.push(' ');
            }
            raw_text.push_str(text);
        }
    }
    normalize_whitespace(&raw_text)
}

fn normalize_whitespace(text: &str) -> String {
    let mut normalized = String::new();
    for word in text.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    ![
        (Ipv4Addr::new(0, 0, 0, 0), 8),
        (Ipv4Addr::new(10, 0, 0, 0), 8),
        (Ipv4Addr::new(100, 64, 0, 0), 10),
        (Ipv4Addr::new(127, 0, 0, 0), 8),
        (Ipv4Addr::new(169, 254, 0, 0), 16),
        (Ipv4Addr::new(172, 16, 0, 0), 12),
        (Ipv4Addr::new(192, 0, 0, 0), 24),
        (Ipv4Addr::new(192, 0, 2, 0), 24),
        (Ipv4Addr::new(192, 88, 99, 0), 24),
        (Ipv4Addr::new(192, 168, 0, 0), 16),
        (Ipv4Addr::new(198, 18, 0, 0), 15),
        (Ipv4Addr::new(198, 51, 100, 0), 24),
        (Ipv4Addr::new(203, 0, 113, 0), 24),
        (Ipv4Addr::new(224, 0, 0, 0), 4),
        (Ipv4Addr::new(240, 0, 0, 0), 4),
    ]
    .into_iter()
    .any(|(network, prefix)| ipv4_in_range(ip, network, prefix))
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if ip.to_ipv4_mapped().is_some() {
        return false;
    }

    ![
        (Ipv6Addr::UNSPECIFIED, 128),
        (Ipv6Addr::LOCALHOST, 128),
        (Ipv6Addr::new(0x0064, 0xff9b, 0, 0, 0, 0, 0, 0), 96),
        (Ipv6Addr::new(0x0064, 0xff9b, 0x0001, 0, 0, 0, 0, 0), 48),
        (Ipv6Addr::new(0x0100, 0, 0, 0, 0, 0, 0, 0), 64),
        (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 23),
        (Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0), 32),
        (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16),
        (Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20),
        (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
        (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
        (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8),
    ]
    .into_iter()
    .any(|(network, prefix)| ipv6_in_range(ip, network, prefix))
}

fn ipv4_in_range(ip: Ipv4Addr, network: Ipv4Addr, prefix: u32) -> bool {
    let mask = u32::MAX << (32 - prefix);
    u32::from(ip) & mask == u32::from(network) & mask
}

fn ipv6_in_range(ip: Ipv6Addr, network: Ipv6Addr, prefix: u32) -> bool {
    let mask = u128::MAX << (128 - prefix);
    u128::from(ip) & mask == u128::from(network) & mask
}

fn temp_snapshot_path(message_id: MessageId, index: usize, url: &Url) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let host = url.host_str().unwrap_or("url");
    let host_file_name = crate::bot::message::sanitize_file_name(host);
    std::env::temp_dir().join(format!(
        "telellm-url-{}-{}-{index}-{host_file_name}-{nanos}-{counter}.md",
        std::process::id(),
        message_id.0
    ))
}

fn render_snapshot_markdown(
    original_url: &str,
    final_url: &str,
    status: u16,
    content_type: Option<&str>,
    extracted: &ExtractedPage,
) -> String {
    let mut output = String::new();
    output.push_str("# URL Snapshot\n\n");
    output.push_str("- URL: ");
    output.push_str(original_url);
    output.push('\n');
    output.push_str("- Final URL: ");
    output.push_str(final_url);
    output.push('\n');
    output.push_str("- Status: ");
    output.push_str(&status.to_string());
    output.push('\n');
    output.push_str("- Content-Type: ");
    output.push_str(content_type.unwrap_or("unknown"));
    output.push('\n');
    output.push_str("- Title: ");
    output.push_str(extracted.title.as_deref().unwrap_or("untitled page"));
    output.push_str("\n\n");
    if extracted.markdown.trim().is_empty() {
        output.push_str("(no readable text extracted)\n");
    } else {
        output.push_str(extracted.markdown.trim());
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UrlIngestionConfig;
    use std::{net::SocketAddr, sync::Arc};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[test]
    fn detect_urls_should_trim_common_trailing_punctuation() {
        let urls = detect_urls("Read https://example.com/path?x=1, then https://example.org.");
        assert_eq!(urls[0].as_str(), "https://example.com/path?x=1");
        assert_eq!(urls[1].as_str(), "https://example.org/");
    }

    #[test]
    fn safety_should_reject_private_and_local_hosts() {
        assert!(is_blocked_ip("127.0.0.1".parse().expect("ip")));
        assert!(is_blocked_ip("10.1.2.3".parse().expect("ip")));
        assert!(is_blocked_ip("172.16.0.1".parse().expect("ip")));
        assert!(is_blocked_ip("192.168.1.1".parse().expect("ip")));
        assert!(is_blocked_ip("169.254.1.1".parse().expect("ip")));
        assert!(!is_blocked_ip("93.184.216.34".parse().expect("ip")));
    }

    #[test]
    fn safety_should_reject_reserved_and_documentation_ranges() {
        for ip in [
            "100.64.0.1",
            "198.18.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "255.255.255.255",
            "64:ff9b::1",
            "2001:db8::1",
            "3fff::1",
        ] {
            assert!(
                is_blocked_ip(ip.parse().expect("ip")),
                "{ip} should be blocked"
            );
        }
        assert!(!is_blocked_ip("93.184.216.34".parse().expect("ip")));
    }

    #[test]
    fn html_extraction_should_return_title_and_text() {
        let extracted = extract_readable_html(
            "https://example.com/page",
            br#"<html><head><title>Example</title></head><body><main><h1>Hello</h1><p>World</p></main></body></html>"#,
        )
        .expect("html should parse");

        assert_eq!(extracted.title.as_deref(), Some("Example"));
        assert!(extracted.markdown.contains("Hello"));
        assert!(extracted.markdown.contains("World"));
    }

    #[test]
    fn workspace_file_name_should_be_stable() {
        let config = UrlIngestionConfig::default();
        let url = reqwest::Url::parse("https://example.com/a/b?x=1").expect("url");
        assert_eq!(
            snapshot_workspace_path(&config.workspace_dir, crate::ids::MessageId(5), 0, &url),
            "web_pages/msg-5/1-example.com.md"
        );
    }

    #[tokio::test]
    async fn fetch_should_fail_when_body_exceeds_limit() {
        let addr = spawn_test_server(|_| {
            http_response("200 OK", &[("Content-Type", "text/plain")], "123456")
        })
        .await;
        let config = UrlIngestionConfig {
            max_fetch_bytes: 5,
            ..UrlIngestionConfig::default()
        };
        let ingestor = UrlIngestor::new(config).expect("ingestor");
        let url = test_url(addr, "/large");
        let resolver = StaticResolver::new("example.com", vec![addr]);

        let err = ingestor
            .fetch_snapshot_with_resolver(MessageId(9), 0, &url, &resolver)
            .await
            .expect_err("body should exceed limit");

        assert!(matches!(err, UrlIngestionError::TooLarge(5)));
    }

    #[tokio::test]
    async fn fetch_should_report_unsupported_content_type() {
        let addr = spawn_test_server(|_| {
            http_response("200 OK", &[("Content-Type", "application/pdf")], "%PDF")
        })
        .await;
        let ingestor = UrlIngestor::new(UrlIngestionConfig::default()).expect("ingestor");
        let url = test_url(addr, "/file.pdf");
        let resolver = StaticResolver::new("example.com", vec![addr]);

        let err = ingestor
            .fetch_snapshot_with_resolver(MessageId(9), 0, &url, &resolver)
            .await
            .expect_err("pdf should be rejected");

        match err {
            UrlIngestionError::UnsupportedContentType(content_type) => {
                assert_eq!(content_type, "application/pdf");
            }
            other => panic!("expected unsupported content type, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_should_reject_redirect_to_blocked_host() {
        let addr = spawn_test_server(|_| {
            http_response("302 Found", &[("Location", "http://127.0.0.1/private")], "")
        })
        .await;
        let ingestor = UrlIngestor::new(UrlIngestionConfig::default()).expect("ingestor");
        let url = test_url(addr, "/redirect");
        let resolver = StaticResolver::new("example.com", vec![addr]);

        let err = ingestor
            .fetch_snapshot_with_resolver(MessageId(9), 0, &url, &resolver)
            .await
            .expect_err("redirect to loopback should be rejected");

        assert!(matches!(err, UrlIngestionError::BlockedHost));
    }

    #[tokio::test]
    async fn fetch_should_stop_after_redirect_limit() {
        let addr =
            spawn_test_server(|_| http_response("302 Found", &[("Location", "/loop")], "")).await;
        let ingestor = UrlIngestor::new(UrlIngestionConfig::default()).expect("ingestor");
        let url = test_url(addr, "/loop");
        let resolver = StaticResolver::new("example.com", vec![addr]);

        let err = ingestor
            .fetch_snapshot_with_resolver(MessageId(9), 0, &url, &resolver)
            .await
            .expect_err("redirect loop should be capped");

        match err {
            UrlIngestionError::Fetch(reason) => assert!(reason.contains("too many URL redirects")),
            other => panic!("expected redirect fetch error, got {other:?}"),
        }
    }

    struct StaticResolver {
        host: String,
        addresses: Vec<SocketAddr>,
    }

    impl StaticResolver {
        fn new(host: &str, addresses: Vec<SocketAddr>) -> Self {
            Self {
                host: host.to_owned(),
                addresses,
            }
        }
    }

    #[async_trait]
    impl UrlAddressResolver for StaticResolver {
        async fn resolve_validated_addresses(
            &self,
            url: &Url,
        ) -> Result<Vec<SocketAddr>, UrlIngestionError> {
            if url.host_str() == Some(self.host.as_str()) {
                return Ok(self.addresses.clone());
            }

            let host = url.host_str().ok_or(UrlIngestionError::UnsupportedScheme)?;
            let port = url
                .port_or_known_default()
                .ok_or(UrlIngestionError::UnsupportedScheme)?;
            let address = SocketAddr::new(
                host.parse::<IpAddr>()
                    .map_err(|err| UrlIngestionError::Fetch(err.to_string()))?,
                port,
            );
            validate_resolved_addresses(&[address])?;
            Ok(vec![address])
        }
    }

    async fn spawn_test_server(
        handler: impl Fn(String) -> String + Send + Sync + 'static,
    ) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let addr = listener.local_addr().expect("test server address");
        let handler = Arc::new(handler);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _peer)) = listener.accept().await else {
                    break;
                };
                let handler = Arc::clone(&handler);
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 4096];
                    let Ok(read) = stream.read(&mut buffer).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                    let response = handler(request);
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        addr
    }

    fn test_url(addr: SocketAddr, path: &str) -> Url {
        Url::parse(&format!("http://example.com:{}{path}", addr.port())).expect("test URL")
    }

    fn http_response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        response.push_str(body);
        response
    }
}
