use crate::pool::proxy::Proxy;
use crate::pool::state::SharedState;
use chrono::Utc;
use std::path::Path;
use std::time::Duration;

const STATE_FILE: &str = "data/state.json";

/// Load proxies from state.json on startup.
/// Proxies that succeeded within the last hour are marked PresumedAlive.
pub async fn load_state(state: &SharedState) -> usize {
    let path = Path::new(STATE_FILE);
    if !path.exists() {
        tracing::info!("No state.json found, starting fresh");
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
                    "Loaded {} proxies from state.json ({} presumed alive)",
                    loaded,
                    presumed_alive
                );

                // If we have presumed-alive proxies, signal that pool is ready
                if presumed_alive > 0 {
                    state.first_ready.notify_waiters();
                }

                loaded
            }
            Err(e) => {
                tracing::warn!("Failed to parse state.json: {}", e);
                0
            }
        },
        Err(e) => {
            tracing::warn!("Failed to read state.json: {}", e);
            0
        }
    }
}

/// Save current proxy pool to state.json (atomic write).
pub async fn save_state(state: &SharedState) {
    tracing::info!("STATE_FILE: {}", STATE_FILE);
    let proxies = state.all_proxies();

    // Only save proxies that have been checked at least once
    let to_save: Vec<_> = proxies
        .into_iter()
        .filter(|p| p.last_check.is_some())
        .collect();

    match serde_json::to_string_pretty(&to_save) {
        Ok(json) => {
            let tmp_path = format!("{}.tmp", STATE_FILE);
            tracing::debug!("STATE_FILE: {}, tmp_path: {}", STATE_FILE, tmp_path);

            // Ensure the directory exists
            let path = Path::new(STATE_FILE);
            tracing::debug!("Parent directory: {:?}", path.parent());
            if let Some(parent) = path.parent() {
                tracing::debug!("Creating directory: {}", parent.display());
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    tracing::error!("Failed to create directory {}: {}", parent.display(), e);
                    return;
                }
                tracing::debug!("Directory created successfully");
            }

            // Atomic write: write to .tmp, then rename
            if let Err(e) = tokio::fs::write(&tmp_path, &json).await {
                tracing::error!("Failed to write {}: {}", tmp_path, e);
                return;
            }
            if let Err(e) = tokio::fs::rename(&tmp_path, STATE_FILE).await {
                tracing::error!("Failed to rename {} -> {}: {}", tmp_path, STATE_FILE, e);
                return;
            }

            tracing::debug!("Saved {} proxies to state.json", to_save.len());
        }
        Err(e) => {
            tracing::error!("Failed to serialize state: {}", e);
        }
    }
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
    // TODO: Add integration tests with temp directory
    // These tests require a temporary directory to avoid interfering with production state.
    // Use tempfile crate or std::env::temp_dir() for isolated test files.
    //
    // Future tests needed:
    // - test_load_state_missing_file
    // - test_save_and_load_state_roundtrip
    // - test_save_state_empty_pool
    // - test_load_state_invalid_json
    // - test_load_state_presumed_alive_logic
    // - test_persistence_loop_saves_periodically
}
