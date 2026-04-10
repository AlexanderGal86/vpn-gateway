use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Proxy protocol type
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Socks5,
    Https,
}

/// Proxy liveness status (not serialized — runtime only)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyStatus {
    /// Verified by health check
    Verified,
    /// Loaded from state.json, not yet re-checked
    PresumedAlive,
    /// Just loaded from source, never checked
    Unchecked,
    /// Failed health check, temporarily disabled
    Failed,
}

/// A single upstream proxy server.
///
/// Split into two parts:
/// - Serializable fields (persisted to state.json)
/// - Runtime-only fields (reconstructed on load)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proxy {
    // === Identity ===
    pub host: String,
    pub port: u16,
    pub protocol: Protocol,

    // === Scoring (persisted) ===
    /// Exponentially weighted moving average of latency in ms
    pub latency_ewma: f64,
    pub success_count: u64,
    pub fail_count: u64,
    pub consecutive_fails: u32,

    // === Timestamps (persisted as DateTime for serde) ===
    pub last_check: Option<DateTime<Utc>>,
    pub last_success: Option<DateTime<Utc>>,
    pub last_fail: Option<DateTime<Utc>>,

    // === Classification ===
    pub country: Option<String>,
    pub manual: bool,
    pub priority_boost: f64,

    // === TLS validation ===
    /// TLS validation result: None = not checked, Some(true) = clean, Some(false) = MITM
    #[serde(default)]
    pub tls_clean: Option<bool>,

    // === Runtime-only (skip serialization) ===
    #[serde(skip)]
    pub status: Option<ProxyStatus>,
    /// Instant when circuit breaker disables this proxy (runtime only)
    #[serde(skip)]
    pub circuit_open_until: Option<Instant>,
}

impl Proxy {
    pub fn new(host: String, port: u16, protocol: Protocol) -> Self {
        Self {
            host,
            port,
            protocol,
            latency_ewma: 5000.0, // pessimistic initial
            success_count: 0,
            fail_count: 0,
            consecutive_fails: 0,
            last_check: None,
            last_success: None,
            last_fail: None,
            country: None,
            manual: false,
            priority_boost: 0.0,
            tls_clean: None,
            status: Some(ProxyStatus::Unchecked),
            circuit_open_until: None,
        }
    }

    /// Unique key for DashMap
    pub fn key(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Update latency using EWMA (α = 0.2)
    pub fn record_success(&mut self, latency_ms: f64) {
        let latency_ms = if latency_ms.is_finite() {
            latency_ms.clamp(0.0, 60_000.0)
        } else {
            5000.0
        };
        self.latency_ewma = self.latency_ewma * 0.8 + latency_ms * 0.2;
        self.success_count += 1;
        self.consecutive_fails = 0;
        self.last_success = Some(Utc::now());
        self.last_check = Some(Utc::now());
        self.status = Some(ProxyStatus::Verified);
        self.circuit_open_until = None;
    }

    /// Record a failed connection attempt
    pub fn record_fail(&mut self) {
        self.fail_count += 1;
        self.consecutive_fails += 1;
        self.last_fail = Some(Utc::now());
        self.last_check = Some(Utc::now());

        // Circuit breaker: escalating disable duration
        let disable_secs = match self.consecutive_fails {
            0..=4 => 0,
            5..=9 => 60,
            10..=19 => 300,
            20..=49 => 3600,
            _ => {
                self.status = Some(ProxyStatus::Failed);
                return; // permanently failed until next source refresh
            }
        };

        if disable_secs > 0 {
            self.circuit_open_until =
                Some(Instant::now() + std::time::Duration::from_secs(disable_secs));
            self.status = Some(ProxyStatus::Failed);
        }
    }

    /// Is this proxy available for selection?
    pub fn is_available(&self) -> bool {
        match self.circuit_open_until {
            Some(until) => Instant::now() >= until, // circuit breaker expired
            None => true,
        }
    }

    /// Lower score = better proxy
    pub fn score(&self) -> f64 {
        let mut s = self.latency_ewma;

        // Penalty for recent failures
        s += self.consecutive_fails as f64 * 50.0;

        // Bonus for manual proxies
        s -= self.priority_boost;

        // Small bonus for recently successful proxies
        if let Some(last) = self.last_success {
            let age_secs = (Utc::now() - last).num_seconds().max(0) as f64;
            if age_secs < 300.0 {
                s -= 100.0; // recently worked = bonus
            }
        }

        s
    }

    /// Mark as presumed alive (loaded from state.json)
    pub fn mark_presumed_alive(&mut self) {
        self.status = Some(ProxyStatus::PresumedAlive);
        self.circuit_open_until = None;
        self.consecutive_fails = 0; // give it a fresh chance
    }

    /// Socket address string
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

impl Default for Proxy {
    fn default() -> Self {
        Self::new(String::new(), 0, Protocol::Http)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ewma_convergence() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        assert_eq!(p.latency_ewma, 5000.0);

        // Simulate 10 successful checks at 200ms
        for _ in 0..10 {
            p.record_success(200.0);
        }
        // EWMA should converge toward 200
        assert!(
            p.latency_ewma < 1000.0,
            "EWMA should converge: {}",
            p.latency_ewma
        );
    }

    #[test]
    fn test_circuit_breaker() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);

        for _ in 0..4 {
            p.record_fail();
            assert!(
                p.is_available(),
                "Should still be available after {} fails",
                p.consecutive_fails
            );
        }

        p.record_fail(); // 5th fail
        assert!(!p.is_available(), "Should be disabled after 5 fails");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Socks5);
        p.record_success(150.0);

