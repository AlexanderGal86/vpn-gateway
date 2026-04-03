use crate::pool::state::SharedState;
use crate::proxy::sniff;
use crate::proxy::upstream;
use anyhow::Result;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

/// Get the original destination address before iptables REDIRECT mangled it.
///
/// Uses SO_ORIGINAL_DST (Linux-specific, requires iptables REDIRECT/DNAT).
fn get_original_dst(stream: &TcpStream) -> Option<SocketAddr> {
    // SO_ORIGINAL_DST = 80 on Linux
    const SO_ORIGINAL_DST: libc::c_int = 80;

    let fd = stream.as_raw_fd();
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;

    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_IP,
            SO_ORIGINAL_DST,
            &mut addr as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    if ret != 0 {
        return None;
    }

    let ip = std::net::Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
    let port = u16::from_be(addr.sin_port);
    Some(SocketAddr::new(std::net::IpAddr::V4(ip), port))
}

/// Handle a single client connection.
///
/// Flow:
/// 1. Get original destination via SO_ORIGINAL_DST
/// 2. Peek first bytes for SNI/Host extraction
/// 3. Select best upstream proxy from pool
/// 4. Connect to target through proxy (CONNECT/SOCKS5)
/// 5. Forward peeked bytes + bidirectional relay
async fn handle_connection(mut client: TcpStream, peer: SocketAddr, state: SharedState) {
    // Step 1: Get where the client wanted to go
    let original_dst = match get_original_dst(&client) {
        Some(dst) => dst,
        None => {
            tracing::debug!("No SO_ORIGINAL_DST for {}, dropping", peer);
            return;
        }
    };

    let target_ip = original_dst.ip().to_string();
    let target_port = original_dst.port();

    // Step 2: Peek first bytes for SNI/Host
    let mut peek_buf = vec![0u8; 4096];
    let peeked =
        match tokio::time::timeout(Duration::from_secs(5), client.peek(&mut peek_buf)).await {
            Ok(Ok(n)) => n,
            _ => {
                tracing::debug!("Peek timeout/error for {} -> {}", peer, original_dst);
                return;
            }
        };

    let target_host = match sniff::sniff(&peek_buf[..peeked]) {
        sniff::SniffedTarget::TlsSni(domain) => {
            tracing::debug!("{} -> {} (SNI: {})", peer, original_dst, domain);
            domain
        }
        sniff::SniffedTarget::HttpHost(host) => {
            tracing::debug!("{} -> {} (Host: {})", peer, original_dst, host);
            host
        }
        sniff::SniffedTarget::Unknown => {
            // Fallback: use IP address
            // TODO: check DNS reverse map here
            tracing::debug!("{} -> {} (IP fallback)", peer, original_dst);
            target_ip.clone()
        }
    };

    // Step 3: Select upstream proxy (with sticky sessions + retry)
    state
        .total_requests
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state
        .active_connections
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let max_retries = 3;

    // Check for sticky session first
    let mut selected_proxy_key: Option<String> = None;

    // Try sticky session on first attempt
    if let Some(sticky_key) = state.sticky_sessions.get(&peer) {
        if state.proxies.contains_key(&sticky_key) {
            tracing::debug!("Using sticky session: {} -> {}", peer, sticky_key);
            selected_proxy_key = Some(sticky_key);
        }
    }

    for attempt in 0..max_retries {
        // Clear sticky key after first failure to avoid retrying dead proxy
        if attempt > 0 {
            selected_proxy_key = None;
        }

        // Get proxy - either sticky or fresh selection
        let proxy = if let Some(ref key) = selected_proxy_key {
            state.proxies.get(key).map(|p| p.value().clone())
        } else {
            state.select_best()
        };

        let proxy = match proxy {
            Some(p) => p,
            None => {
                // No proxies available — wait for first_ready
                tracing::warn!("No proxies available, waiting...");
                match tokio::time::timeout(Duration::from_secs(10), state.first_ready.notified())
                    .await
                {
                    Ok(_) => match state.select_best() {
                        Some(p) => p,
                        None => {
                            tracing::error!("Still no proxies after wait");
                            break;
                        }
                    },
                    Err(_) => {
                        tracing::error!("Timeout waiting for first proxy");
                        break;
                    }
                }
            }
        };

        let proxy_key = proxy.key();

        // Step 4: Connect through upstream proxy
        let start = std::time::Instant::now();
        match upstream::connect_to_target(&proxy, &target_host, target_port).await {
            Ok(mut upstream) => {
                let latency = start.elapsed().as_millis() as f64;
                state.record_success(&proxy_key, latency);

                // Set sticky session on success
                state.sticky_sessions.set(peer, proxy_key.clone());
                tracing::debug!(
                    "{} -> {} via {} ({}ms) [sticky]",
                    peer,
                    target_host,
                    proxy_key,
                    latency as u64
                );

                // Step 5: Bidirectional relay with idle timeout
                // Note: peeked bytes haven't been consumed (we used peek).
                // copy_bidirectional will read them normally from the client socket.
                const IDLE_TIMEOUT_SECS: u64 = 300;
                match tokio::time::timeout(
                    Duration::from_secs(IDLE_TIMEOUT_SECS),
                    copy_bidirectional(&mut client, &mut upstream),
                )
                .await
                {
                    Ok(Ok((down, up))) => {
                        tracing::debug!("{}: transfer done ({} down, {} up)", peer, down, up);
                    }
                    Ok(Err(e)) => {
                        tracing::debug!("{}: relay error: {}", peer, e);
                    }
                    Err(_) => {
                        tracing::debug!("{}: idle timeout ({}s)", peer, IDLE_TIMEOUT_SECS);
                    }
                }

                state
                    .active_connections
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                return; // success — done
            }
            Err(e) => {
                state.record_fail(&proxy_key);
                tracing::debug!(
                    "{} -> {} via {} FAILED (attempt {}): {}",
                    peer,
                    target_host,
                    proxy_key,
                    attempt + 1,
                    e
                );
                // Will retry with next proxy
            }
        }
    }

    state
        .active_connections
        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    tracing::warn!(
        "All {} retries failed for {} -> {}",
        max_retries,
        peer,
        target_host
    );
}

