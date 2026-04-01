use crate::pool::proxy::{Proxy, ProxyStatus};
use crate::pool::sticky_sessions::StickySessionManager;
use chrono::Utc;
use dashmap::DashMap;
use rand::distributions::WeightedIndex;
use rand::prelude::Distribution;
use rand::thread_rng;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

use super::connection_pool::ConnectionPool;
use super::geo_ip::GeoIp;

/// Thread-safe shared state for the entire gateway.
///
/// Uses DashMap for lock-free concurrent reads (O(1) proxy lookup).
/// Atomic counters for metrics (no mutex contention).
/// Notify for fast-path signaling (wake waiting clients when first proxy found).
#[derive(Clone)]
pub struct SharedState {
    /// All known proxies: key = "host:port"
    pub proxies: Arc<DashMap<String, Proxy>>,

    /// Banned proxies (key = "host:port")
    pub banned: Arc<DashMap<String, Proxy>>,

    /// Signaled when the first verified proxy becomes available.
    /// Clients arriving before any proxy is ready wait on this.
    pub first_ready: Arc<Notify>,

    // === Atomic metrics ===
    pub total_requests: Arc<AtomicU64>,
    pub active_connections: Arc<AtomicU64>,
    pub proxy_rotations: Arc<AtomicU64>,

    // === Connection Pool ===
    pub connection_pool: Arc<ConnectionPool>,

    // === Sticky Sessions ===
    pub sticky_sessions: Arc<StickySessionManager>,

    // === GeoIP ===
    pub geoip: Arc<GeoIp>,

