#![allow(dead_code)]
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_TTL_SECS: u64 = 300;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct StickySession {
    pub client_ip: SocketAddr,
    pub proxy_key: String,
    pub created_at: DateTime<Utc>,
    pub last_access: DateTime<Utc>,
}

pub struct StickySessionManager {
    sessions: Arc<DashMap<SocketAddr, StickySession>>,
    ttl: Arc<RwLock<Duration>>,
}

impl StickySessionManager {
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_TTL_SECS)
    }

    pub fn with_ttl(ttl_secs: u64) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            ttl: Arc::new(RwLock::new(Duration::from_secs(ttl_secs))),
        }
    }

    /// Update TTL at runtime.
    pub fn set_ttl(&self, ttl_secs: u64) {
        let mut ttl = self.ttl.write();
        *ttl = Duration::from_secs(ttl_secs);
        tracing::info!("Sticky session TTL updated to {}s", ttl_secs);
    }

    /// Get the sticky proxy for a client, if any.
    pub fn get(&self, client_ip: &SocketAddr) -> Option<String> {
        let session = self.sessions.get(client_ip)?;
        let ttl = ChronoDuration::seconds(self.ttl.read().as_secs() as i64);
        if session.last_access < Utc::now() - ttl {
            drop(session);
            self.sessions.remove(client_ip);
            return None;
        }
        Some(session.proxy_key.clone())
    }

    /// Set sticky session for a client.
    pub fn set(&self, client_ip: SocketAddr, proxy_key: String) {
        let now = Utc::now();

        self.sessions.insert(
            client_ip,
            StickySession {
                client_ip,
                proxy_key,
                created_at: now,
                last_access: now,
            },
        );
    }

    /// Update last access time.
    #[allow(dead_code)]
    pub fn touch(&self, client_ip: &SocketAddr) {
        if let Some(mut session) = self.sessions.get_mut(client_ip) {
            session.last_access = Utc::now();
        }
    }

    /// Remove sticky session for a client.
    pub fn remove(&self, client_ip: &SocketAddr) {
        self.sessions.remove(client_ip);
    }

    /// Clean up expired sessions.
    pub fn cleanup(&self) {
        let now = Utc::now();
        let ttl = ChronoDuration::seconds(self.ttl.read().as_secs() as i64);
        self.sessions
            .retain(|_, session| now.signed_duration_since(session.last_access) < ttl);
    }

    /// Get session count.
    pub fn count(&self) -> usize {
        self.sessions.len()
    }

    /// Clear all sessions.
    pub fn clear(&self) {
        self.sessions.clear();
    }
}

impl Default for StickySessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_sticky_session() {
        let manager = StickySessionManager::new();
        let client_ip = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 12345);

        // Initially no session
        assert!(manager.get(&client_ip).is_none());

        // Set session
        manager.set(client_ip, "1.2.3.4:8080".to_string());

        // Now we have a session
        assert!(manager.get(&client_ip).is_some());

        // Remove
        manager.remove(&client_ip);
        assert!(manager.get(&client_ip).is_none());
    }

    #[test]
    fn test_session_cleanup() {
        let manager = StickySessionManager::with_ttl(1);
        let client_ip = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 12345);

        manager.set(client_ip, "1.2.3.4:8080".to_string());
        assert_eq!(manager.count(), 1);

        manager.cleanup();
        assert_eq!(manager.count(), 1);
    }

    #[test]
    fn test_session_expires() {
        let manager = StickySessionManager::with_ttl(1);
        let client_ip = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 12345);

        manager.set(client_ip, "1.2.3.4:8080".to_string());
        assert!(manager.get(&client_ip).is_some());

        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(manager.get(&client_ip).is_none());
    }

    #[test]
    fn test_multiple_sessions() {
        let manager = StickySessionManager::new();
        let ip1 = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 12345);
        let ip2 = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 2).into(), 12346);

        manager.set(ip1, "proxy1:8080".to_string());
        manager.set(ip2, "proxy2:8080".to_string());

        assert_eq!(manager.count(), 2);
        assert_eq!(manager.get(&ip1), Some("proxy1:8080".to_string()));
        assert_eq!(manager.get(&ip2), Some("proxy2:8080".to_string()));
    }

    #[test]
    fn test_clear_all_sessions() {
        let manager = StickySessionManager::new();
        let ip1 = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 12345);
        let ip2 = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 2).into(), 12346);

        manager.set(ip1, "proxy1:8080".to_string());
        manager.set(ip2, "proxy2:8080".to_string());
        manager.clear();

        assert_eq!(manager.count(), 0);
        assert!(manager.get(&ip1).is_none());
        assert!(manager.get(&ip2).is_none());
    }
}