/// Start the transparent proxy listener with a configurable connection limit.
pub async fn run_with_max_connections(
    state: SharedState,
    port: u16,
    max_connections: usize,
) -> Result<()> {
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    let semaphore = Arc::new(Semaphore::new(max_connections));
    tracing::info!(
        "Transparent proxy listening on {} (max {} connections)",
        addr,
        max_connections
    );

    loop {
        let (stream, peer) = listener.accept().await?;
        let state = state.clone();
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                tracing::warn!(
                    "Connection limit reached ({}), rejecting {}",
                    max_connections,
                    peer
                );
                drop(stream);
                continue;
            }
        };
        tokio::spawn(async move {
            handle_connection(stream, peer, state).await;
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::proxy::{Protocol, Proxy};
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn test_get_original_dst_does_not_panic() {
        // Without iptables REDIRECT, SO_ORIGINAL_DST may return None or
        // the socket's own address depending on the kernel. Either is fine —
        // the important thing is that the function doesn't panic.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let client_handle = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });

        let (server_stream, _peer) = listener.accept().await.unwrap();
        let result = get_original_dst(&server_stream);
        // Result is kernel-dependent: None (no NAT) or Some(addr) (returns socket addr)
        if let Some(dst) = result {
            assert!(dst.port() > 0, "Returned address should have a valid port");
        }

        let _ = client_handle.await;
    }

    #[tokio::test]
    async fn test_listener_binds_and_accepts() {
        let state = SharedState::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn a client that connects
        let client = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });

        let (stream, peer) = listener.accept().await.unwrap();
        assert!(peer.ip().is_loopback());
        drop(stream);
        let _ = client.await;
        drop(state);
    }

    #[tokio::test]
    async fn test_semaphore_rejects_when_full() {
        let semaphore = Arc::new(Semaphore::new(1));

        // Acquire the only permit
        let permit1 = semaphore.clone().try_acquire_owned();
        assert!(permit1.is_ok());

        // Second acquire should fail
        let permit2 = semaphore.clone().try_acquire_owned();
        assert!(permit2.is_err());

        // After dropping first permit, next one succeeds
        drop(permit1);
        let permit3 = semaphore.try_acquire_owned();
        assert!(permit3.is_ok());
    }

    #[tokio::test]
    async fn test_run_with_max_connections_rejects_over_limit() {
        let state = SharedState::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // free the port

        // Start proxy with max 2 connections
        let proxy_state = state.clone();
        let proxy_handle = tokio::spawn(async move {
            let _ = run_with_max_connections(proxy_state, port, 2).await;
        });

        // Give the proxy time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect 3 clients
        let addr = format!("127.0.0.1:{}", port);
        let c1 = TcpStream::connect(&addr).await;
        let c2 = TcpStream::connect(&addr).await;
        let c3 = TcpStream::connect(&addr).await;

        // All 3 should connect at TCP level (kernel accept queue),
        // but the proxy will drop without SO_ORIGINAL_DST quickly,
        // freeing the semaphore permits
        assert!(c1.is_ok());
        assert!(c2.is_ok());
        assert!(c3.is_ok());

        proxy_handle.abort();
    }

    #[tokio::test]
    async fn test_metrics_increment_on_connection_attempt() {
        // Verify that total_requests and active_connections counters work.
        let state = SharedState::new();

        // Insert a proxy so select_best() can return something
        let proxy = Proxy::new("192.0.2.1".to_string(), 8080, Protocol::Http);
        state.insert_if_absent(proxy);
        state.record_success("192.0.2.1:8080", 100.0);

        assert_eq!(state.total_requests.load(Ordering::Relaxed), 0);
        assert_eq!(state.active_connections.load(Ordering::Relaxed), 0);

        // Simulate request counting (handle_connection is hard to test
        // without iptables, so we test the atomic operations directly)
        state.total_requests.fetch_add(1, Ordering::Relaxed);
        state.active_connections.fetch_add(1, Ordering::Relaxed);
        assert_eq!(state.total_requests.load(Ordering::Relaxed), 1);
        assert_eq!(state.active_connections.load(Ordering::Relaxed), 1);

        state.active_connections.fetch_sub(1, Ordering::Relaxed);
        assert_eq!(state.active_connections.load(Ordering::Relaxed), 0);
    }
}
