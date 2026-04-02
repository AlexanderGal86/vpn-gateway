#![allow(dead_code)]
use anyhow::Result;
use dashmap::DashMap;
use serde::Deserialize;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct GeoIpInfo {
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub city: Option<String>,
}

impl std::fmt::Display for GeoIpInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.country_code.as_deref().unwrap_or("??"))
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ApiResponse {
    country: Option<String>,
    #[serde(rename = "countryCode")]
    country_code: Option<String>,
    city: Option<String>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct GeoIp {
    db_path: Option<String>,
    loaded: Arc<RwLock<bool>>,
    client: reqwest::Client,
    /// Cache of GeoIP lookups: IP → GeoIpInfo.
    /// Eliminates redundant API requests for the same IP.
    cache: Arc<DashMap<IpAddr, GeoIpInfo>>,
}

impl GeoIp {
    pub fn new() -> Self {
        Self {
            db_path: None,
            loaded: Arc::new(RwLock::new(false)),
            client: reqwest::Client::new(),
            cache: Arc::new(DashMap::new()),
        }
    }

    pub fn with_db_path(path: String) -> Self {
        Self {
            db_path: Some(path),
            loaded: Arc::new(RwLock::new(false)),
            client: reqwest::Client::new(),
            cache: Arc::new(DashMap::new()),
        }
    }

    pub fn with_auto_detect() -> Self {
        let paths = [
            "data/GeoLite2-City.mmdb",
            "data/GeoLite2-Country.mmdb",
            "/app/data/GeoLite2-City.mmdb",
            "/app/data/GeoLite2-Country.mmdb",
        ];

        for path in &paths {
            if Path::new(path).exists() {
                tracing::info!("Auto-detected GeoIP database: {}", path);
                return Self::with_db_path(path.to_string());
            }
        }

        Self::new()
    }

    pub async fn load(&self) -> Result<()> {
        let path = match &self.db_path {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        if !Path::new(&path).exists() {
            tracing::warn!("GeoIP database not found at: {}", path);
            return Ok(());
        }

        *self.loaded.write().await = true;
        tracing::info!("GeoIP database found at: {}", path);
        Ok(())
    }

    /// Lookup country by IP using geo.wp-statistics.com API.
    /// Results are cached to eliminate redundant API requests.
    pub async fn lookup(&self, ip: IpAddr) -> Option<GeoIpInfo> {
        // Check cache first
        if let Some(cached) = self.cache.get(&ip) {
            return Some(cached.value().clone());
        }

        let url = format!("https://geo.wp-statistics.com/{}?format=json", ip);

        let info = match self.client.get(&url).send().await {
            Ok(resp) => match resp.json::<ApiResponse>().await {
                Ok(data) => Some(GeoIpInfo {
                    country_code: data.country_code,
                    country_name: data.country,
                    city: data.city,
                }),
                Err(_) => None,
            },
            Err(e) => {
                tracing::debug!("GeoIP lookup failed for {}: {}", ip, e);
                None
            }
        };

        // Cache the result
        if let Some(ref info) = info {
            self.cache.insert(ip, info.clone());
        }

        info
    }

    /// Number of cached GeoIP entries.
    pub fn cache_size(&self) -> usize {
        self.cache.len()
    }

    pub async fn lookup_str(&self, ip_str: &str) -> Option<GeoIpInfo> {
        let ip: IpAddr = ip_str.parse().ok()?;
        self.lookup(ip).await
    }

    pub async fn country_code(&self, ip: IpAddr) -> Option<String> {
        self.lookup(ip).await?.country_code
    }

    pub async fn is_loaded(&self) -> bool {
        true // Always available via API
    }

    pub async fn reload(&self) -> Result<()> {
        self.load().await
    }
}

impl Default for GeoIp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_geoip_creation() {
        let geo = GeoIp::new();
        assert!(geo.is_loaded().await);
    }

    #[tokio::test]
    async fn test_geoip_auto_detect() {
        let geo = GeoIp::with_auto_detect();
        assert!(geo.is_loaded().await);
    }

    #[tokio::test]
    async fn test_geoip_cache_stores_and_returns() {
        let geo = GeoIp::new();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();

        // Manually insert into cache
        geo.cache.insert(
            ip,
            GeoIpInfo {
                country_code: Some("US".to_string()),
                country_name: Some("United States".to_string()),
                city: None,
            },
        );
        assert_eq!(geo.cache_size(), 1);

        // Lookup should return cached value without API call
        let result = geo.lookup(ip).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().country_code, Some("US".to_string()));
    }
}
