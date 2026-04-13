use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// A pre-established TCP connection to a proxy server, ready for handshake.
struct WarmConnection {
    stream: TcpStream,
    #[allow(dead_code)]
    proxy_key: String,
    established_at: Instant,
}

/// Statistics about the warm pool for monitoring.
#[allow(dead_code)]
pub struct WarmPoolStats {
    pub total_connections: usize,
    pub proxies_tracked: usize,
    pub hits: u64,
    pub misses: u64,
}

/// Pool of pre-established TCP connections to top-scoring proxy servers.
///
/// The warm pool maintains ready-to-use TCP connections to the best proxies.
/// When a client needs to connect through a proxy, instead of TCP connect + handshake,
/// we can skip the TCP connect step (~50-200ms savings) by reusing a warm connection.
///
/// Connections are refreshed periodically to ensure they remain alive and to
/// track which proxies are currently reachable.
pub struct WarmPool {
    /// proxy_key → queue of warm connections (newest at back)
    connections: DashMap<String, Arc<Mutex<VecDeque<WarmConnection>>>>,
    /// Maximum warm connections per proxy
    max_per_proxy: usize,
    /// Maximum number of proxies to maintain warm connections for
    max_proxies: usize,
    /// Maximum age before a warm connection is considered stale
    max_age: Duration,
    /// Counters for monitoring
    hits: AtomicU64,
    misses: AtomicU64,
}

