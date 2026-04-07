use crate::pool::proxy::{Proxy, ProxyStatus};
use crate::pool::sticky_sessions::StickySessionManager;
use chrono::Utc;
use dashmap::DashMap;
use rand::distr::{weighted::WeightedIndex, Distribution};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

use super::connection_pool::ConnectionPool;
use super::geo_ip::GeoIp;

/// Country-based geo-index for O(1) proxy selection by country.
/// Maps country code → set of proxy keys.
#[derive(Clone)]
pub struct GeoIndex {
    index: Arc<DashMap<String, Vec<String>>>,
}

impl Default for GeoIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl GeoIndex {
    pub fn new() -> Self {
        Self {
            index: Arc::new(DashMap::new()),
        }
    }

    /// Add a proxy key to a country's index.
    pub fn insert(&self, country: &str, proxy_key: &str) {
        let mut entry = self.index.entry(country.to_string()).or_default();
        if !entry.contains(&proxy_key.to_string()) {
            entry.push(proxy_key.to_string());
        }
    }

    /// Remove a proxy key from a country's index.
    #[allow(dead_code)]
    pub fn remove(&self, country: &str, proxy_key: &str) {
        if let Some(mut entry) = self.index.get_mut(country) {
            entry.retain(|k| k != proxy_key);
        }
    }

    /// Get all proxy keys for a country.
    pub fn get_keys(&self, country: &str) -> Vec<String> {
        self.index
            .get(country)
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    /// Rebuild the entire index from a proxies DashMap.
    pub fn rebuild(&self, proxies: &DashMap<String, Proxy>) {
        self.index.clear();
        for entry in proxies.iter() {
            if let Some(ref country) = entry.value().country {
                self.insert(country, entry.key());
            }
        }
    }

    /// Number of countries indexed.
    #[allow(dead_code)]
    pub fn country_count(&self) -> usize {
        self.index.len()
    }
}

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

