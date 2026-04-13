//! Metrics - System metrics collection and Prometheus format

use crate::pool::state::SharedState;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct Metrics {
    pub proxies_total: usize,
    pub proxies_alive: usize,
    pub proxies_fast_pool: usize,
    pub proxies_banned: usize,
    pub avg_latency_ms: f64,
    pub connections_active: usize,
    pub proxies_by_country: HashMap<String, usize>,
    pub success_rate: f64,
    pub circuit_breaker_trips: usize,
    pub warm_pool_proxies: usize,
    pub warm_pool_hits: u64,
    pub warm_pool_misses: u64,
    pub sticky_sessions_active: usize,
    pub avg_uptime_secs: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            proxies_total: 0,
            proxies_alive: 0,
            proxies_fast_pool: 0,
            proxies_banned: 0,
            avg_latency_ms: 0.0,
            connections_active: 0,
            proxies_by_country: HashMap::new(),
            success_rate: 0.0,
            circuit_breaker_trips: 0,
            warm_pool_proxies: 0,
            warm_pool_hits: 0,
            warm_pool_misses: 0,
            sticky_sessions_active: 0,
            avg_uptime_secs: 0.0,
        }
    }
}

pub fn collect(state: &SharedState) -> Metrics {
    let proxies: Vec<_> = state.proxies.iter().map(|p| p.value().clone()).collect();

    let total = proxies.len();
    let alive = proxies.iter().filter(|p| p.is_available()).count();
    let banned = state.banned.len();

    let avg_latency = if alive > 0 {
        proxies
            .iter()
            .filter(|p| p.is_available())
            .map(|p| p.latency_ewma)
            .sum::<f64>()
            / alive as f64
    } else {
        0.0
    };

    let mut by_country: HashMap<String, usize> = HashMap::new();
    for proxy in &proxies {
        if let Some(ref country) = proxy.country {
            *by_country.entry(country.clone()).or_insert(0) += 1;
        }
    }

    let (total_successes, total_fails) = proxies.iter().fold((0u64, 0u64), |(s, f), p| {
        (s + p.success_count, f + p.fail_count)
    });

    let success_rate = if total_successes + total_fails > 0 {
        total_successes as f64 / (total_successes + total_fails) as f64
    } else {
        0.0
    };

    let circuit_trips = proxies
        .iter()
        .filter(|p| p.circuit_open_until.is_some())
        .count();

    // Warm pool stats
    let warm_stats = state.warm_pool.stats();

    // Sticky sessions count
    let sticky_count = state.sticky_sessions.count();

    // Average uptime of top proxies (verified, with uptime data)
    let uptime_proxies: Vec<f64> = proxies
        .iter()
        .filter(|p| p.is_available() && p.uptime_max_secs > 0)
        .map(|p| {
            let current = p
                .uptime_streak_start
                .map(|s| s.elapsed().as_secs() as f64)
                .unwrap_or(0.0);
            current.max(p.uptime_max_secs as f64)
        })
        .collect();
    let avg_uptime = if !uptime_proxies.is_empty() {
        uptime_proxies.iter().sum::<f64>() / uptime_proxies.len() as f64
    } else {
        0.0
    };

    Metrics {
        proxies_total: total,
        proxies_alive: alive,
        proxies_fast_pool: 0,
        proxies_banned: banned,
        avg_latency_ms: avg_latency,
        connections_active: state
            .active_connections
            .load(std::sync::atomic::Ordering::Relaxed) as usize,
        proxies_by_country: by_country,
        success_rate,
        circuit_breaker_trips: circuit_trips,
        warm_pool_proxies: warm_stats.proxies_tracked,
        warm_pool_hits: warm_stats.hits,
        warm_pool_misses: warm_stats.misses,
        sticky_sessions_active: sticky_count,
        avg_uptime_secs: avg_uptime,
    }
}

pub fn format_prometheus(m: &Metrics) -> String {
    let mut lines = Vec::new();

    lines.push("# HELP vpn_proxies_total Total number of proxies".to_string());
    lines.push("# TYPE vpn_proxies_total gauge".to_string());
    lines.push(format!("vpn_proxies_total {}", m.proxies_total));

    lines.push("# HELP vpn_proxies_alive Number of alive proxies".to_string());
    lines.push("# TYPE vpn_proxies_alive gauge".to_string());
    lines.push(format!("vpn_proxies_alive {}", m.proxies_alive));

    lines.push("# HELP vpn_proxies_banned Number of banned proxies".to_string());
    lines.push("# TYPE vpn_proxies_banned gauge".to_string());
    lines.push(format!("vpn_proxies_banned {}", m.proxies_banned));

    lines.push("# HELP vpn_avg_latency_ms Average proxy latency in milliseconds".to_string());
    lines.push("# TYPE vpn_avg_latency_ms gauge".to_string());
    lines.push(format!("vpn_avg_latency_ms {:.2}", m.avg_latency_ms));

    lines.push("# HELP vpn_connections_active Active TCP connections".to_string());
    lines.push("# TYPE vpn_connections_active gauge".to_string());
    lines.push(format!("vpn_connections_active {}", m.connections_active));

    lines.push("# HELP vpn_success_rate Overall success rate".to_string());
    lines.push("# TYPE vpn_success_rate gauge".to_string());
    lines.push(format!("vpn_success_rate {:.4}", m.success_rate));

    lines
        .push("# HELP vpn_circuit_breaker_trips Number of circuit breaker activations".to_string());
    lines.push("# TYPE vpn_circuit_breaker_trips gauge".to_string());
    lines.push(format!(
        "vpn_circuit_breaker_trips {}",
        m.circuit_breaker_trips
    ));

    lines.push("# HELP vpn_proxies_by_country Proxies by country".to_string());
    lines.push("# TYPE vpn_proxies_by_country gauge".to_string());
    for (country, count) in &m.proxies_by_country {
        lines.push(format!(
            "vpn_proxies_by_country{{country=\"{}\"}} {}",
            country, count
        ));
    }

    lines.push("# HELP vpn_warm_pool_proxies Number of proxies with warm connections".to_string());
    lines.push("# TYPE vpn_warm_pool_proxies gauge".to_string());
    lines.push(format!("vpn_warm_pool_proxies {}", m.warm_pool_proxies));

    lines.push("# HELP vpn_warm_pool_hits Warm pool connection hits".to_string());
    lines.push("# TYPE vpn_warm_pool_hits counter".to_string());
    lines.push(format!("vpn_warm_pool_hits {}", m.warm_pool_hits));

    lines.push("# HELP vpn_warm_pool_misses Warm pool connection misses".to_string());
    lines.push("# TYPE vpn_warm_pool_misses counter".to_string());
    lines.push(format!("vpn_warm_pool_misses {}", m.warm_pool_misses));

    lines.push("# HELP vpn_sticky_sessions_active Active sticky sessions".to_string());
    lines.push("# TYPE vpn_sticky_sessions_active gauge".to_string());
    lines.push(format!(
        "vpn_sticky_sessions_active {}",
        m.sticky_sessions_active
    ));

    lines.push("# HELP vpn_avg_uptime_secs Average proxy uptime in seconds".to_string());
    lines.push("# TYPE vpn_avg_uptime_secs gauge".to_string());
    lines.push(format!("vpn_avg_uptime_secs {:.0}", m.avg_uptime_secs));

    lines.join("\n")
}
