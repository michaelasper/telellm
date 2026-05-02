use crate::{config::UrlIngestionConfig, ids::MessageId};
use async_trait::async_trait;
use reqwest::{
    Url,
    header::{CONTENT_TYPE, LOCATION},
};
use scraper::{Html, Selector};
use std::{
    net::{IpAddr, Ipv6Addr},
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
    client: reqwest::Client,
}

impl UrlIngestor {
    pub fn new(config: UrlIngestionConfig) -> Result<Self, UrlIngestionError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(config.timeout_secs))
            .user_agent(config.user_agent.clone())
            .build()
            .map_err(|err| UrlIngestionError::Fetch(err.to_string()))?;

        Ok(Self { config, client })
    }

    pub async fn fetch_snapshot(
        &self,
        message_id: MessageId,
        index: usize,
        url: &Url,
    ) -> Result<UrlSnapshot, UrlIngestionError> {
        let original_url = url.clone();
        let mut current_url = url.clone();
        let mut redirects = 0;

        let mut response = loop {
            validate_fetch_url(&current_url).await?;
            let response = self
                .client
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
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ipv6_mapped_private_ip(ip)
        }
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

async fn validate_fetch_url(url: &Url) -> Result<(), UrlIngestionError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlIngestionError::UnsupportedScheme);
    }

    let host = url.host_str().ok_or(UrlIngestionError::UnsupportedScheme)?;
    let port = url
        .port_or_known_default()
        .ok_or(UrlIngestionError::UnsupportedScheme)?;
    let mut addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|err| UrlIngestionError::Fetch(err.to_string()))?;
    let mut resolved_any = false;
    for address in addresses.by_ref() {
        resolved_any = true;
        if is_blocked_ip(address.ip()) {
            return Err(UrlIngestionError::BlockedHost);
        }
    }

    if !resolved_any {
        return Err(UrlIngestionError::Fetch(
            "URL host did not resolve to an address".to_owned(),
        ));
    }

    Ok(())
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

    Err(UrlIngestionError::UnsupportedScheme)
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

fn ipv6_mapped_private_ip(ip: Ipv6Addr) -> bool {
    ip.to_ipv4_mapped().is_some_and(|mapped| {
        mapped.is_loopback()
            || mapped.is_private()
            || mapped.is_link_local()
            || mapped.is_multicast()
            || mapped.is_unspecified()
    })
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
}
