use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::hint::black_box;
use vpn_gateway::pool::proxy::{Protocol, Proxy};
use vpn_gateway::pool::state::SharedState;

/// Create a pool of N verified proxies with varying latencies.
fn setup_pool(n: usize) -> SharedState {
    let state = SharedState::new();
    for i in 0..n {
        let host = format!(
            "10.{}.{}.{}",
            (i >> 16) & 0xFF,
            (i >> 8) & 0xFF,
            (i & 0xFF) + 1
        );
        let proxy = Proxy::new(host.clone(), 8080, Protocol::Http);
        state.insert_if_absent(proxy);
        state.record_success(&format!("{}:8080", host), (i as f64 + 1.0) * 10.0);
    }
    state
}

fn bench_collect_top_n(c: &mut Criterion) {
    let mut group = c.benchmark_group("collect_top_n");
    for size in [100, 500, 1000, 5000] {
        let state = setup_pool(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &state, |b, state| {
            b.iter(|| {
                black_box(state.select_best());
            });
        });
    }
    group.finish();
}

fn bench_ewma_scoring(c: &mut Criterion) {
    let mut group = c.benchmark_group("ewma_scoring");

    group.bench_function("record_success", |b| {
        let mut proxy = Proxy::new("10.0.0.1".to_string(), 8080, Protocol::Http);
        b.iter(|| {
            proxy.record_success(black_box(150.0));
        });
    });

    group.bench_function("record_fail", |b| {
        let mut proxy = Proxy::new("10.0.0.1".to_string(), 8080, Protocol::Http);
        b.iter(|| {
            proxy.record_fail();
            // Reset to avoid permanent failure
            if proxy.consecutive_fails >= 50 {
                proxy.consecutive_fails = 0;
                proxy.circuit_open_until = None;
            }
        });
    });

    group.bench_function("score", |b| {
        let mut proxy = Proxy::new("10.0.0.1".to_string(), 8080, Protocol::Http);
        proxy.record_success(150.0);
        b.iter(|| {
            black_box(proxy.score());
        });
    });

    group.bench_function("is_available", |b| {
        let mut proxy = Proxy::new("10.0.0.1".to_string(), 8080, Protocol::Http);
        proxy.record_success(100.0);
        b.iter(|| {
            black_box(proxy.is_available());
        });
    });

    group.finish();
}

fn bench_insert_if_absent(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_if_absent");

    group.bench_function("new_proxy", |b| {
        let state = SharedState::new();
        let mut i = 0u64;
        b.iter(|| {
            let proxy = Proxy::new(
                format!(
                    "10.{}.{}.{}",
                    (i >> 16) & 0xFF,
                    (i >> 8) & 0xFF,
                    (i & 0xFF) + 1
                ),
                8080,
                Protocol::Http,
            );
            black_box(state.insert_if_absent(proxy));
            i += 1;
        });
    });

    group.bench_function("duplicate", |b| {
        let state = SharedState::new();
        let proxy = Proxy::new("10.0.0.1".to_string(), 8080, Protocol::Http);
        state.insert_if_absent(proxy);
        b.iter(|| {
            let proxy = Proxy::new("10.0.0.1".to_string(), 8080, Protocol::Http);
            black_box(state.insert_if_absent(proxy));
        });
    });

    group.finish();
}

fn bench_select_best_by_country(c: &mut Criterion) {
    let mut group = c.benchmark_group("select_best_by_country");

    let state = setup_pool(1000);
    // Assign countries to proxies and update geo-index
    let countries = ["US", "DE", "NL", "JP", "GB"];
    for (i, entry) in state.proxies.iter().enumerate() {
        let country = countries[i % countries.len()];
        let key = entry.key().clone();
        drop(entry);
        if let Some(mut p) = state.proxies.get_mut(&key) {
            p.country = Some(country.to_string());
        }
        state.update_geo_index(&key, country);
    }

    group.bench_function("with_geo_index", |b| {
        b.iter(|| {
            black_box(state.select_best_by_country("US"));
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_collect_top_n,
    bench_ewma_scoring,
    bench_insert_if_absent,
    bench_select_best_by_country,
);
criterion_main!(benches);
