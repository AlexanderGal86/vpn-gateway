use crate::pool::metrics;
use crate::pool::proxy::Protocol;
use crate::pool::state::SharedState;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    total_proxies: usize,
    verified_proxies: usize,
    available_proxies: usize,
}

#[derive(Serialize)]
struct MetricsResponse {
    total_proxies: usize,
    verified_proxies: usize,
    available_proxies: usize,
    tls_clean_proxies: usize,
    mitm_proxies: usize,
    total_requests: u64,
    active_connections: u64,
    proxy_rotations: u64,
}

#[derive(Serialize)]
struct ProxyInfo {
    host: String,
    port: u16,
    protocol: String,
    latency_ms: f64,
    success_count: u64,
    fail_count: u64,
    available: bool,
    country: Option<String>,
    tls_clean: Option<bool>,
}

#[derive(Deserialize)]
struct AddProxyRequest {
    host: String,
    port: u16,
    protocol: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ApiResponse {
    success: bool,
    message: String,
}

#[derive(Serialize, Deserialize)]
struct NetworkStatus {
    lan_ip: Option<String>,
    wan_ip: Option<String>,
    upnp_active: bool,
    wg_port: u16,
    peer_count: usize,
    peers: Vec<String>,
}

#[derive(Serialize)]
struct WgPeerInfo {
    name: String,
    address: String,
    public_key: String,
    has_lan_config: bool,
    has_wan_config: bool,
}

async fn health(State(state): State<SharedState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: if state.available_count() > 0 {
            "ok"
        } else {
            "no_proxies"
        },
        total_proxies: state.total_count(),
        verified_proxies: state.verified_count(),
        available_proxies: state.available_count(),
    })
}

async fn metrics(State(state): State<SharedState>) -> Json<MetricsResponse> {
    Json(MetricsResponse {
        total_proxies: state.total_count(),
        verified_proxies: state.verified_count(),
        available_proxies: state.available_count(),
        tls_clean_proxies: state.tls_clean_count(),
        mitm_proxies: state.tls_dirty_count(),
        total_requests: state.total_requests.load(Ordering::Relaxed),
        active_connections: state.active_connections.load(Ordering::Relaxed),
        proxy_rotations: state.proxy_rotations.load(Ordering::Relaxed),
    })
}

async fn prometheus_metrics(State(state): State<SharedState>) -> String {
    let m = metrics::collect(&state);
    metrics::format_prometheus(&m)
}