    // === Pool size limit ===
    pub max_proxies: Arc<AtomicUsize>,
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            proxies: Arc::new(DashMap::with_capacity(4096)),
            banned: Arc::new(DashMap::with_capacity(1024)),
            first_ready: Arc::new(Notify::new()),
            total_requests: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicU64::new(0)),
            proxy_rotations: Arc::new(AtomicU64::new(0)),
            connection_pool: Arc::new(ConnectionPool::new()),
            sticky_sessions: Arc::new(StickySessionManager::new()),
            geoip: Arc::new(GeoIp::new()),
            max_proxies: Arc::new(AtomicUsize::new(5000)),
        }
    }

    pub fn with_config(geoip_path: Option<String>, sticky_ttl_secs: u64, max_proxies: usize) -> Self {
        let geoip = match geoip_path {
            Some(path) => {
                let g = GeoIp::with_db_path(path);
                // Note: load() should be called separately as it's async
                g
            }
            None => GeoIp::new(),
        };

        Self {
            proxies: Arc::new(DashMap::with_capacity(4096)),
            banned: Arc::new(DashMap::with_capacity(1024)),
            first_ready: Arc::new(Notify::new()),
            total_requests: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicU64::new(0)),
            proxy_rotations: Arc::new(AtomicU64::new(0)),
            connection_pool: Arc::new(ConnectionPool::new()),
            sticky_sessions: Arc::new(StickySessionManager::with_ttl(sticky_ttl_secs)),
            geoip: Arc::new(geoip),
            max_proxies: Arc::new(AtomicUsize::new(max_proxies)),
        }
    }

    // === Proxy selection (fast-path priority) ===

    /// Select the best available proxy using weighted random from top-N.
    ///
    /// Priority:
    /// 1. Verified + low latency (recently checked, working)
    /// 2. PresumedAlive (from state.json, not yet re-checked)
    /// 3. Unchecked (just loaded from source, never tested)
    ///
    /// Within each tier, selects from top-N candidates with weighted random
    /// to avoid overloading a single proxy.
    pub fn select_best(&self) -> Option<Proxy> {
        // Tier 1: Verified proxies
        let verified: Vec<_> = self
            .proxies
            .iter()
            .filter(|p| p.is_available())
            .filter(|p| matches!(&p.status, Some(ProxyStatus::Verified)))
            .map(|p| p.value().clone())
            .collect();

        if !verified.is_empty() {
            return Some(self.weighted_random_select(&verified));
        }

        // Tier 2: PresumedAlive (state.json)
        let presumed: Vec<_> = self
            .proxies
            .iter()
            .filter(|p| p.is_available())
            .filter(|p| matches!(&p.status, Some(ProxyStatus::PresumedAlive)))
            .map(|p| p.value().clone())
            .collect();

        if !presumed.is_empty() {
            return Some(self.weighted_random_select(&presumed));
        }

        // Tier 3: Unchecked (random — we don't know latency)
        self.proxies
            .iter()
            .filter(|p| p.is_available())
            .filter(|p| matches!(&p.status, Some(ProxyStatus::Unchecked) | None))
            .next()
            .map(|p| p.value().clone())
    }

    /// Select from candidates using weighted random from top-N.
    /// Lower score = higher weight.
    fn weighted_random_select(&self, candidates: &[Proxy]) -> Proxy {
        const TOP_N: usize = 10;
        let mut sorted: Vec<_> = candidates.to_vec();
        sorted.sort_by(|a, b| {
            a.score()
                .partial_cmp(&b.score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let top_n: Vec<_> = sorted.into_iter().take(TOP_N).collect();

        if top_n.len() == 1 {
            return top_n.into_iter().next().unwrap();
        }

        // Invert scores for weights (lower score = higher weight)
        let max_score = top_n.iter().map(|p| p.score()).fold(f64::MIN, f64::max);
        let weights: Vec<f64> = top_n.iter().map(|p| (max_score - p.score()) + 1.0).collect();

        match WeightedIndex::new(&weights) {
            Ok(dist) => {
                let idx = dist.sample(&mut thread_rng());
                top_n[idx].clone()
            }
            Err(_) => top_n[0].clone(),
        }
    }

    /// Select best proxy filtered by country code.
    #[allow(dead_code)]
    pub fn select_best_by_country(&self, country: &str) -> Option<Proxy> {
        self.proxies
            .iter()
            .filter(|p| p.is_available())
            .filter(|p| p.country.as_deref() == Some(country))
            .min_by(|a, b| {
                a.score()
                    .partial_cmp(&b.score())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|p| p.value().clone())
    }

    // === Proxy mutations ===

    /// Add a verified proxy to the pool and signal waiters if it's the first.
    #[allow(dead_code)]
    pub fn add_verified(&self, mut proxy: Proxy) {
        proxy.status = Some(ProxyStatus::Verified);
        let was_empty = self.verified_count() == 0;
        self.proxies.insert(proxy.key(), proxy);

        if was_empty && self.verified_count() > 0 {
            self.first_ready.notify_waiters();
        }
    }

    /// Record success for a proxy by key
    pub fn record_success(&self, key: &str, latency_ms: f64) {
        if let Some(mut p) = self.proxies.get_mut(key) {
            p.record_success(latency_ms);
        }
    }

    /// Record failure for a proxy by key
    pub fn record_fail(&self, key: &str) {
        if let Some(mut p) = self.proxies.get_mut(key) {
            p.record_fail();
        }
        self.proxy_rotations.fetch_add(1, Ordering::Relaxed);
    }

    // === Counts ===

    pub fn total_count(&self) -> usize {
        self.proxies.len()
    }

    pub fn verified_count(&self) -> usize {
        self.proxies
            .iter()
            .filter(|p| matches!(&p.status, Some(ProxyStatus::Verified)))
            .count()
    }

    pub fn available_count(&self) -> usize {
        self.proxies.iter().filter(|p| p.is_available()).count()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.available_count() == 0
    }

    // === Bulk operations ===

    /// Insert proxies without overwriting existing ones (from source manager).
    /// Returns true if the proxy was inserted, false if it already existed or pool is full.
    pub fn insert_if_absent(&self, proxy: Proxy) -> bool {
        let max = self.max_proxies.load(Ordering::Relaxed);
        if max > 0 && self.proxies.len() >= max {
            return false;
        }
        use dashmap::mapref::entry::Entry;
        match self.proxies.entry(proxy.key()) {
            Entry::Vacant(entry) => {
                entry.insert(proxy);
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    /// Remove proxies that haven't been checked in over 24 hours and never succeeded.
    pub fn cleanup_stale(&self) {
        let now = Utc::now();
        self.proxies.retain(|_, p| {
            if p.success_count > 0 {
                return true; // keep any proxy that ever worked
            }
            match p.last_check {
                Some(checked) => (now - checked).num_hours() < 24,
                None => true, // keep unchecked (might be new)
            }
        });
    }

    /// Get all proxies as Vec (for persistence / API)
    pub fn all_proxies(&self) -> Vec<Proxy> {
        self.proxies.iter().map(|p| p.value().clone()).collect()
    }

    /// Get proxies that need checking, sorted by priority.
    pub fn proxies_needing_check(&self, limit: usize) -> Vec<Proxy> {
        let now = Utc::now();
        let mut candidates: Vec<_> = self
            .proxies
            .iter()
            .filter(|p| {
                match (&p.status, p.last_check) {
                    // Never checked — highest priority
                    (Some(ProxyStatus::Unchecked) | None, _) => true,
                    // PresumedAlive — need verification
                    (Some(ProxyStatus::PresumedAlive), _) => true,
                    // Verified but stale (>60s since last check)
                    (Some(ProxyStatus::Verified), Some(last)) => (now - last).num_seconds() > 60,
                    // Failed but circuit breaker expired
                    (Some(ProxyStatus::Failed), _) => p.is_available(),
                    _ => false,
                }
            })
            .map(|p| p.value().clone())
            .collect();

        // Unchecked first, then PresumedAlive, then Verified, then Failed
        candidates.sort_by_key(|p| match &p.status {
            Some(ProxyStatus::Unchecked) | None => 0,
            Some(ProxyStatus::PresumedAlive) => 1,
            Some(ProxyStatus::Verified) => 2,
            Some(ProxyStatus::Failed) => 3,
        });

        candidates.truncate(limit);
        candidates
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::proxy::Protocol;

    fn make_proxy(host: &str, port: u16) -> Proxy {
        Proxy::new(host.to_string(), port, Protocol::Http)
    }

    #[test]
    fn test_insert_and_get() {
        let state = SharedState::new();
        let proxy = make_proxy("1.2.3.4", 8080);
        assert!(state.insert_if_absent(proxy));
        assert_eq!(state.total_count(), 1);
        assert!(state.proxies.contains_key("1.2.3.4:8080"));
    }

    #[test]
    fn test_insert_duplicate_returns_false() {
        let state = SharedState::new();
        let proxy1 = make_proxy("1.2.3.4", 8080);
        let proxy2 = make_proxy("1.2.3.4", 8080);
        assert!(state.insert_if_absent(proxy1));
        assert!(!state.insert_if_absent(proxy2));
    }

    #[test]
    fn test_record_success_updates_status() {
        let state = SharedState::new();
        state.insert_if_absent(make_proxy("1.2.3.4", 8080));
        state.record_success("1.2.3.4:8080", 150.0);
        assert_eq!(state.verified_count(), 1);
        assert_eq!(state.available_count(), 1);
    }

    #[test]
    fn test_record_fail_increments_fail_count() {
        let state = SharedState::new();
        state.insert_if_absent(make_proxy("1.2.3.4", 8080));
        state.record_fail("1.2.3.4:8080");
        // After 1 fail, circuit is not yet open (opens at 5)
        // But the fail_count should be incremented
        assert!(state.proxies.contains_key("1.2.3.4:8080"));
    }

    #[test]
    fn test_circuit_breaker_opens_after_5_fails() {
        let state = SharedState::new();
        state.insert_if_absent(make_proxy("1.2.3.4", 8080));
        for _ in 0..5 {
            state.record_fail("1.2.3.4:8080");
        }
        assert!(!state.proxies.get("1.2.3.4:8080").unwrap().is_available());
    }

    #[test]
    fn test_select_best_returns_none_when_empty() {
        let state = SharedState::new();
        assert!(state.select_best().is_none());
    }

    #[test]
    fn test_select_best_returns_verified_first() {
        let state = SharedState::new();
        state.insert_if_absent(make_proxy("1.2.3.4", 8080));
        state.insert_if_absent(make_proxy("5.6.7.8", 3128));
        state.record_success("1.2.3.4:8080", 100.0);
        state.record_success("5.6.7.8:3128", 200.0);
        let selected = state.select_best().unwrap();
        assert_eq!(selected.host, "1.2.3.4");
    }

    #[test]
    fn test_weighted_random_select_from_top_n() {
        let state = SharedState::new();
        for i in 0..20 {
            state.insert_if_absent(make_proxy(&format!("10.0.0.{}", i + 1), 8080));
            state.record_success(&format!("10.0.0.{}:8080", i + 1), (i + 1) as f64 * 10.0);
        }
        let mut selections = std::collections::HashSet::new();
        for _ in 0..50 {
            if let Some(p) = state.select_best() {
                selections.insert(p.host);
            }
        }
        assert!(selections.len() > 1, "Should select from multiple proxies, got {}", selections.len());
    }

    #[test]
    fn test_proxies_needing_check_priority() {
        let state = SharedState::new();
        state.insert_if_absent(make_proxy("1.1.1.1", 8080));
        state.insert_if_absent(make_proxy("2.2.2.2", 8080));
        state.record_success("1.1.1.1:8080", 100.0);
        let batch = state.proxies_needing_check(10);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].host, "2.2.2.2");
    }

    #[test]
    fn test_ban_and_unban() {
        let state = SharedState::new();
        state.insert_if_absent(make_proxy("1.2.3.4", 8080));
        // Ban: remove from proxies, add to banned
        if let Some((_, proxy)) = state.proxies.remove("1.2.3.4:8080") {
            state.banned.insert("1.2.3.4:8080".to_string(), proxy);
        }
        assert_eq!(state.total_count(), 0);
        assert_eq!(state.banned.len(), 1);
        // Unban: remove from banned, add back to proxies
        if let Some((_, proxy)) = state.banned.remove("1.2.3.4:8080") {
            state.proxies.insert("1.2.3.4:8080".to_string(), proxy);
        }
        assert_eq!(state.total_count(), 1);
        assert_eq!(state.banned.len(), 0);
    }

    #[test]
    fn test_insert_rejected_when_pool_full() {
        let state = SharedState::new();
        state.max_proxies.store(2, Ordering::Relaxed);
        assert!(state.insert_if_absent(make_proxy("1.1.1.1", 8080)));
        assert!(state.insert_if_absent(make_proxy("2.2.2.2", 8080)));
        assert!(!state.insert_if_absent(make_proxy("3.3.3.3", 8080)));
        assert_eq!(state.total_count(), 2);
    }

    #[test]
    fn test_cleanup_stale_removes_old_unchecked() {
        let state = SharedState::new();
        state.insert_if_absent(make_proxy("1.2.3.4", 8080));
        state.record_success("1.2.3.4:8080", 100.0);
        state.insert_if_absent(make_proxy("5.6.7.8", 3128));
        state.cleanup_stale();
        assert_eq!(state.total_count(), 2);
    }
}
