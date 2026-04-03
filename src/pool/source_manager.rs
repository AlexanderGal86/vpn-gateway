use crate::pool::proxy::{Protocol, Proxy};
use crate::pool::state::SharedState;
use futures::stream::{FuturesUnordered, StreamExt};
use serde::Deserialize;
use std::time::Duration;

/// Default free proxy sources.
fn default_sources() -> Vec<(&'static str, Protocol)> {
    vec![
        // HTTP proxy lists
        ("https://api.proxyscrape.com/v2/?request=getproxies&protocol=http&timeout=5000&country=all", Protocol::Http),
        ("https://raw.githubusercontent.com/TheSpeedX/PROXY-List/master/http.txt", Protocol::Http),
        ("https://raw.githubusercontent.com/monosans/proxy-list/main/proxies/http.txt", Protocol::Http),
        ("https://raw.githubusercontent.com/ShiftyTR/Proxy-List/master/http.txt", Protocol::Http),
        ("https://www.proxy-list.download/api/v1/get?type=http", Protocol::Http),
        ("https://raw.githubusercontent.com/clarketm/proxy-list/master/proxy-list-raw.txt", Protocol::Http),
        // SOCKS5 proxy lists
        ("https://api.proxyscrape.com/v2/?request=getproxies&protocol=socks5&timeout=5000&country=all", Protocol::Socks5),
        ("https://raw.githubusercontent.com/TheSpeedX/PROXY-List/master/socks5.txt", Protocol::Socks5),
        ("https://raw.githubusercontent.com/monosans/proxy-list/main/proxies/socks5.txt", Protocol::Socks5),
        ("https://raw.githubusercontent.com/ShiftyTR/Proxy-List/master/socks5.txt", Protocol::Socks5),
        ("https://www.proxy-list.download/api/v1/get?type=socks5", Protocol::Socks5),
    ]
}

/// Parse a single "ip:port" line.
fn parse_line(line: &str, protocol: Protocol) -> Option<Proxy> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut parts = line.splitn(2, ':');
    let host = parts.next()?.trim().to_string();
    let port: u16 = parts.next()?.trim().parse().ok()?;

    // Basic IP validation
    if host.parse::<std::net::Ipv4Addr>().is_err() {
        return None;
    }

    Some(Proxy::new(host, port, protocol))
}

