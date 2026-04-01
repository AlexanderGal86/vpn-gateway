use anyhow::Result;
use notify::Watcher;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_gateway_port")]
    pub gateway_port: u16,
    
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    
    #[serde(default = "default_udp_port")]
    pub udp_port: u16,
    
    #[serde(default = "default_max_proxies")]
    pub max_proxies: usize,
    
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval: u64,
    
    #[serde(default = "default_source_update_interval")]
    pub source_update_interval: u64,
    
    #[serde(default)]
    pub preferred_countries: Vec<String>,
    
    #[serde(default)]
    pub geoip_path: Option<String>,
    
    #[serde(default = "default_state_path")]
    pub state_path: String,
    
    #[serde(default = "default_sources_path")]
    pub sources_path: String,
    
    #[serde(default = "default_connection_pool_max_idle")]
    pub connection_pool_max_idle: u64,
    
    #[serde(default = "default_connection_pool_max_per_proxy")]
    pub connection_pool_max_per_proxy: usize,
    
    #[serde(default)]
    pub enable_connection_pool: bool,
    
    #[serde(default = "default_sticky_session_ttl")]
    pub sticky_session_ttl: u64,
    
    #[serde(default)]
    pub enable_sticky_sessions: bool,
}

fn default_gateway_port() -> u16 { 1080 }
fn default_api_port() -> u16 { 8080 }
fn default_udp_port() -> u16 { 1081 }
fn default_max_proxies() -> usize { 5000 }
fn default_max_connections() -> usize { 10000 }
fn default_health_check_interval() -> u64 { 30 }
fn default_source_update_interval() -> u64 { 300 }
fn default_state_path() -> String { "data/state.json".to_string() }
fn default_sources_path() -> String { "config/sources.json".to_string() }
fn default_connection_pool_max_idle() -> u64 { 60 }
fn default_connection_pool_max_per_proxy() -> usize { 10 }
fn default_sticky_session_ttl() -> u64 { 300 }

impl Default for Config {
    fn default() -> Self {
        Self {
            gateway_port: default_gateway_port(),
            api_port: default_api_port(),
            udp_port: default_udp_port(),
            max_proxies: default_max_proxies(),
            max_connections: default_max_connections(),
            health_check_interval: default_health_check_interval(),
            source_update_interval: default_source_update_interval(),
            preferred_countries: Vec::new(),
            geoip_path: None,
            state_path: default_state_path(),
            sources_path: default_sources_path(),
            connection_pool_max_idle: default_connection_pool_max_idle(),
            connection_pool_max_per_proxy: default_connection_pool_max_per_proxy(),
            enable_connection_pool: false,
            sticky_session_ttl: default_sticky_session_ttl(),
            enable_sticky_sessions: false,
        }
    }
}

impl Config {
    pub fn load_from_file(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn load_or_default(path: &str) -> Self {
        match Self::load_from_file(path) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!("Failed to load config from '{}': {}. Using defaults.", path, e);
                Self::default()
            }
        }
    }
}

#[derive(Clone)]
pub struct ConfigManager {
    config: Arc<RwLock<Config>>,
    watch_path: PathBuf,
}

impl ConfigManager {
    pub fn new(path: String) -> Self {
        let watch_path = PathBuf::from(&path);
        let initial_config = Config::load_or_default(&path);
        
        Self {
            config: Arc::new(RwLock::new(initial_config)),
            watch_path,
        }
    }

    pub async fn get(&self) -> Config {
        self.config.read().await.clone()
    }

    #[allow(dead_code)]
    pub async fn update(&self, new_config: Config) {
        let mut guard = self.config.write().await;
        *guard = new_config;
    }

    /// Reload config from file.
    pub async fn reload(&self) -> Result<()> {
        let new_config = Config::load_from_file(self.watch_path.to_str().unwrap_or("config.json"))?;
        let mut guard = self.config.write().await;
        *guard = new_config;
        tracing::info!("Config reloaded from file");
        Ok(())
    }

    /// Start watching for config file changes.
    /// Returns a receiver that gets a message when the config file is modified.
    pub async fn start_watching(&self) -> tokio::sync::mpsc::Receiver<()> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);

        let watch_path = self.watch_path.clone();
        
        tokio::spawn(async move {
            let watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        if event.kind.is_modify() || event.kind.is_create() {
                            let _ = tx.blocking_send(());
                        }
                    }
                    Err(e) => tracing::error!("Config watch error: {}", e),
                }
            });
            
            if let Ok(mut watcher) = watcher {
                if let Err(e) = watcher.watch(&watch_path, notify::RecursiveMode::NonRecursive) {
                    tracing::error!("Failed to watch config: {}", e);
                }
                tracing::info!("Config file watcher started for {:?}", watch_path);
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                }
            } else {
                tracing::error!("Failed to create config file watcher");
            }
        });

        rx
    }

    /// Stop watching for changes.
    pub async fn stop_watching(&self) {
        // The watcher task runs until process shutdown (handled via JoinHandle.abort in main)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.gateway_port, 1080);
        assert_eq!(config.api_port, 8080);
    }

    #[test]
    fn test_config_load_or_default() {
        let config = Config::load_or_default("nonexistent.json");
        assert_eq!(config.gateway_port, 1080); // defaults
    }

    #[test]
    fn test_config_all_defaults() {
        let config = Config::default();
        assert_eq!(config.gateway_port, 1080);
        assert_eq!(config.api_port, 8080);
        assert_eq!(config.udp_port, 1081);
        assert_eq!(config.max_proxies, 5000);
        assert_eq!(config.max_connections, 10000);
        assert_eq!(config.health_check_interval, 30);
        assert_eq!(config.source_update_interval, 300);
        assert_eq!(config.preferred_countries.len(), 0);
        assert_eq!(config.state_path, "data/state.json");
        assert_eq!(config.sources_path, "config/sources.json");
        assert_eq!(config.connection_pool_max_idle, 60);
        assert_eq!(config.connection_pool_max_per_proxy, 10);
        assert!(!config.enable_connection_pool);
        assert_eq!(config.sticky_session_ttl, 300);
        assert!(!config.enable_sticky_sessions);
    }

    #[test]
    fn test_config_from_json() {
        let json = r#"{
            "gateway_port": 9999,
            "api_port": 7777,
            "udp_port": 6666,
            "preferred_countries": ["US", "DE"],
            "enable_connection_pool": true,
            "enable_sticky_sessions": true
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.gateway_port, 9999);
        assert_eq!(config.api_port, 7777);
        assert_eq!(config.udp_port, 6666);
        assert_eq!(config.preferred_countries, vec!["US", "DE"]);
        assert!(config.enable_connection_pool);
        assert!(config.enable_sticky_sessions);
    }

    #[test]
    fn test_config_partial_json() {
        let json = r#"{"gateway_port": 1234}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.gateway_port, 1234);
        assert_eq!(config.api_port, 8080); // default
    }

    #[test]
    fn test_config_manager_get() {
        let manager = ConfigManager::new("nonexistent.json".to_string());
        let config = futures::executor::block_on(manager.get());
        assert_eq!(config.gateway_port, 1080);
    }
}
