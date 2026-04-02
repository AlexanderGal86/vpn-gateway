use crate::pool::proxy::Proxy;
use crate::pool::state::SharedState;
use chrono::Utc;
use std::path::Path;
use std::time::Duration;

const STATE_FILE: &str = "data/state.json";

// ── Private helpers ──────────────────────────────────────────────────────────

/// Inner implementation: load proxies from an arbitrary path.
/// Returns the number of proxies loaded.
async fn load_state_from(state: &SharedState, path: &Path) -> usize {
    if !path.exists() {
        tracing::info!("No state file found at {}, starting fresh", path.display());
        return 0;
    }

    match tokio::fs::read_to_string(path).await {
        Ok(data) => match serde_json::from_str::<Vec<Proxy>>(&data) {
            Ok(proxies) => {
                let now = Utc::now();
                let mut loaded = 0;
                let mut presumed_alive = 0;

                for mut proxy in proxies {
                    // If this proxy succeeded in the last hour, mark as presumed alive
                    let is_recent = proxy
                        .last_success
                        .map(|t| (now - t).num_hours() < 1)
                        .unwrap_or(false);

                    if is_recent {
                        proxy.mark_presumed_alive();
                        presumed_alive += 1;
                    }

                    state.insert_if_absent(proxy);
                    loaded += 1;
                }

                tracing::info!(
                    "Loaded {} proxies from {} ({} presumed alive)",
                    loaded,
                    path.display(),
                    presumed_alive
                );

                // If we have presumed-alive proxies, signal that pool is ready
                if presumed_alive > 0 {
                    state.first_ready.notify_waiters();
                }

                loaded
            }
            Err(e) => {
                tracing::warn!("Failed to parse {}: {}", path.display(), e);
                0
            }
        },
        Err(e) => {
            tracing::warn!("Failed to read {}: {}", path.display(), e);
            0
        }
    }
}

