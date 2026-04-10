use crate::pool::proxy::{Protocol, Proxy};
use crate::pool::state::SharedState;
use futures::stream::{FuturesUnordered, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

/// Max concurrent GeoIP HTTP lookups to avoid network/resource exhaustion.
const MAX_GEOIP_CONCURRENT: usize = 20;

/// TLS validation target — high availability, fast response, not blocked in most countries.
const TLS_CHECK_HOST: &str = "cloudflare.com";
const TLS_CHECK_PORT: u16 = 443;
const CONNECT_CHECK_PORT: u16 = 80;

/// Result of a full proxy health check (3-stage).
enum CheckResult {
    /// TCP + CONNECT + TLS clean
    Ok(f64),
    /// TCP + CONNECT ok, but TLS validation failed (MITM proxy)
    OkNoTls(f64),
    /// TCP or CONNECT failed — proxy is dead
    Dead,
}

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
/// Sends CONNECT to cloudflare.com:80 and checks for 200 response.
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

    let connect_req = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n\r\n",
        TLS_CHECK_HOST, CONNECT_CHECK_PORT, TLS_CHECK_HOST, CONNECT_CHECK_PORT
    );
    if write_half.write_all(connect_req.as_bytes()).await.is_err() {
        return (key, Err(()));
    }

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    let response = match tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        reader.read_line(&mut line),
    )
    .await
    {
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

/// Stage 3: TLS validation through proxy tunnel.
///
/// Connects to the proxy, establishes a CONNECT/SOCKS5 tunnel to a known HTTPS host,
/// then performs a TLS handshake with certificate validation. If the certificate is
/// invalid (e.g., self-signed, wrong CA), the proxy is doing MITM.
async fn check_tls_validity(proxy: &Proxy, timeout_ms: u64) -> bool {
    let timeout = Duration::from_millis(timeout_ms);

    // Step 1: TCP connect to proxy
    let stream = match tokio::time::timeout(timeout, TcpStream::connect(proxy.addr())).await {
        Ok(Ok(s)) => s,
        _ => return false, // can't connect — treat as unknown (not MITM)
    };

    // Step 2: Protocol-specific tunnel handshake to TLS_CHECK_HOST:443
    let stream = match proxy.protocol {
        Protocol::Http | Protocol::Https => {
            match tls_check_http_connect(stream, timeout_ms).await {
                Some(s) => s,
                None => return false,
            }
        }
        Protocol::Socks5 => match tls_check_socks5(stream, timeout_ms).await {
            Some(s) => s,
            None => return false,
        },
    };

    // Step 3: TLS handshake with certificate validation
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    let server_name = match rustls_pki_types::ServerName::try_from(TLS_CHECK_HOST) {
        Ok(name) => name.to_owned(),
        Err(_) => return false,
    };

    match tokio::time::timeout(timeout, connector.connect(server_name, stream)).await {
        Ok(Ok(_tls_stream)) => true, // Certificate valid — clean proxy
        Ok(Err(e)) => {
            tracing::debug!("TLS validation failed for {}: {}", proxy.key(), e);
            false // Certificate invalid — likely MITM
        }
        Err(_) => false, // Timeout — treat as failed
    }
}

/// HTTP CONNECT handshake for TLS check (to TLS_CHECK_HOST:443).
async fn tls_check_http_connect(mut stream: TcpStream, timeout_ms: u64) -> Option<TcpStream> {
    let request = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n\r\n",
        TLS_CHECK_HOST, TLS_CHECK_PORT, TLS_CHECK_HOST, TLS_CHECK_PORT
    );
    if stream.write_all(request.as_bytes()).await.is_err() {
        return None;
    }

    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    let response = match tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        reader.read_line(&mut line),
    )
    .await
    {
        Ok(Ok(_)) => line,
        _ => return None,
    };

    if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
        // Drain remaining headers
        let mut drain = String::new();
        loop {
            drain.clear();
            match tokio::time::timeout(
                Duration::from_millis(timeout_ms),
                reader.read_line(&mut drain),
            )
            .await
            {
                Ok(Ok(n)) if n > 0 => {
                    if drain.trim().is_empty() {
                        break; // End of headers
                    }
                }
                _ => break,
            }
        }
        Some(reader.into_inner().unsplit(write_half))
    } else {
        None
    }
}

/// SOCKS5 handshake for TLS check (to TLS_CHECK_HOST:443).
async fn tls_check_socks5(mut stream: TcpStream, timeout_ms: u64) -> Option<TcpStream> {
    use tokio::io::AsyncReadExt;
    let timeout = Duration::from_millis(timeout_ms);

    // Greeting: VER=5, NMETHODS=1, METHOD=0 (no auth)
    if stream.write_all(&[0x05, 0x01, 0x00]).await.is_err() {
        return None;
    }

    let mut resp = [0u8; 2];
    if tokio::time::timeout(timeout, stream.read_exact(&mut resp))
        .await
        .is_err()
    {
        return None;
    }
    if resp[0] != 0x05 || resp[1] == 0xFF {
        return None;
    }

    // Connect request: VER=5, CMD=CONNECT, RSV=0, ATYP=DOMAIN
    let domain = TLS_CHECK_HOST.as_bytes();
    let mut req = Vec::with_capacity(7 + domain.len());
    req.push(0x05); // VER
    req.push(0x01); // CMD = CONNECT
    req.push(0x00); // RSV
    req.push(0x03); // ATYP = DOMAIN
    req.push(domain.len() as u8);
    req.extend_from_slice(domain);
    req.extend_from_slice(&TLS_CHECK_PORT.to_be_bytes());

    if stream.write_all(&req).await.is_err() {
        return None;
    }

    // Response: VER, REP, RSV, ATYP
    let mut resp_header = [0u8; 4];
    if tokio::time::timeout(timeout, stream.read_exact(&mut resp_header))
        .await
        .is_err()
    {
        return None;
    }
    if resp_header[0] != 0x05 || resp_header[1] != 0x00 {
        return None;
    }

    // Skip BND.ADDR + BND.PORT
    match resp_header[3] {
        0x01 => {
            let mut skip = [0u8; 6];
            let _ = tokio::time::timeout(timeout, stream.read_exact(&mut skip)).await;
        }
        0x03 => {
            let mut len = [0u8; 1];
            let _ = tokio::time::timeout(timeout, stream.read_exact(&mut len)).await;
            let mut skip = vec![0u8; len[0] as usize + 2];
            let _ = tokio::time::timeout(timeout, stream.read_exact(&mut skip)).await;
        }
        0x04 => {
            let mut skip = [0u8; 18];
            let _ = tokio::time::timeout(timeout, stream.read_exact(&mut skip)).await;
        }
        _ => return None,
    }

    Some(stream)
}

