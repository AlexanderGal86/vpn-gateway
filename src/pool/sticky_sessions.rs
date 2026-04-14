#![allow(dead_code)]
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const DEFAULT_TTL_SECS: u64 = 300;

#[derive(Clone, Debug)]
pub struct StickySession {
    pub client_ip: SocketAddr,
    pub proxy_key: String,
    /// Previous proxy key for fast failover
    pub backup_proxy_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_access: DateTime<Utc>,
    /// Number of successful connections through this sticky proxy
    pub success_count: u32,
}

pub struct StickySessionManager {
    sessions: Arc<DashMap<SocketAddr, StickySession>>,
    ttl_secs: Arc<AtomicU64>,
}

impl StickySessionManager {
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_TTL_SECS)
    }

    pub fn with_ttl(ttl_secs: u64) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            ttl_secs: Arc::new(AtomicU64::new(ttl_secs)),
        }
    }

    /// Update TTL at runtime (lock-free).
    pub fn set_ttl(&self, ttl_secs: u64) {
        self.ttl_secs.store(ttl_secs, Ordering::Relaxed);
        tracing::info!("Sticky session TTL updated to {}s", ttl_secs);
    }

    /// Get the sticky proxy for a client, if any.
    ///
    /// Uses dynamic TTL: base TTL + bonus for heavily-used sessions.
    /// A session with 100 successful connections gets up to 3x the base TTL,
    /// keeping proven proxy affinities alive much longer.
    pub fn get(&self, client_ip: &SocketAddr) -> Option<String> {
        let session = self.sessions.get(client_ip)?;
        let base_ttl = self.ttl_secs.load(Ordering::Relaxed) as i64;
        // +60s per 10 successes, capped at 2x base (total effective = 3x base)
        let bonus_secs = (session.success_count as i64 / 10) * 60;
        let effective_ttl = ChronoDuration::seconds(base_ttl + bonus_secs.min(base_ttl * 2));
        if session.last_access < Utc::now() - effective_ttl {
            drop(session);
            self.sessions.remove(client_ip);
            return None;
        }
        Some(session.proxy_key.clone())
    }

    /// Set sticky session for a client (overwrites any existing session).
    pub fn set(&self, client_ip: SocketAddr, proxy_key: String) {
        let now = Utc::now();
        self.sessions.insert(
            client_ip,
            StickySession {
                client_ip,
                proxy_key,
                backup_proxy_key: None,
                created_at: now,
                last_access: now,
                success_count: 1,
            },
        );
    }

    /// Set or touch a sticky session:
    /// - Same proxy: update last_access and increment success_count
    /// - Different proxy: demote old proxy to backup, set new one
    /// - No session: create new
    pub fn set_or_touch(&self, client_ip: SocketAddr, proxy_key: String) {
        let now = Utc::now();
        match self.sessions.get_mut(&client_ip) {
            Some(mut session) if session.proxy_key == proxy_key => {
                // Same proxy — touch and increment
                session.last_access = now;
                session.success_count += 1;
            }
            Some(mut session) => {
                // Different proxy — demote old to backup
                let old_key = session.proxy_key.clone();
                session.proxy_key = proxy_key;
                session.backup_proxy_key = Some(old_key);
                session.last_access = now;
                session.success_count = 1;
            }
            None => {
                // New session
                self.sessions.insert(
                    client_ip,
                    StickySession {
                        client_ip,
                        proxy_key,
                        backup_proxy_key: None,
                        created_at: now,
                        last_access: now,
                        success_count: 1,
                    },
                );
            }
        }
    }

    /// Get the backup proxy for a client (for faster failover on retry).
    pub fn get_backup(&self, client_ip: &SocketAddr) -> Option<String> {
        self.sessions
            .get(client_ip)
            .and_then(|s| s.backup_proxy_key.clone())
    }

    /// Update last access time.
    pub fn touch(&self, client_ip: &SocketAddr) {
        if let Some(mut session) = self.sessions.get_mut(client_ip) {
            session.last_access = Utc::now();
        }
    }

    /// Remove sticky session for a client.
    pub fn remove(&self, client_ip: &SocketAddr) {
        self.sessions.remove(client_ip);
    }

    /// Clean up expired sessions (uses dynamic TTL per session).
    pub fn cleanup(&self) {
        let now = Utc::now();
        let base_ttl = self.ttl_secs.load(Ordering::Relaxed) as i64;
        self.sessions.retain(|_, session| {
            let bonus_secs = (session.success_count as i64 / 10) * 60;
            let effective_ttl = ChronoDuration::seconds(base_ttl + bonus_secs.min(base_ttl * 2));
            now.signed_duration_since(session.last_access) < effective_ttl
        });
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

    #[test]
    fn test_set_or_touch_same_proxy() {
        let manager = StickySessionManager::new();
        let ip = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 12345);

        manager.set_or_touch(ip, "proxy1:8080".to_string());
        manager.set_or_touch(ip, "proxy1:8080".to_string());
        manager.set_or_touch(ip, "proxy1:8080".to_string());

        let session = manager.sessions.get(&ip).unwrap();
        assert_eq!(session.success_count, 3);
        assert_eq!(session.proxy_key, "proxy1:8080");
        assert!(session.backup_proxy_key.is_none());
    }

    #[test]
    fn test_set_or_touch_different_proxy_creates_backup() {
        let manager = StickySessionManager::new();
        let ip = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 12345);

        manager.set_or_touch(ip, "proxy1:8080".to_string());
        manager.set_or_touch(ip, "proxy2:8080".to_string());

        let session = manager.sessions.get(&ip).unwrap();
        assert_eq!(session.proxy_key, "proxy2:8080");
        assert_eq!(session.backup_proxy_key, Some("proxy1:8080".to_string()));
        assert_eq!(session.success_count, 1); // reset on proxy change
    }

    #[test]
    fn test_get_backup() {
        let manager = StickySessionManager::new();
        let ip = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 12345);

        // No session yet
        assert!(manager.get_backup(&ip).is_none());

        // Set first proxy — no backup
        manager.set_or_touch(ip, "proxy1:8080".to_string());
        assert!(manager.get_backup(&ip).is_none());

        // Switch to second proxy — first becomes backup
        manager.set_or_touch(ip, "proxy2:8080".to_string());
        assert_eq!(manager.get_backup(&ip), Some("proxy1:8080".to_string()));
    }

    #[test]
    fn test_dynamic_ttl_extends_for_active_sessions() {
        // Base TTL = 1 second
        let manager = StickySessionManager::with_ttl(1);
        let ip = SocketAddr::new(Ipv4Addr::new(192, 168, 1, 1).into(), 12345);

        manager.set_or_touch(ip, "proxy1:8080".to_string());

        // Simulate 20 successful connections → bonus = (20/10) * 60 = 120s
        // But capped at 2*base=2s, so effective TTL = 1+2 = 3s
        {
            let mut session = manager.sessions.get_mut(&ip).unwrap();
            session.success_count = 20;
        }

        // After 1.5s, a basic session would expire (TTL=1s)
        // but our extended session (TTL=3s) should still be alive
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(
            manager.get(&ip).is_some(),
            "Session with high success_count should survive past base TTL"
        );
    }
}