impl WarmPool {
    pub fn new(max_per_proxy: usize, max_proxies: usize, max_age_secs: u64) -> Self {
        Self {
            connections: DashMap::new(),
            max_per_proxy,
            max_proxies,
            max_age: Duration::from_secs(max_age_secs),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Try to get a warm TCP connection to a specific proxy.
    ///
    /// Returns None if no warm connections are available for this proxy.
    /// The returned TcpStream is already connected at TCP level — caller
    /// only needs to perform the protocol handshake (CONNECT/SOCKS5).
    pub async fn take(&self, proxy_key: &str) -> Option<TcpStream> {
        let queue = match self.connections.get(proxy_key) {
            Some(q) => q,
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        let mut guard = queue.value().lock().await;

        while let Some(conn) = guard.pop_front() {
            if conn.established_at.elapsed() < self.max_age {
                // Check liveness with a non-blocking peek
                if is_connection_alive(&conn.stream) {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return Some(conn.stream);
                }
            }
            // Too old or dead — drop it
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Refresh warm connections for top proxies.
    ///
    /// Called periodically by the maintenance loop. Selects the best proxies
    /// from the pool, removes stale connections, and establishes new ones.
    pub async fn refresh(&self, top_proxy_keys: &[(String, String)]) {
        let active_keys: std::collections::HashSet<&str> =
            top_proxy_keys.iter().map(|(k, _)| k.as_str()).collect();

        // Remove proxies no longer in the top set
        self.connections
            .retain(|key, _| active_keys.contains(key.as_str()));

        // Establish connections for each top proxy
        for (key, addr) in top_proxy_keys.iter().take(self.max_proxies) {
            let queue = self
                .connections
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(VecDeque::new())))
                .value()
                .clone();

            let mut guard = queue.lock().await;

            // Remove stale connections
            guard.retain(|c| c.established_at.elapsed() < self.max_age);

            // Fill up to max_per_proxy
            while guard.len() < self.max_per_proxy {
                match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await {
                    Ok(Ok(stream)) => {
                        // Set TCP keepalive on warm connections too
                        let sock_ref = socket2::SockRef::from(&stream);
                        let keepalive = socket2::TcpKeepalive::new()
                            .with_time(Duration::from_secs(15))
                            .with_interval(Duration::from_secs(10));
                        let _ = sock_ref.set_tcp_keepalive(&keepalive);

                        guard.push_back(WarmConnection {
                            stream,
                            proxy_key: key.clone(),
                            established_at: Instant::now(),
                        });
                    }
                    _ => break, // proxy unreachable, stop trying
                }
            }
        }
    }

    /// Get statistics about the warm pool.
    pub fn stats(&self) -> WarmPoolStats {
        let mut total = 0;
        for entry in self.connections.iter() {
            // We can't lock async here, so estimate from DashMap entry count
            total += self.max_per_proxy; // upper bound
            let _ = entry; // just counting entries
        }

        WarmPoolStats {
            total_connections: total,
            proxies_tracked: self.connections.len(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    /// Get exact connection count (async — needs to lock each queue).
    #[allow(dead_code)]
    pub async fn connection_count(&self) -> usize {
        let mut total = 0;
        for entry in self.connections.iter() {
            let guard = entry.value().lock().await;
            total += guard.len();
        }
        total
    }
}

/// Non-blocking check if a TCP connection is still alive.
///
/// Uses `peek()` with zero-length read: if the socket returns an error,
/// it's dead. If it returns Ok(0) with no data, it may still be alive.
fn is_connection_alive(stream: &TcpStream) -> bool {
    // Non-blocking peek via raw syscall to check if the socket is still open.
    // WouldBlock means no data but socket alive. EOF (0) or error means dead.
    let fd = std::os::fd::AsRawFd::as_raw_fd(stream);
    let mut buf = [0u8; 1];
    match unsafe {
        libc::recv(
            fd,
            buf.as_mut_ptr() as *mut libc::c_void,
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    } {
        -1 => {
            let err = std::io::Error::last_os_error();
            // WouldBlock = no data available, socket is alive
            err.kind() == std::io::ErrorKind::WouldBlock
        }
        0 => false, // EOF — peer closed
        _ => true,  // data available — socket alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_warm_pool_take_returns_none_when_empty() {
        let pool = WarmPool::new(2, 5, 45);
        assert!(pool.take("1.2.3.4:8080").await.is_none());
    }

    #[tokio::test]
    async fn test_warm_pool_refresh_and_take() {
        // Start a local listener to act as a "proxy"
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let key = addr.to_string();

        // Accept connections in background
        let accept_handle = tokio::spawn(async move {
            let mut streams = Vec::new();
            for _ in 0..2 {
                if let Ok((stream, _)) = listener.accept().await {
                    streams.push(stream);
                }
            }
            // Keep streams alive
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(streams);
        });

        let pool = WarmPool::new(2, 5, 45);
        let top = vec![(key.clone(), addr.to_string())];

        pool.refresh(&top).await;

        // Should be able to take a warm connection
        let conn = pool.take(&key).await;
        assert!(conn.is_some(), "Should get a warm connection after refresh");

        accept_handle.abort();
    }

    #[tokio::test]
    async fn test_warm_pool_stale_connections_evicted() {
        let pool = WarmPool::new(2, 5, 1); // 1 second max age

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let key = addr.to_string();

        let accept_handle = tokio::spawn(async move {
            let mut streams = Vec::new();
            for _ in 0..2 {
                if let Ok((s, _)) = listener.accept().await {
                    streams.push(s);
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(streams);
        });

        let top = vec![(key.clone(), addr.to_string())];
        pool.refresh(&top).await;

        // Wait for connections to go stale
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // Stale connections should be evicted
        let conn = pool.take(&key).await;
        assert!(conn.is_none(), "Stale connections should be evicted");

        accept_handle.abort();
    }

    #[tokio::test]
    async fn test_warm_pool_removes_untracked_proxies() {
        let pool = WarmPool::new(2, 5, 45);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let key = addr.to_string();

        let accept_handle = tokio::spawn(async move {
            let mut streams = Vec::new();
            for _ in 0..2 {
                if let Ok((s, _)) = listener.accept().await {
                    streams.push(s);
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(streams);
        });

        // Refresh with proxy
        let top = vec![(key.clone(), addr.to_string())];
        pool.refresh(&top).await;
        assert_eq!(pool.connections.len(), 1);

        // Refresh with empty set — proxy should be removed
        pool.refresh(&[]).await;
        assert_eq!(pool.connections.len(), 0);

        accept_handle.abort();
    }

    #[tokio::test]
    async fn test_warm_pool_hit_miss_counters() {
        let pool = WarmPool::new(2, 5, 45);

        // Miss — no connections
        let _ = pool.take("1.2.3.4:8080").await;
        let stats = pool.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 0);
    }
}
