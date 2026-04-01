#![allow(dead_code)]
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::net::TcpStream;

const MAX_IDLE_SECS: u64 = 30;
const MAX_PER_PROXY: usize = 5;

#[derive(Clone)]
pub struct ConnectionPool {
    pools: Arc<DashMap<String, Arc<Mutex<Vec<PooledConnection>>>>>,
    max_idle: Duration,
    max_per_proxy: usize,
}

/// A pooled TCP connection wrapper.
struct PooledConnection {
    stream: TcpStream,
    created_at: Instant,
    last_used: Instant,
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            pools: Arc::new(DashMap::new()),
            max_idle: Duration::from_secs(MAX_IDLE_SECS),
            max_per_proxy: MAX_PER_PROXY,
        }
    }

    pub fn with_config(max_idle_secs: u64, max_per_proxy: usize) -> Self {
        Self {
            pools: Arc::new(DashMap::new()),
            max_idle: Duration::from_secs(max_idle_secs),
            max_per_proxy,
        }
    }

    /// Try to get a connection from the pool.
    /// Pops candidates under lock, then checks liveness outside lock to avoid blocking.
    pub async fn get(&self, proxy_key: &str) -> Option<TcpStream> {
        let pool = self.pools.get(proxy_key)?;
        let now = Instant::now();

        // Pop up to max_per_proxy candidates under lock (fast, no I/O)
        let candidates: Vec<PooledConnection> = {
            let mut guard = pool.value().lock().await;
            let mut batch = Vec::new();
            while let Some(conn) = guard.pop() {
                if now.duration_since(conn.created_at) <= self.max_idle {
                    batch.push(conn);
                    break; // take one candidate at a time
                }
                // Too old — drop it (implicit)
            }
            batch
        };

        // Check liveness outside of lock
        for conn in candidates {
            if Self::is_connection_alive(&conn.stream).await {
                tracing::debug!("Connection pool hit: {}", proxy_key);
                return Some(conn.stream);
            }
        }

        None
    }

    /// Return a connection to the pool.
    pub async fn put(&self, proxy_key: &str, stream: TcpStream) {
        let pool = self.pools
            .entry(proxy_key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(Vec::new())))
            .value()
            .clone();

        let mut guard = pool.lock().await;
        
        if guard.len() >= self.max_per_proxy {
            return; // Pool full
        }

        guard.push(PooledConnection {
            stream,
            created_at: Instant::now(),
            last_used: Instant::now(),
        });
        
        tracing::debug!("Connection pool put: {}", proxy_key);
    }

    /// Check if a TCP connection is still alive.
    async fn is_connection_alive(stream: &TcpStream) -> bool {
        match tokio::time::timeout(
            Duration::from_millis(100),
            stream.peek(&mut [0u8; 1])
        ).await {
            Ok(Ok(_)) => true,
            _ => false,
        }
    }

    /// Clean up stale connections.
    pub async fn cleanup(&self) {
        let now = Instant::now();
        
        for pool in self.pools.iter() {
            let mut guard = pool.value().lock().await;
            guard.retain(|conn| now.duration_since(conn.created_at) < self.max_idle);
        }
    }

    /// Get pool stats.
    pub fn stats(&self) -> ConnectionPoolStats {
        let mut total = 0;
        let mut proxies = 0;
        
        for pool in self.pools.iter() {
            proxies += 1;
            if let Ok(guard) = pool.value().try_lock() {
                total += guard.len();
            }
        }

        ConnectionPoolStats {
            total_connections: total,
            proxy_count: proxies,
        }
    }

    /// Clear all connections.
    pub async fn clear(&self) {
        self.pools.clear();
    }
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ConnectionPoolStats {
    pub total_connections: usize,
    pub proxy_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let pool = ConnectionPool::new();
        assert_eq!(pool.stats().total_connections, 0);
    }

    #[test]
    fn test_pool_with_config() {
        let pool = ConnectionPool::with_config(30, 5);
        assert_eq!(pool.max_idle.as_secs(), 30);
    }
}