/// Check a single proxy with three-stage verification.
/// Stage 1: TCP connect (fast filter)
/// Stage 2: HTTP CONNECT handshake (real verification)
/// Stage 3: TLS validation (MITM detection)
async fn check_single(proxy: &Proxy, timeout_ms: u64) -> (String, CheckResult) {
    let key = proxy.key();

    // Stage 1: TCP connect
    match check_tcp_connect(proxy, timeout_ms).await {
        (_, Ok(_)) => {}
        (_, Err(())) => return (key, CheckResult::Dead),
    }

    // Stage 2: HTTP CONNECT
    match check_http_connect(proxy, timeout_ms).await {
        (_, Ok(latency)) => {
            // Stage 3: TLS validation
            if check_tls_validity(proxy, timeout_ms).await {
                (key, CheckResult::Ok(latency))
            } else {
                (key, CheckResult::OkNoTls(latency))
            }
        }
        (_, Err(())) => (key, CheckResult::Dead),
    }
}

/// Fast probe: check a batch of proxies in parallel with three-stage verification.
/// Each working proxy is IMMEDIATELY added to the pool (early return pattern).
/// Returns the number of working proxies found.
pub async fn fast_probe(state: &SharedState, proxies: Vec<Proxy>, timeout_ms: u64) -> usize {
    if proxies.is_empty() {
        return 0;
    }

    let batch_size = proxies.len();
    tracing::info!(
        "Fast probe: checking {} proxies (timeout={}ms, 3-stage)",
        batch_size,
        timeout_ms
    );

    let mut tasks = FuturesUnordered::new();
    for proxy in &proxies {
        let proxy = proxy.clone();
        tasks.push(async move { check_single(&proxy, timeout_ms).await });
    }

    let geoip_semaphore = Arc::new(Semaphore::new(MAX_GEOIP_CONCURRENT));

    let mut found = 0;
    let mut tls_clean_count = 0;
    let mut mitm_count = 0;
    while let Some((key, result)) = tasks.next().await {
        match result {
            CheckResult::Ok(latency) => {
                state.record_success(&key, latency);
                state.set_tls_clean(&key, true);
                found += 1;
                tls_clean_count += 1;

                // GeoIP lookup for newly verified proxies (bounded concurrency)
                spawn_geoip_lookup(state, &key, &geoip_semaphore);

                if found == 1 {
                    tracing::info!(
                        "First working proxy found: {} ({}ms, TLS clean)",
                        key,
                        latency as u64
                    );
                    state.first_ready.notify_waiters();
                }
            }
            CheckResult::OkNoTls(latency) => {
                state.record_success(&key, latency);
                state.set_tls_clean(&key, false);
                found += 1;
                mitm_count += 1;

                // GeoIP lookup
                spawn_geoip_lookup(state, &key, &geoip_semaphore);

                if found == 1 {
                    tracing::info!(
                        "First working proxy found: {} ({}ms, MITM - HTTP only)",
                        key,
                        latency as u64
                    );
                    state.first_ready.notify_waiters();
                }
            }
            CheckResult::Dead => {
                state.record_fail(&key);
            }
        }
    }

    tracing::info!(
        "Fast probe complete: {}/{} working ({} TLS-clean, {} MITM)",
        found,
        batch_size,
        tls_clean_count,
        mitm_count,
    );
    found
}

/// Spawn a background GeoIP lookup for a proxy.
fn spawn_geoip_lookup(state: &SharedState, key: &str, semaphore: &Arc<Semaphore>) {
    let state_clone = state.clone();
    let proxy_key = key.to_string();
    let sem = semaphore.clone();
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
                    p.country = Some(country.clone());
                }
                state_clone.update_geo_index(&proxy_key, &country);
            }
        }
    });
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

                let tls_clean = state.tls_clean_count();
                let tls_dirty = state.tls_dirty_count();
                tracing::info!(
                    "Pool status: {} total, {} verified, {} available (TLS: {} clean, {} MITM)",
                    state.total_count(),
                    state.verified_count(),
                    state.available_count(),
                    tls_clean,
                    tls_dirty,
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

    #[test]
    fn test_check_result_variants() {
        // Verify CheckResult enum works correctly
        let ok = CheckResult::Ok(100.0);
        let no_tls = CheckResult::OkNoTls(200.0);
        let dead = CheckResult::Dead;

        match ok {
            CheckResult::Ok(l) => assert_eq!(l, 100.0),
            _ => panic!("Expected Ok"),
        }
        match no_tls {
            CheckResult::OkNoTls(l) => assert_eq!(l, 200.0),
            _ => panic!("Expected OkNoTls"),
        }
        match dead {
            CheckResult::Dead => {}
            _ => panic!("Expected Dead"),
        }
    }
}