        let json = serde_json::to_string(&p).unwrap();
        let p2: Proxy = serde_json::from_str(&json).unwrap();

        assert_eq!(p2.host, "1.2.3.4");
        assert_eq!(p2.port, 8080);
        assert_eq!(p2.protocol, Protocol::Socks5);
        assert!(p2.circuit_open_until.is_none()); // runtime field not serialized
    }

    #[test]
    fn test_score_lower_is_better() {
        let mut fast = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        let mut slow = Proxy::new("5.6.7.8".into(), 3128, Protocol::Http);
        fast.record_success(50.0);
        slow.record_success(500.0);
        assert!(
            fast.score() < slow.score(),
            "Fast proxy should have lower score"
        );
    }

    #[test]
    fn test_score_penalizes_consecutive_fails() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        let base_score = p.score();
        p.record_fail();
        assert!(p.score() > base_score, "Score should increase after fail");
    }

    #[test]
    fn test_circuit_breaker_escalation() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        for _ in 0..4 {
            p.record_fail();
        }
        assert!(p.is_available(), "Should be available after 4 fails");

        p.record_fail(); // 5th
        assert!(!p.is_available(), "Should be disabled after 5 fails");

        // Wait for circuit to expire (simulate by clearing)
        p.circuit_open_until = None;
        assert!(
            p.is_available(),
            "Should be available after circuit expires"
        );
    }

    #[test]
    fn test_permanent_failure_after_50_fails() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        for _ in 0..50 {
            p.record_fail();
        }
        assert_eq!(p.status, Some(ProxyStatus::Failed));
    }

    #[test]
    fn test_recovery_after_success() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        p.record_fail();
        p.record_fail();
        p.record_fail();
        p.record_fail();
        assert!(p.is_available(), "Should be available after 4 fails");
        p.record_success(100.0);
        assert!(p.is_available(), "Should be available after success");
        assert_eq!(p.consecutive_fails, 0, "Consecutive fails should reset");
    }

    #[test]
    fn test_mark_presumed_alive() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        p.record_fail();
        p.mark_presumed_alive();
        assert_eq!(p.status, Some(ProxyStatus::PresumedAlive));
        assert!(p.is_available());
        assert_eq!(p.consecutive_fails, 0);
    }

    #[test]
    fn test_ewma_alpha_is_0_2() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        p.record_success(100.0);
        // EWMA = 0.2 * 100 + 0.8 * 5000 = 20 + 4000 = 4020
        assert!(
            (p.latency_ewma - 4020.0).abs() < 1.0,
            "EWMA should be ~4020, got {}",
            p.latency_ewma
        );
    }

    #[test]
    fn test_ewma_handles_nan_and_infinity() {
        let mut p = Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        p.record_success(f64::NAN);
        assert!(
            p.latency_ewma.is_finite(),
            "EWMA must stay finite after NaN input"
        );
        p.record_success(f64::INFINITY);
        assert!(
            p.latency_ewma.is_finite(),
            "EWMA must stay finite after infinity input"
        );
        p.record_success(-100.0);
        assert!(
            p.latency_ewma.is_finite(),
            "EWMA must stay finite after negative input"
        );
    }

    #[test]
    fn test_addr_format() {
        let p = Proxy::new("10.0.0.1".into(), 8080, Protocol::Http);
        assert_eq!(p.addr(), "10.0.0.1:8080");
    }

    #[test]
    fn test_protocol_display() {
        assert_eq!(format!("{:?}", Protocol::Http), "Http");
        assert_eq!(format!("{:?}", Protocol::Socks5), "Socks5");
    }
}
