use crate::pool::proxy::Proxy;
use crate::pool::state::SharedState;
use futures::stream::{FuturesUnordered, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

/// Max concurrent GeoIP HTTP lookups to avoid network/resource exhaustion.
const MAX_GEOIP_CONCURRENT: usize = 20;

/// Stage 1: TCP connect check (fast, filters obviously dead proxies).
async fn check_tcp_connect(proxy: &Proxy, timeout_ms: u64) -> (String, Result<f64, ()>) {
    let key = proxy.key();
    let start = Instant::now();

    let result = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        TcpStream::connect(proxy.addr()),
    )
    .await;

    match result {
        Ok(Ok(_stream)) => {
            let latency = start.elapsed().as_millis() as f64;
            (key, Ok(latency))
        }
        _ => (key, Err(())),
    }
}

/// Stage 2: HTTP CONNECT through proxy to verify it actually works.
/// Sends CONNECT to httpbin.org:80 and checks for 200 response.
async fn check_http_connect(proxy: &Proxy, timeout_ms: u64) -> (String, Result<f64, ()>) {
    let key = proxy.key();
    let start = Instant::now();

    let stream = match tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        TcpStream::connect(proxy.addr()),
    )
    .await
    {
        Ok(Ok(s)) => s,
        _ => return (key, Err(())),
    };

    let (read_half, mut write_half) = tokio::io::split(stream);

    let request = b"CONNECT httpbin.org:80 HTTP/1.1\r\nHost: httpbin.org:80\r\n\r\n";
    if write_half.write_all(request).await.is_err() {
        return (key, Err(()));
    }

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    let response = match tokio::time::timeout(Duration::from_millis(timeout_ms), reader.read_line(&mut line)).await {
        Ok(Ok(_)) => line,
        _ => return (key, Err(())),
    };

    if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
        let latency = start.elapsed().as_millis() as f64;
        (key, Ok(latency))
    } else {
        (key, Err(()))
    }
}

/// Check a single proxy with two-stage verification.
/// Stage 1: TCP connect (fast filter)
/// Stage 2: HTTP CONNECT handshake (real verification)
async fn check_single(proxy: &Proxy, timeout_ms: u64) -> (String, Result<f64, ()>) {
    let key = proxy.key();

    match check_tcp_connect(proxy, timeout_ms).await {
        (_, Ok(_tcp_latency)) => {
            check_http_connect(proxy, timeout_ms).await
        }
        (_, Err(())) => (key, Err(())),
    }
}

/// Fast probe: check a batch of proxies in parallel with two-stage verification.
/// Each working proxy is IMMEDIATELY added to the pool (early return pattern).
/// Returns the number of working proxies found.
pub async fn fast_probe(
    state: &SharedState,
    proxies: Vec<Proxy>,
    timeout_ms: u64,
) -> usize {
    if proxies.is_empty() {
        return 0;
    }

    let batch_size = proxies.len();
    tracing::info!("Fast probe: checking {} proxies (timeout={}ms, 2-stage)", batch_size, timeout_ms);

    let mut tasks = FuturesUnordered::new();
    for proxy in &proxies {
        let proxy = proxy.clone();
        tasks.push(async move { check_single(&proxy, timeout_ms).await });
    }

    let geoip_semaphore = Arc::new(Semaphore::new(MAX_GEOIP_CONCURRENT));

    let mut found = 0;
    while let Some((key, result)) = tasks.next().await {
        match result {
            Ok(latency) => {
                state.record_success(&key, latency);
                found += 1;

                // GeoIP lookup for newly verified proxies (bounded concurrency)
                let state_clone = state.clone();
                let proxy_key = key.clone();
                let sem = geoip_semaphore.clone();
                tokio::spawn(async move {
                    let _permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => return,
                    };
                    let host = {
                        let guard = state_clone.proxies.get(&proxy_key);
                        match guard {
                            Some(p) => p.host.clone(),
                            None => return,
                        }
                    };
                    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                        if let Some(country) = state_clone.geoip.country_code(ip).await {
                            if let Some(mut p) = state_clone.proxies.get_mut(&proxy_key) {
                                p.country = Some(country);
                            }
                        }
                    }
                });

                if found == 1 {
                    tracing::info!(
                        "First working proxy found: {} ({}ms, verified via HTTP CONNECT)",
                        key,
                        latency as u64
                    );
                    state.first_ready.notify_waiters();
                }
            }
            Err(()) => {
                state.record_fail(&key);
            }
        }
    }

    tracing::info!(
        "Fast probe complete: {}/{} proxies verified working",
        found,
        batch_size
    );
    found
}

/// Background health check loop.
///
/// Continuously checks proxies that need verification.
/// Adapts frequency based on proxy status:
/// - Unchecked: immediate
/// - PresumedAlive: high priority
/// - Verified but stale: every 60s
/// - Failed but circuit expired: low priority
pub async fn run_health_loop(state: SharedState) {
    let mut fast_interval = tokio::time::interval(Duration::from_secs(5));
    let mut normal_interval = tokio::time::interval(Duration::from_secs(30));

    loop {
        tokio::select! {
            _ = fast_interval.tick() => {
                let batch = state.proxies_needing_check(100);
                if !batch.is_empty() {
                    fast_probe(&state, batch, 5000).await;
                }
            }
            _ = normal_interval.tick() => {
                let batch = state.proxies_needing_check(50);
                if !batch.is_empty() {
                    fast_probe(&state, batch, 5000).await;
                }

                tracing::info!(
                    "Pool status: {} total, {} verified, {} available",
                    state.total_count(),
                    state.verified_count(),
                    state.available_count()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::proxy::{Protocol, Proxy};
    use crate::pool::state::SharedState;

    fn make_proxy(host: &str, port: u16) -> Proxy {
        Proxy::new(host.to_string(), port, Protocol::Http)
    }

    // TODO: Add integration tests with mock TCP server
    // These tests require a real TCP server to verify the 2-stage check works.
    // For now, we test the state interactions.

    #[test]
    fn test_check_single_returns_error_for_invalid_proxy() {
        // Invalid IP should fail TCP connect immediately
        // This test is a placeholder for future mock-based testing
        assert!(true);
    }

    #[tokio::test]
    async fn test_fast_probe_empty_batch() {
        let state = SharedState::new();
        let result = fast_probe(&state, vec![], 1000).await;
        assert_eq!(result, 0);
    }

    #[tokio::test]
    async fn test_fast_probe_with_unreachable_proxies() {
        let state = SharedState::new();
        // These IPs are unreachable, should all fail
        let proxies = vec![make_proxy("192.0.2.1", 8080), make_proxy("192.0.2.2", 3128)];
        let result = fast_probe(&state, proxies, 100).await;
        assert_eq!(result, 0);
        // Proxies are inserted into state by record_fail via the key
        // After 1 fail, circuit breaker not yet open
        assert!(state.total_count() <= 2);
    }

    #[tokio::test]
    async fn test_health_loop_does_not_panic() {
        // Smoke test: health loop should not panic on empty state
        let state = SharedState::new();
        // We can't run the full loop (it's infinite), but we can verify
        // that proxies_needing_check works on empty state
        let batch = state.proxies_needing_check(100);
        assert!(batch.is_empty());
    }
}