/// Inner implementation: save proxies to an arbitrary path (atomic write).
async fn save_state_to(state: &SharedState, path: &Path) {
    let proxies = state.all_proxies();

    // Only save proxies that have been checked at least once
    let to_save: Vec<_> = proxies
        .into_iter()
        .filter(|p| p.last_check.is_some())
        .collect();

    match serde_json::to_string_pretty(&to_save) {
        Ok(json) => {
            let tmp_path = format!("{}.tmp", path.display());

            // Ensure the directory exists
            if let Some(parent) = path.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    tracing::error!("Failed to create directory {}: {}", parent.display(), e);
                    return;
                }
            }

            // Atomic write: write to .tmp, then rename
            if let Err(e) = tokio::fs::write(&tmp_path, &json).await {
                tracing::error!("Failed to write {}: {}", tmp_path, e);
                return;
            }
            if let Err(e) = tokio::fs::rename(&tmp_path, path).await {
                tracing::error!("Failed to rename {} -> {}: {}", tmp_path, path.display(), e);
                return;
            }

            tracing::debug!("Saved {} proxies to {}", to_save.len(), path.display());
        }
        Err(e) => {
            tracing::error!("Failed to serialize state: {}", e);
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Load proxies from state.json on startup.
/// Proxies that succeeded within the last hour are marked PresumedAlive.
pub async fn load_state(state: &SharedState) -> usize {
    load_state_from(state, Path::new(STATE_FILE)).await
}

/// Save current proxy pool to state.json (atomic write).
pub async fn save_state(state: &SharedState) {
    tracing::info!("Saving state to {}", STATE_FILE);
    save_state_to(state, Path::new(STATE_FILE)).await
}

/// Background loop: save state every `interval_secs`.
pub async fn run_persistence_loop(state: SharedState, interval_secs: u64) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));

    loop {
        ticker.tick().await;
        save_state(&state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::proxy::{Protocol, ProxyStatus};
    use chrono::Utc;
    use std::sync::atomic::Ordering;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("vpn_gateway_test_{}_{}", name, std::process::id()))
    }

    fn make_proxy(host: &str, port: u16) -> Proxy {
        Proxy::new(host.to_string(), port, Protocol::Http)
    }

    // ── 1. load from a missing file returns 0 ────────────────────────────────

    #[tokio::test]
    async fn test_load_state_missing_file() {
        let state = SharedState::new();
        let path = temp_path("missing");
        // Ensure the file does not exist
        let _ = tokio::fs::remove_file(&path).await;

        let count = load_state_from(&state, &path).await;
        assert_eq!(count, 0);
        assert_eq!(state.total_count(), 0);
    }

    // ── 2. save → load round-trip preserves proxy data ───────────────────────

    #[tokio::test]
    async fn test_save_and_load_roundtrip() {
        let path = temp_path("roundtrip.json");

        // Build a state with one checked proxy
        let state_a = SharedState::new();
        let mut p = make_proxy("1.2.3.4", 8080);
        p.record_success(100.0); // sets last_check so it gets saved
        state_a.insert_if_absent(p);

        save_state_to(&state_a, &path).await;
        assert!(path.exists(), "state file should have been created");

        // Load into a fresh state
        let state_b = SharedState::new();
        let loaded = load_state_from(&state_b, &path).await;

        assert_eq!(loaded, 1);
        assert!(state_b.proxies.contains_key("1.2.3.4:8080"));

        // Clean up
        let _ = tokio::fs::remove_file(&path).await;
    }

    // ── 3. save with empty pool writes an empty JSON array ───────────────────

    #[tokio::test]
    async fn test_save_state_empty_pool() {
        let path = temp_path("empty.json");
        let state = SharedState::new();

        save_state_to(&state, &path).await;

        // File may or may not exist; if it does, it should contain "[]"
        if path.exists() {
            let contents = tokio::fs::read_to_string(&path).await.unwrap();
            let parsed: Vec<serde_json::Value> = serde_json::from_str(&contents).unwrap();
            assert!(parsed.is_empty());
            let _ = tokio::fs::remove_file(&path).await;
        }
        // An empty pool with no checked proxies produces an empty to_save vec,
        // so the file is still written (as "[]").
    }

    // ── 4. invalid JSON returns 0 and does not panic ─────────────────────────

    #[tokio::test]
    async fn test_load_state_invalid_json() {
        let path = temp_path("bad.json");
        tokio::fs::write(&path, b"not valid json at all!!!").await.unwrap();

        let state = SharedState::new();
        let count = load_state_from(&state, &path).await;
        assert_eq!(count, 0);
        assert_eq!(state.total_count(), 0);

        let _ = tokio::fs::remove_file(&path).await;
    }

    // ── 5. presumed-alive logic: recent success → PresumedAlive ──────────────

    #[tokio::test]
    async fn test_load_state_recent_success_marks_presumed_alive() {
        let path = temp_path("recent.json");

        // Create a proxy with last_success = now (within 1 hour)
        let mut p = make_proxy("5.6.7.8", 3128);
        p.record_success(200.0); // sets last_success and last_check
        let json = serde_json::to_string(&vec![p]).unwrap();
        tokio::fs::write(&path, json.as_bytes()).await.unwrap();

        let state = SharedState::new();
        let count = load_state_from(&state, &path).await;

        assert_eq!(count, 1);
        let loaded = state.proxies.get("5.6.7.8:3128").unwrap();
        assert_eq!(
            loaded.status,
            Some(ProxyStatus::PresumedAlive),
            "Recent proxy should be PresumedAlive"
        );

        let _ = tokio::fs::remove_file(&path).await;
    }

    // ── 6. old success (>1 h ago) → not marked PresumedAlive ─────────────────

    #[tokio::test]
    async fn test_load_state_old_success_not_presumed_alive() {
        let path = temp_path("old.json");

        // Create a proxy whose last_success is 2 hours ago
        let mut p = make_proxy("9.9.9.9", 9999);
        p.record_success(300.0);
        p.last_success = Some(Utc::now() - chrono::Duration::hours(2));
        p.last_check = p.last_success;
        let json = serde_json::to_string(&vec![p]).unwrap();
        tokio::fs::write(&path, json.as_bytes()).await.unwrap();

        let state = SharedState::new();
        let count = load_state_from(&state, &path).await;

        assert_eq!(count, 1);
        let loaded = state.proxies.get("9.9.9.9:9999").unwrap();
        assert_ne!(
            loaded.status,
            Some(ProxyStatus::PresumedAlive),
            "Old proxy should NOT be PresumedAlive"
        );

        let _ = tokio::fs::remove_file(&path).await;
    }

    // ── 7. unchecked proxies (no last_check) are NOT written to disk ─────────

    #[tokio::test]
    async fn test_save_skips_unchecked_proxies() {
        let path = temp_path("unchecked.json");

        let state = SharedState::new();
        // Insert one unchecked (never had last_check) and one checked proxy
        state.insert_if_absent(make_proxy("1.1.1.1", 80));
        let mut checked = make_proxy("2.2.2.2", 80);
        checked.record_success(50.0);
        state.insert_if_absent(checked);

        save_state_to(&state, &path).await;

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let saved: Vec<serde_json::Value> = serde_json::from_str(&contents).unwrap();
        assert_eq!(saved.len(), 1, "Only the checked proxy should be saved");
        assert_eq!(saved[0]["host"], "2.2.2.2");

        let _ = tokio::fs::remove_file(&path).await;
    }

    // ── 8. save creates parent directories automatically ──────────────────────

    #[tokio::test]
    async fn test_save_creates_parent_directories() {
        let base = temp_path("nested");
        let path = base.join("sub").join("state.json");

        let state = SharedState::new();
        let mut p = make_proxy("3.3.3.3", 3333);
        p.record_success(75.0);
        state.insert_if_absent(p);

        save_state_to(&state, &path).await;
        assert!(path.exists(), "File should be created even if parent dirs were missing");

        // Clean up
        let _ = tokio::fs::remove_file(&path).await;
        let _ = tokio::fs::remove_dir(path.parent().unwrap()).await;
        let _ = tokio::fs::remove_dir(&base).await;
    }

    // ── 9. pool-full guard is respected during load ───────────────────────────

    #[tokio::test]
    async fn test_load_respects_max_proxies_limit() {
        let path = temp_path("maxprox.json");

        // Write 5 proxies to disk, all with last_check set
        let mut proxies = Vec::new();
        for i in 0u8..5 {
            let mut p = make_proxy(&format!("10.0.0.{}", i + 1), 8080);
            p.record_success(100.0);
            proxies.push(p);
        }
        let json = serde_json::to_string(&proxies).unwrap();
        tokio::fs::write(&path, json.as_bytes()).await.unwrap();

        // Cap pool at 3
        let state = SharedState::new();
        state.max_proxies.store(3, Ordering::Relaxed);

        let count = load_state_from(&state, &path).await;

        // 5 were in the file; all 5 are "loaded" (counter) but only 3 inserted
        assert_eq!(count, 5, "load count reflects file contents");
        assert_eq!(state.total_count(), 3, "pool should be capped at max_proxies");

        let _ = tokio::fs::remove_file(&path).await;
    }
}