async fn list_proxies(State(state): State<SharedState>) -> Json<Vec<ProxyInfo>> {
    let mut proxies: Vec<_> = state
        .all_proxies()
        .into_iter()
        .filter(|p| p.is_available())
        .map(|p| ProxyInfo {
            host: p.host.clone(),
            port: p.port,
            protocol: format!("{:?}", p.protocol).to_lowercase(),
            latency_ms: p.latency_ewma,
            success_count: p.success_count,
            fail_count: p.fail_count,
            available: true,
            country: p.country.clone(),
            tls_clean: p.tls_clean,
        })
        .collect();

    proxies.sort_by(|a, b| {
        a.latency_ms
            .partial_cmp(&b.latency_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Json(proxies)
}

async fn add_proxy(
    State(state): State<SharedState>,
    Json(payload): Json<AddProxyRequest>,
) -> Json<ApiResponse> {
    let protocol = match payload.protocol.as_deref() {
        Some("socks5") | Some("socks5 ") => Protocol::Socks5,
        _ => Protocol::Http,
    };

    let key = format!("{}:{}", payload.host, payload.port);

    if state.proxies.contains_key(&key) {
        return Json(ApiResponse {
            success: false,
            message: "Proxy already exists".to_string(),
        });
    }

    let proxy = crate::pool::proxy::Proxy::new(payload.host.clone(), payload.port, protocol);
    state.proxies.insert(key, proxy);

    tracing::info!("Manual proxy added: {}:{}", payload.host, payload.port);

    Json(ApiResponse {
        success: true,
        message: format!("Proxy {}:{} added", payload.host, payload.port),
    })
}

async fn ban_proxy(
    State(state): State<SharedState>,
    Path(proxy_key): Path<String>,
) -> Json<ApiResponse> {
    let parts: Vec<&str> = proxy_key.split(':').collect();
    if parts.len() != 2 {
        return Json(ApiResponse {
            success: false,
            message: "Invalid proxy key format. Use host:port".to_string(),
        });
    }
    if parts[1].parse::<u16>().is_err() {
        return Json(ApiResponse {
            success: false,
            message: "Invalid port number".to_string(),
        });
    }

    let key = format!("{}:{}", parts[0], parts[1]);

    if let Some((_, proxy)) = state.proxies.remove(&key) {
        state.banned.insert(key.clone(), proxy);
        tracing::info!("Proxy banned: {}", key);

        Json(ApiResponse {
            success: true,
            message: format!("Proxy {} banned", key),
        })
    } else {
        Json(ApiResponse {
            success: false,
            message: "Proxy not found".to_string(),
        })
    }
}

async fn unban_proxy(
    State(state): State<SharedState>,
    Path(proxy_key): Path<String>,
) -> Json<ApiResponse> {
    if let Some((_, proxy)) = state.banned.remove(&proxy_key) {
        let key = proxy.key();
        state.proxies.insert(key, proxy);

        tracing::info!("Proxy unbanned: {}", proxy_key);

        Json(ApiResponse {
            success: true,
            message: format!("Proxy {} unbanned", proxy_key),
        })
    } else {
        Json(ApiResponse {
            success: false,
            message: "Banned proxy not found".to_string(),
        })
    }
}

async fn network_status() -> Json<NetworkStatus> {
    let status_path = std::path::Path::new("data/clients/network-status.json");
    if status_path.exists() {
        match tokio::fs::read_to_string(status_path).await {
            Ok(content) => match serde_json::from_str::<NetworkStatus>(&content) {
                Ok(status) => return Json(status),
                Err(e) => tracing::warn!("Failed to parse network-status.json: {}", e),
            },
            Err(e) => tracing::warn!("Failed to read network-status.json: {}", e),
        }
    }
    Json(NetworkStatus {
        lan_ip: None,
        wan_ip: None,
        upnp_active: false,
        wg_port: 51820,
        peer_count: 0,
        peers: Vec::new(),
    })
}

async fn list_wg_peers() -> Json<Vec<WgPeerInfo>> {
    let peers_dir = std::path::Path::new("data/wg");
    let mut result = Vec::new();

    if !peers_dir.exists() {
        return Json(result);
    }

    match tokio::fs::read_dir(peers_dir).await {
        Ok(mut entries) => {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("peer") || !entry.path().is_dir() {
                    continue;
                }

                let peer_dir = entry.path();
                let address = read_peer_address(&peer_dir).await;
                let public_key = read_peer_file(&peer_dir, &format!("publickey-{}", name)).await;
                let has_lan = peer_dir.join(format!("{}-lan.conf", name)).exists();
                let has_wan = peer_dir.join(format!("{}-wan.conf", name)).exists();

                result.push(WgPeerInfo {
                    name,
                    address,
                    public_key,
                    has_lan_config: has_lan,
                    has_wan_config: has_wan,
                });
            }
        }
        Err(e) => tracing::warn!("Failed to read peers directory: {}", e),
    }

    Json(result)
}

async fn read_peer_address(peer_dir: &std::path::Path) -> String {
    let name = match peer_dir.file_name() {
        Some(n) => n.to_string_lossy(),
        None => return String::new(),
    };
    let conf_path = peer_dir.join(format!("{}.conf", name));
    if let Ok(content) = tokio::fs::read_to_string(&conf_path).await {
        for line in content.lines() {
            if line.starts_with("Address = ") {
                return line.trim_start_matches("Address = ").to_string();
            }
        }
    }
    String::new()
}

async fn read_peer_file(peer_dir: &std::path::Path, filename: &str) -> String {
    let path = peer_dir.join(filename);
    tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_default()
        .trim()
        .to_string()
}

static DASHBOARD_HTML: &str = include_str!("../../dashboard/index.html");

async fn dashboard() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

/// Validate peer name: alphanumeric, hyphens, underscores only (1-64 chars)
fn is_safe_peer_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn peer_config(Path(name): Path<String>) -> Response {
    if !is_safe_peer_name(&name) {
        return (StatusCode::BAD_REQUEST, "Invalid peer name").into_response();
    }

    let conf_path = std::path::PathBuf::from(format!("data/wg/{}/{}.conf", name, name));
    match tokio::fs::read_to_string(&conf_path).await {
        Ok(content) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    &format!("attachment; filename=\"{}.conf\"", name),
                ),
            ],
            content,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Config not found").into_response(),
    }
}

async fn peer_qr(Path(name): Path<String>) -> Response {
    if !is_safe_peer_name(&name) {
        return (StatusCode::BAD_REQUEST, "Invalid peer name").into_response();
    }

    let qr_path = std::path::PathBuf::from(format!("data/wg/{}/{}-qr.png", name, name));
    match tokio::fs::read(&qr_path).await {
        Ok(bytes) => (StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "QR code not found").into_response(),
    }
}