/// Fetch a single source URL with retry and rate limiting.
async fn fetch_source(client: &reqwest::Client, url: &str, protocol: Protocol) -> Vec<Proxy> {
    // Retry up to 2 times with backoff
    for attempt in 0..3 {
        if attempt > 0 {
            // Rate limit between retries: 500ms stagger
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        match client.get(url).send().await {
            Ok(resp) => match resp.text().await {
                Ok(text) => {
                    if !text.is_empty() {
                        return text
                            .lines()
                            .filter_map(|line| parse_line(line, protocol.clone()))
                            .collect();
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to read body from {}: {}", url, e);
                }
            },
            Err(e) => {
                if attempt < 2 {
                    let backoff = Duration::from_secs(2u64.pow(attempt as u32));
                    tracing::warn!(
                        "Failed to fetch {} (attempt {}), retrying in {:?}: {}",
                        url,
                        attempt + 1,
                        backoff,
                        e
                    );
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                tracing::warn!("Failed to fetch {} after 3 attempts: {}", url, e);
            }
        }
        break;
    }
    Vec::new()
}

/// Fast bootstrap: fetch first 3 sources, take first 20 from each = 60 proxies.
/// Returns as soon as sources are fetched (health checking is separate).
pub async fn fast_bootstrap(state: &SharedState) -> usize {
    tracing::info!("Fast bootstrap: loading from top 3 sources...");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap_or_default();

    let sources = default_sources();
    let mut tasks = FuturesUnordered::new();

    // Only first 3 sources for speed
    for (url, proto) in sources.iter().take(3) {
        let client = client.clone();
        let url = url.to_string();
        let proto = proto.clone();
        tasks.push(async move { fetch_source(&client, &url, proto).await });
    }

    let mut count = 0;
    while let Some(proxies) = tasks.next().await {
        // Take only first 20 from each source for fast bootstrap
        for proxy in proxies.into_iter().take(20) {
            if state.insert_if_absent(proxy) {
                count += 1;
            }
        }
    }

    tracing::info!("Fast bootstrap complete: {} proxies loaded", count);
    count
}

/// Full source refresh: fetch all sources, deduplicate, add to pool.
#[allow(dead_code)]
pub async fn full_refresh(state: &SharedState) -> usize {
    full_refresh_with_sources(state, "config/sources.json").await
}

/// Max proxies accepted from a single source to prevent a rogue source from flooding the pool.
const MAX_PER_SOURCE: usize = 500;

/// Full source refresh with custom sources file.
pub async fn full_refresh_with_sources(state: &SharedState, sources_path: &str) -> usize {
    tracing::info!("Full source refresh starting...");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    // Try to load from file, fallback to defaults
    let source_urls = load_sources_from_file(sources_path).await;

    let sources: Vec<(&str, Protocol)> = source_urls
        .iter()
        .map(|url| {
            let proto = if url.contains("socks5") {
                Protocol::Socks5
            } else {
                Protocol::Http
            };
            (url.as_str(), proto)
        })
        .collect();

    let mut tasks = FuturesUnordered::new();

    for (url, proto) in sources {
        let client = client.clone();
        let url = url.to_string();
        tasks.push(async move { fetch_source(&client, &url, proto).await });
    }

    let mut count = 0;
    while let Some(proxies) = tasks.next().await {
        for proxy in proxies.into_iter().take(MAX_PER_SOURCE) {
            if state.insert_if_absent(proxy) {
                count += 1;
            }
        }
    }

    // Cleanup stale entries
    state.cleanup_stale();

    tracing::info!(
        "Full refresh complete: {} new proxies, {} total in pool",
        count,
        state.total_count()
    );
    count
}

/// Background loop: refresh sources every `interval` seconds.
#[allow(dead_code)]
pub async fn run_source_loop(state: SharedState, interval_secs: u64) {
    run_source_loop_with_path(state, interval_secs, "config/sources.json").await
}

/// Background loop with custom sources file.
pub async fn run_source_loop_with_path(state: SharedState, interval_secs: u64, sources_path: &str) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    ticker.tick().await; // skip first immediate tick

    loop {
        ticker.tick().await;
        full_refresh_with_sources(&state, sources_path).await;
    }
}

/// Load source URLs from JSON config file.
pub async fn load_sources_from_file(path: &str) -> Vec<String> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to read sources config: {}", e);
            return default_sources()
                .into_iter()
                .map(|(s, _)| s.to_string())
                .collect();
        }
    };

    #[derive(Deserialize)]
    struct SourceConfig {
        sources: Vec<String>,
    }

    match serde_json::from_str::<SourceConfig>(&content) {
        Ok(config) => config.sources,
        Err(e) => {
            tracing::warn!("Failed to parse sources config: {}", e);
            default_sources()
                .into_iter()
                .map(|(s, _)| s.to_string())
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_proxy_line() {
        let proxy = parse_line("1.2.3.4:8080", Protocol::Http);
        assert!(proxy.is_some());
        let proxy = proxy.unwrap();
        assert_eq!(proxy.host, "1.2.3.4");
        assert_eq!(proxy.port, 8080);
        assert_eq!(proxy.protocol, Protocol::Http);
    }

    #[test]
    fn test_parse_proxy_line_with_socks5() {
        let proxy = parse_line("5.6.7.8:1080", Protocol::Socks5);
        assert!(proxy.is_some());
        assert_eq!(proxy.unwrap().protocol, Protocol::Socks5);
    }

    #[test]
    fn test_parse_invalid_proxy_lines() {
        assert!(parse_line("", Protocol::Http).is_none());
        assert!(parse_line("noport", Protocol::Http).is_none());
        assert!(parse_line("1.2.3.4:", Protocol::Http).is_none());
        assert!(parse_line("1.2.3.4:99999", Protocol::Http).is_none());
        assert!(parse_line("# comment", Protocol::Http).is_none());
        assert!(parse_line("not-an-ip:8080", Protocol::Http).is_none());
        assert!(parse_line("1.2.3.4:8080:extra", Protocol::Http).is_none());
    }

    #[test]
    fn test_default_sources_not_empty() {
        let sources = default_sources();
        assert!(!sources.is_empty());
        assert!(sources.len() >= 10);
    }

    #[tokio::test]
    async fn test_load_sources_from_missing_file() {
        let sources = load_sources_from_file("nonexistent.json").await;
        assert!(!sources.is_empty());
        assert!(sources[0].contains("proxyscrape"));
    }

    #[tokio::test]
    async fn test_load_sources_from_invalid_file() {
        let path = "data/state.json";
        let sources = load_sources_from_file(path).await;
        assert!(!sources.is_empty());
    }
}