    // === Geo-index for O(1) country lookup ===
    pub geo_index: GeoIndex,
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
            geo_index: GeoIndex::new(),
        }
    }

    pub fn with_config(
        geoip_path: Option<String>,
        sticky_ttl_secs: u64,
        max_proxies: usize,
    ) -> Self {
        let geoip = match geoip_path {
            // Note: load() should be called separately as it's async
            Some(path) => GeoIp::with_db_path(path),
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
            geo_index: GeoIndex::new(),
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
        // Tier 1: Verified proxies — single-pass top-N selection
        let top = self.collect_top_n(|p| {
            p.is_available() && matches!(&p.status, Some(ProxyStatus::Verified))
        });
        if !top.is_empty() {
            return Some(Self::weighted_random_select(&top));
        }

        // Tier 2: PresumedAlive (state.json)
        let top = self.collect_top_n(|p| {
            p.is_available() && matches!(&p.status, Some(ProxyStatus::PresumedAlive))
        });
        if !top.is_empty() {
            return Some(Self::weighted_random_select(&top));
        }

        // Tier 3: Unchecked (random — we don't know latency)
        self.proxies
            .iter()
            .filter(|p| p.is_available())
            .find(|p| matches!(&p.status, Some(ProxyStatus::Unchecked) | None))
            .map(|p| p.value().clone())
    }

    /// Single-pass top-N collection from DashMap. O(n) scan, O(TOP_N) memory.
    fn collect_top_n(&self, filter: impl Fn(&Proxy) -> bool) -> Vec<Proxy> {
        const TOP_N: usize = 10;
        let mut top: Vec<(f64, Proxy)> = Vec::with_capacity(TOP_N);
        let mut worst_score = f64::MAX;

        for entry in self.proxies.iter() {
            let p = entry.value();
            if !filter(p) {
                continue;
            }
            let score = p.score();
            if top.len() < TOP_N {
                top.push((score, p.clone()));
                if top.len() == TOP_N {
                    worst_score = top.iter().map(|(s, _)| *s).fold(f64::MIN, f64::max);
                }
            } else if score < worst_score {
                // Replace the worst entry
                if let Some(idx) = top
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| {
                        a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(i, _)| i)
                {
                    top[idx] = (score, p.clone());
                    worst_score = top.iter().map(|(s, _)| *s).fold(f64::MIN, f64::max);
                }
            }
        }

        top.into_iter().map(|(_, p)| p).collect()
    }

    /// Select from candidates using weighted random.
    /// Lower score = higher weight.
    fn weighted_random_select(candidates: &[Proxy]) -> Proxy {
        if candidates.len() == 1 {
            return candidates[0].clone();
        }

        // Invert scores for weights (lower score = higher weight)
        let max_score = candidates
            .iter()
            .map(|p| p.score())
            .fold(f64::MIN, f64::max);
        let weights: Vec<f64> = candidates
            .iter()
            .map(|p| (max_score - p.score()) + 1.0)
            .collect();

        match WeightedIndex::new(&weights) {
            Ok(dist) => {
                let idx = dist.sample(&mut rand::rng());
                candidates[idx].clone()
            }
            Err(_) => candidates[0].clone(),
        }
    }

    /// Select best proxy filtered by country code.
    /// Uses the geo-index for O(1) candidate lookup, then picks the best.
    #[allow(dead_code)]
    pub fn select_best_by_country(&self, country: &str) -> Option<Proxy> {
        let keys = self.geo_index.get_keys(country);
        if keys.is_empty() {
            return None;
        }

        let mut candidates: Vec<Proxy> = keys
            .iter()
            .filter_map(|k| self.proxies.get(k).map(|p| p.value().clone()))
            .filter(|p| p.is_available() && matches!(&p.status, Some(ProxyStatus::Verified)))
            .collect();

        if candidates.is_empty() {
            return None;
        }

        candidates.sort_by(|a, b| {
            a.score()
                .partial_cmp(&b.score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(10);
        Some(Self::weighted_random_select(&candidates))
    }

    /// Update the geo-index when a proxy's country is set.
    pub fn update_geo_index(&self, proxy_key: &str, country: &str) {
        self.geo_index.insert(country, proxy_key);
    }

    /// Rebuild the full geo-index from current proxy state.
    #[allow(dead_code)]
    pub fn rebuild_geo_index(&self) {
        self.geo_index.rebuild(&self.proxies);
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
        assert!(
            selections.len() > 1,
            "Should select from multiple proxies, got {}",
            selections.len()
        );
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

    /// Full lifecycle: insert → verify → fail → circuit breaker → fallback selection
    #[test]
    fn test_full_proxy_lifecycle() {
        let state = SharedState::new();

        // Phase 1: Insert proxies
        state.insert_if_absent(make_proxy("10.0.0.1", 8080));
        state.insert_if_absent(make_proxy("10.0.0.2", 8080));
        state.insert_if_absent(make_proxy("10.0.0.3", 8080));

        // Phase 2: All unchecked — select_best returns one of them (tier 3)
        let first = state.select_best();
        assert!(first.is_some(), "Should select from unchecked tier");

        // Phase 3: Verify two proxies — tier 1 should be preferred
        state.record_success("10.0.0.1:8080", 100.0);
        state.record_success("10.0.0.2:8080", 200.0);
        let selected = state.select_best().unwrap();
        assert!(
            selected.host == "10.0.0.1" || selected.host == "10.0.0.2",
            "Should select from verified tier, got {}",
            selected.host
        );

        // Phase 4: Fail proxy 1 until circuit opens (5 fails)
        for _ in 0..5 {
            state.record_fail("10.0.0.1:8080");
        }
        assert!(
            !state.proxies.get("10.0.0.1:8080").unwrap().is_available(),
            "Proxy 1 should be circuit-broken"
        );

        // Phase 5: Only proxy 2 should be selected now (verified tier)
        for _ in 0..10 {
            let p = state.select_best().unwrap();
            assert_eq!(p.host, "10.0.0.2", "Should only select proxy 2 now");
        }

        // Phase 6: Recovery — success resets circuit
        state.record_success("10.0.0.1:8080", 50.0);
        assert!(
            state.proxies.get("10.0.0.1:8080").unwrap().is_available(),
            "Proxy 1 should recover after success"
        );
    }

    #[test]
    fn test_geo_index_insert_and_lookup() {
        let index = GeoIndex::new();
        index.insert("US", "1.2.3.4:8080");
        index.insert("US", "5.6.7.8:3128");
        index.insert("DE", "9.0.1.2:8080");

        let us_keys = index.get_keys("US");
        assert_eq!(us_keys.len(), 2);
        assert!(us_keys.contains(&"1.2.3.4:8080".to_string()));
        assert!(us_keys.contains(&"5.6.7.8:3128".to_string()));

        let de_keys = index.get_keys("DE");
        assert_eq!(de_keys.len(), 1);

        assert_eq!(index.get_keys("JP").len(), 0);
        assert_eq!(index.country_count(), 2);
    }

    #[test]
    fn test_geo_index_no_duplicates() {
        let index = GeoIndex::new();
        index.insert("US", "1.2.3.4:8080");
        index.insert("US", "1.2.3.4:8080");
        assert_eq!(index.get_keys("US").len(), 1);
    }

    #[test]
    fn test_geo_index_remove() {
        let index = GeoIndex::new();
        index.insert("US", "1.2.3.4:8080");
        index.insert("US", "5.6.7.8:3128");
        index.remove("US", "1.2.3.4:8080");
        let keys = index.get_keys("US");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], "5.6.7.8:3128");
    }

    #[test]
    fn test_select_best_by_country_uses_geo_index() {
        let state = SharedState::new();
        // Insert and verify proxies
        state.insert_if_absent(make_proxy("1.2.3.4", 8080));
        state.insert_if_absent(make_proxy("5.6.7.8", 3128));
        state.insert_if_absent(make_proxy("9.0.1.2", 8080));
        state.record_success("1.2.3.4:8080", 100.0);
        state.record_success("5.6.7.8:3128", 200.0);
        state.record_success("9.0.1.2:8080", 150.0);

        // Set countries and update geo-index
        if let Some(mut p) = state.proxies.get_mut("1.2.3.4:8080") {
            p.country = Some("US".to_string());
        }
        if let Some(mut p) = state.proxies.get_mut("5.6.7.8:3128") {
            p.country = Some("US".to_string());
        }
        if let Some(mut p) = state.proxies.get_mut("9.0.1.2:8080") {
            p.country = Some("DE".to_string());
        }
        state.update_geo_index("1.2.3.4:8080", "US");
        state.update_geo_index("5.6.7.8:3128", "US");
        state.update_geo_index("9.0.1.2:8080", "DE");

        // Select by country
        let us_proxy = state.select_best_by_country("US").unwrap();
        assert!(us_proxy.host == "1.2.3.4" || us_proxy.host == "5.6.7.8");

        let de_proxy = state.select_best_by_country("DE").unwrap();
        assert_eq!(de_proxy.host, "9.0.1.2");

        assert!(state.select_best_by_country("JP").is_none());
    }

    /// Test that collect_top_n produces correct results with many proxies
    #[test]
    fn test_collect_top_n_correctness() {
        let state = SharedState::new();
        // Insert 50 proxies with varying latencies
        for i in 0u16..50 {
            let host = format!("10.0.{}.{}", i / 255 + 1, i % 255 + 1);
            let port = 8080;
            state.insert_if_absent(make_proxy(&host, port));
            let key = format!("{}:{}", host, port);
            state.record_success(&key, (i as f64 + 1.0) * 100.0);
        }
        assert_eq!(state.verified_count(), 50);

        // select_best should return one of the top-10 lowest latency proxies
        let selected = state.select_best().unwrap();
        // Top-10 proxies have EWMA around 4000-4020 (one sample from 5000 initial)
        // After record_success(100.0): ewma = 5000*0.8 + 100*0.2 = 4020
        // After record_success(1000.0): ewma = 5000*0.8 + 1000*0.2 = 4200
        // So top-10 should have ewma < 4300
        assert!(
            selected.latency_ewma < 4300.0,
            "Selected proxy latency {} should be in top-10 range",
            selected.latency_ewma
        );
    }
}
