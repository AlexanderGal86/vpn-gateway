mod api;
mod config;
mod pool;
mod proxy;

use config::ConfigManager;
use pool::state::SharedState;
use std::io::Write;
use std::time::Duration;
use tokio::task::JoinHandle;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing - force stdout for Docker logging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("vpn_gateway=info".parse().unwrap()),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .try_init();

    // Ensure stdout is not buffered
    std::io::stdout().flush().ok();

    tracing::info!("VPN Gateway starting...");

    // Load configuration
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config/gateway.json".to_string());
    let config_manager = ConfigManager::new(config_path.clone());
    let config = config_manager.get().await;
    
    tracing::info!("Loaded config: proxy_port={}, api_port={}", config.gateway_port, config.api_port);

    // Create shared state with config
    let state = SharedState::with_config(config.geoip_path.clone(), config.sticky_session_ttl, config.max_proxies);

    // Load GeoIP if configured
    if let Some(ref _path) = config.geoip_path {
        if let Err(e) = state.geoip.load().await {
            tracing::warn!("Failed to load GeoIP database: {}", e);
        }
    }

    // Start config hot reload watcher
    let mut config_rx = config_manager.start_watching().await;

    // =============================================
    // LEVEL 0: INSTANT — Load state.json
    // =============================================
    let loaded = pool::persistence::load_state(&state).await;
    tracing::info!("Level 0: loaded {} proxies from state.json", loaded);

    // =============================================
    // LEVEL 1: FAST PROBE (3–8 seconds)
    // =============================================
    // Fast bootstrap: load from top 3 sources
    let bootstrap_count = pool::source_manager::fast_bootstrap(&state).await;
    tracing::info!("Level 1: {} proxies from fast bootstrap", bootstrap_count);

    // Probe all loaded proxies in parallel (fast timeout)
    let to_probe = state.proxies_needing_check(60);
    let fast_probe_handle = {
        let state = state.clone();
        tokio::spawn(async move {
            pool::health_checker::fast_probe(&state, to_probe, 3000).await
        })
    };

    // =============================================
    // Start services (don't wait for fast probe)
    // =============================================

    // Track all task handles for graceful shutdown
    let mut handles: Vec<JoinHandle<()>> = Vec::new();

    // Transparent proxy (accepts clients immediately,
    // waits on first_ready if pool is empty)
    let proxy_state = state.clone();
    let max_connections = config.max_connections;
    handles.push(tokio::spawn(async move {
        if let Err(e) = proxy::transparent::run_with_max_connections(proxy_state, config.gateway_port, max_connections).await {
            tracing::error!("Transparent proxy error: {}", e);
        }
    }));

    // UDP relay (DNS and other UDP)
    let udp_state = state.clone();
    let udp_port = config.udp_port;
    let dns_upstream = config.dns_upstream.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = proxy::udp::start(udp_state, udp_port, dns_upstream).await {
            tracing::error!("UDP relay error: {}", e);
        }
    }));

    // Web API
    let api_state = state.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = api::web::run(api_state, config.api_port).await {
            tracing::error!("Web API error: {}", e);
        }
    }));

    // Wait for fast probe to finish (but proxy is already accepting)
    if let Ok(found) = fast_probe_handle.await {
        tracing::info!("Level 1 complete: {} working proxies found", found);
    }

    // =============================================
    // LEVEL 2: BACKGROUND — Full source refresh
    // =============================================
    let full_state = state.clone();
    let sources_path = config.sources_path.clone();
    handles.push(tokio::spawn(async move {
        // Initial full refresh
        pool::source_manager::full_refresh_with_sources(&full_state, &sources_path).await;

        // Then periodic refresh
        pool::source_manager::run_source_loop_with_path(full_state, config.source_update_interval, &sources_path).await;
    }));

    // =============================================
    // LEVEL 3: CONTINUOUS — Health check loop
    // =============================================
    let health_state = state.clone();
    handles.push(tokio::spawn(async move {
        // Small delay to let full refresh populate the pool first
        tokio::time::sleep(Duration::from_secs(10)).await;
        pool::health_checker::run_health_loop(health_state).await;
    }));

    // State persistence loop
    let persist_state = state.clone();
    handles.push(tokio::spawn(async move {
        pool::persistence::run_persistence_loop(persist_state, 300).await;
    }));

    // Connection pool cleanup loop (only if enabled)
    if config.enable_connection_pool {
        let pool_state = state.clone();
        handles.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                pool_state.connection_pool.cleanup().await;
                tracing::debug!("Connection pool cleaned up");
            }
        }));
    }

    // Sticky sessions cleanup loop
    let sticky_state = state.clone();
    handles.push(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            sticky_state.sticky_sessions.cleanup();
            tracing::debug!("Sticky sessions cleaned up");
        }
    }));

    // Config reload loop
    let reload_state = state.clone();
    let reload_manager = config_manager.clone();
    handles.push(tokio::spawn(async move {
        while config_rx.recv().await.is_some() {
            tracing::info!("Config file changed, reloading...");
            if let Err(e) = reload_manager.reload().await {
                tracing::error!("Config reload failed: {}", e);
            } else {
                tracing::info!("Config reloaded successfully");
            }
            // Update sticky session TTL if changed
            let new_config = reload_manager.get().await;
            reload_state.sticky_sessions.set_ttl(new_config.sticky_session_ttl);
        }
    }));

    // =============================================
    // Graceful shutdown
    // =============================================
    tracing::info!(
        "VPN Gateway ready. Proxy: :{}, API: :{}",
        config.gateway_port,
        config.api_port
    );

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
    tracing::info!("Received Ctrl+C, shutting down...");

    // Abort all background tasks
    for handle in handles {
        handle.abort();
    }

    // Stop config watcher
    config_manager.stop_watching().await;

    // Clear connection pool (if enabled)
    if config.enable_connection_pool {
        state.connection_pool.clear().await;
    }

    // Clear sticky sessions
    state.sticky_sessions.clear();

    // Save state before exit
    pool::persistence::save_state(&state).await;
    tracing::info!("State saved. Goodbye.");

    Ok(())
}