pub fn create_router(state: SharedState) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/health", get(health))
        .route("/api/metrics", get(metrics))
        .route("/metrics", get(prometheus_metrics))
        .route("/api/proxies", get(list_proxies))
        .route("/api/proxy/add", post(add_proxy))
        .route("/api/proxy/ban/{proxy_key}", post(ban_proxy))
        .route("/api/proxy/unban/{proxy_key}", post(unban_proxy))
        .route("/api/network-status", get(network_status))
        .route("/api/wg/peers", get(list_wg_peers))
        .route("/api/wg/peers/{name}/config", get(peer_config))
        .route("/api/wg/peers/{name}/qr", get(peer_qr))
        .with_state(state)
}

pub async fn run(state: SharedState, bind: &str, port: u16) -> anyhow::Result<()> {
    let app = create_router(state.clone());
    let app_localhost = create_router(state);

    // Bind to both WireGuard interface and localhost
    let wg_addr = format!("{}:{}", bind, port);
    let localhost_addr = format!("127.0.0.1:{}", port);

    tokio::spawn(async move {
        if let Ok(listener) = tokio::net::TcpListener::bind(&localhost_addr).await {
            tracing::info!("Web API listening on localhost {}", localhost_addr);
            axum::serve(listener, app_localhost).await.ok();
        }
    });

    let wg_listener = tokio::net::TcpListener::bind(&wg_addr).await?;
    tracing::info!("Web API listening on {}", wg_addr);
    axum::serve(wg_listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::state::SharedState;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::Router;
    use http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        let state = SharedState::new();
        create_router(state)
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_metrics_endpoint() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_list_proxies_empty() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/proxies")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_network_status_no_net_manager() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/network-status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_wg_peers_empty() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/wg/peers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    fn test_app_with_state() -> (Router, SharedState) {
        let state = SharedState::new();
        let app = create_router(state.clone());
        (app, state)
    }

    #[tokio::test]
    async fn test_add_proxy_valid() {
        let (app, _state) = test_app_with_state();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/proxy/add")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"host":"1.2.3.4","port":8080}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_add_proxy_duplicate() {
        let (app, state) = test_app_with_state();
        // Pre-insert
        let proxy = crate::pool::proxy::Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        state.proxies.insert("1.2.3.4:8080".into(), proxy);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/proxy/add")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"host":"1.2.3.4","port":8080}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // Parse body to check success=false
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let resp: ApiResponse = serde_json::from_slice(&body).unwrap();
        assert!(!resp.success);
    }

    #[tokio::test]
    async fn test_ban_proxy_invalid_key() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/proxy/ban/invalid-no-port")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let resp: ApiResponse = serde_json::from_slice(&body).unwrap();
        assert!(!resp.success);
    }

    #[tokio::test]
    async fn test_ban_proxy_invalid_port() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/proxy/ban/1.2.3.4:notaport")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let resp: ApiResponse = serde_json::from_slice(&body).unwrap();
        assert!(!resp.success);
    }

    #[tokio::test]
    async fn test_ban_and_unban_proxy() {
        let (app, state) = test_app_with_state();
        let proxy = crate::pool::proxy::Proxy::new("5.6.7.8".into(), 3128, Protocol::Http);
        state.proxies.insert("5.6.7.8:3128".into(), proxy);

        // Ban
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/proxy/ban/5.6.7.8:3128")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let resp: ApiResponse = serde_json::from_slice(&body).unwrap();
        assert!(resp.success);
        assert_eq!(state.proxies.len(), 0);
        assert_eq!(state.banned.len(), 1);

        // Unban (need fresh app since oneshot consumes it)
        let app2 = create_router(state.clone());
        let response = app2
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/proxy/unban/5.6.7.8:3128")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let resp: ApiResponse = serde_json::from_slice(&body).unwrap();
        assert!(resp.success);
        assert_eq!(state.proxies.len(), 1);
        assert_eq!(state.banned.len(), 0);
    }

    #[tokio::test]
    async fn test_prometheus_metrics_format() {
        let (app, state) = test_app_with_state();
        // Add a proxy so metrics have something
        let proxy = crate::pool::proxy::Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        state.proxies.insert("1.2.3.4:8080".into(), proxy);
        state.record_success("1.2.3.4:8080", 100.0);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("vpn_proxies_total"),
            "Prometheus metrics should contain vpn_proxies_total"
        );
    }

    #[tokio::test]
    async fn test_health_with_proxies() {
        let (app, state) = test_app_with_state();
        let proxy = crate::pool::proxy::Proxy::new("1.2.3.4".into(), 8080, Protocol::Http);
        state.proxies.insert("1.2.3.4:8080".into(), proxy);
        state.record_success("1.2.3.4:8080", 100.0);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("\"status\":\"ok\""));
        assert!(text.contains("\"verified_proxies\":1"));
    }
}
