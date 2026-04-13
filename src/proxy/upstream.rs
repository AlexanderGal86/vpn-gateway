use crate::pool::proxy::{Protocol, Proxy};
use anyhow::{anyhow, Result};
use socket2::SockRef;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Set TCP keepalive on an upstream proxy connection.
///
/// Probing starts after `idle_secs` of silence, then retries every
/// `interval_secs`. With default kernel retries (~3), a dead connection
/// is detected in approximately `idle_secs + interval_secs * 3`.
fn set_tcp_keepalive(stream: &TcpStream, idle_secs: u64, interval_secs: u64) {
    let sock_ref = SockRef::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(idle_secs))
        .with_interval(Duration::from_secs(interval_secs));
    if let Err(e) = sock_ref.set_tcp_keepalive(&keepalive) {
        tracing::debug!("Failed to set TCP keepalive: {}", e);
    }
}

/// Connect to target through an upstream proxy, performing the correct
/// protocol handshake (HTTP CONNECT or SOCKS5).
///
/// Returns a TcpStream that is already tunneled to the target.
/// After this returns, bytes written to the stream go directly to the target.
pub async fn connect_through_proxy(
    proxy: &Proxy,
    target_host: &str,
    target_port: u16,
    connect_timeout: Duration,
) -> Result<TcpStream> {
    // Step 1: TCP connect to the proxy server
    let stream = tokio::time::timeout(connect_timeout, TcpStream::connect(proxy.addr()))
        .await
        .map_err(|_| anyhow!("TCP connect to proxy {} timed out", proxy.addr()))?
        .map_err(|e| anyhow!("TCP connect to proxy {} failed: {}", proxy.addr(), e))?;

    // Enable TCP keepalive: detect dead connections in ~75s (30 + 15*3)
    set_tcp_keepalive(&stream, 30, 15);

    // Step 2: Protocol-specific handshake
    match proxy.protocol {
        Protocol::Http | Protocol::Https => {
            http_connect_handshake(stream, target_host, target_port).await
        }
        Protocol::Socks5 => socks5_handshake(stream, target_host, target_port).await,
    }
}

/// HTTP CONNECT tunnel handshake.
///
/// Sends: CONNECT target:port HTTP/1.1\r\nHost: target:port\r\n\r\n
/// Expects: HTTP/1.1 200 ...\r\n\r\n
async fn http_connect_handshake(mut stream: TcpStream, host: &str, port: u16) -> Result<TcpStream> {
    // Send CONNECT request
    let request = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\nProxy-Connection: keep-alive\r\n\r\n",
        host, port, host, port
    );
    stream.write_all(request.as_bytes()).await?;

    // Read response (read until we see \r\n\r\n or hit 1KB limit)
    let mut buf = vec![0u8; 1024];
    let mut total = 0;

    loop {
        if total >= buf.len() {
            return Err(anyhow!("HTTP CONNECT response too large"));
        }
        let n = stream.read(&mut buf[total..]).await?;
        if n == 0 {
            return Err(anyhow!("HTTP CONNECT: connection closed before response"));
        }
        total += n;

        // Check if we have the complete response header
        if let Some(header_end) = find_header_end(&buf[..total]) {
            let response = String::from_utf8_lossy(&buf[..header_end]);
            let first_line = response.lines().next().unwrap_or("");

            // Parse status code from "HTTP/1.1 200 Connection established"
            let status: u16 = first_line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);

            if status == 200 {
                // Tunnel established.
                // If there are bytes after the header, they belong to the target.
                // (In practice, there shouldn't be — the proxy waits for client data.)
                return Ok(stream);
            } else {
                return Err(anyhow!(
                    "HTTP CONNECT failed with status {}: {}",
                    status,
                    first_line
                ));
            }
        }
    }
}

/// Find the end of HTTP headers (\r\n\r\n) in a buffer.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// SOCKS5 handshake (RFC 1928).
///
/// 1. Greeting: VER=5, NMETHODS=1, METHOD=0 (no auth)
/// 2. Server: VER=5, METHOD=0
/// 3. Request: VER=5, CMD=CONNECT, ATYP=DOMAIN, domain, port
/// 4. Server: VER=5, REP=0 (success), ...
async fn socks5_handshake(mut stream: TcpStream, host: &str, port: u16) -> Result<TcpStream> {
    // --- Step 1: Greeting ---
    // VER=5, NMETHODS=1, METHODS=[NO_AUTH(0)]
    stream.write_all(&[0x05, 0x01, 0x00]).await?;

    // --- Step 2: Server method selection ---
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp).await?;

    if resp[0] != 0x05 {
        return Err(anyhow!(
            "SOCKS5: server returned wrong version: {}",
            resp[0]
        ));
    }
    if resp[1] == 0xFF {
        return Err(anyhow!("SOCKS5: no acceptable auth methods"));
    }
    // resp[1] == 0x00 means no auth needed — proceed

    // --- Step 3: Connection request ---
    // VER=5, CMD=CONNECT(1), RSV=0, ATYP=DOMAIN(3)
    let domain_bytes = host.as_bytes();
    if domain_bytes.len() > 255 {
        return Err(anyhow!("SOCKS5: domain too long: {}", host));
    }

    let mut req = Vec::with_capacity(7 + domain_bytes.len());
    req.push(0x05); // VER
    req.push(0x01); // CMD = CONNECT
    req.push(0x00); // RSV
    req.push(0x03); // ATYP = DOMAIN
    req.push(domain_bytes.len() as u8);
    req.extend_from_slice(domain_bytes);
    req.extend_from_slice(&port.to_be_bytes());

    stream.write_all(&req).await?;

    // --- Step 4: Server response ---
    // Read VER, REP, RSV, ATYP
    let mut resp_header = [0u8; 4];
    stream.read_exact(&mut resp_header).await?;

    if resp_header[0] != 0x05 {
        return Err(anyhow!(
            "SOCKS5: wrong version in response: {}",
            resp_header[0]
        ));
    }
    if resp_header[1] != 0x00 {
        let err_msg = match resp_header[1] {
            0x01 => "general SOCKS server failure",
            0x02 => "connection not allowed by ruleset",
            0x03 => "network unreachable",
            0x04 => "host unreachable",
            0x05 => "connection refused",
            0x06 => "TTL expired",
            0x07 => "command not supported",
            0x08 => "address type not supported",
            _ => "unknown error",
        };
        return Err(anyhow!(
            "SOCKS5 CONNECT failed: {} (code {})",
            err_msg,
            resp_header[1]
        ));
    }

    // Read and discard BND.ADDR + BND.PORT
    match resp_header[3] {
        0x01 => {
            // IPv4: 4 bytes addr + 2 bytes port
            let mut skip = [0u8; 6];
            stream.read_exact(&mut skip).await?;
        }
        0x03 => {
            // Domain: 1 byte len + domain + 2 bytes port
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut skip = vec![0u8; len[0] as usize + 2];
            stream.read_exact(&mut skip).await?;
        }
        0x04 => {
            // IPv6: 16 bytes addr + 2 bytes port
            let mut skip = [0u8; 18];
            stream.read_exact(&mut skip).await?;
        }
        _ => {
            return Err(anyhow!(
                "SOCKS5: unknown ATYP in response: {}",
                resp_header[3]
            ));
        }
    }

    // Tunnel is now established — stream is connected to target
    Ok(stream)
}

/// Convenience: try to connect through a proxy, falling back IP-only if no domain.
///
/// If we have a domain name (from SNI/DNS), use CONNECT/SOCKS5 with domain.
/// If we only have an IP, use CONNECT/SOCKS5 with IP (works for most proxies).
pub async fn connect_to_target(
    proxy: &Proxy,
    target: &str, // domain or IP
    port: u16,
) -> Result<TcpStream> {
    connect_through_proxy(proxy, target, port, Duration::from_secs(10)).await
}

/// Perform protocol handshake on an already-established TcpStream.
///
/// Used by the warm pool: the TCP connection is pre-established,
/// so we only need to do the CONNECT/SOCKS5 handshake (saving ~50-200ms).
pub async fn handshake_on_stream(
    stream: TcpStream,
    proxy: &Proxy,
    target: &str,
    port: u16,
) -> Result<TcpStream> {
    tokio::time::timeout(Duration::from_secs(10), async {
        match proxy.protocol {
            Protocol::Http | Protocol::Https => http_connect_handshake(stream, target, port).await,
            Protocol::Socks5 => socks5_handshake(stream, target, port).await,
        }
    })
    .await
    .map_err(|_| anyhow!("Handshake on warm connection timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spawn a mock HTTP CONNECT proxy that responds with the given status code.
    async fn mock_http_proxy(status: u16) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                // Read the CONNECT request
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);

                if request.contains("CONNECT") {
                    let response = format!(
                        "HTTP/1.1 {} {}\r\n\r\n",
                        status,
                        if status == 200 {
                            "Connection established"
                        } else {
                            "Forbidden"
                        }
                    );
                    let _ = stream.write_all(response.as_bytes()).await;

                    if status == 200 {
                        // Echo back any data sent through the tunnel
                        let mut data = [0u8; 4096];
                        if let Ok(n) = stream.read(&mut data).await {
                            if n > 0 {
                                let _ = stream.write_all(&data[..n]).await;
                            }
                        }
                    }
                }
            }
        });

        (port, handle)
    }

    /// Spawn a mock SOCKS5 proxy that handles the full handshake.
    /// If `success` is true, responds with REP=0 (success), otherwise REP=5 (refused).
    async fn mock_socks5_proxy(success: bool) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                // Step 1: Read greeting (VER, NMETHODS, METHODS...)
                let mut greeting = [0u8; 3];
                if stream.read_exact(&mut greeting).await.is_err() {
                    return;
                }

                // Step 2: Respond with method selection (no auth)
                let _ = stream.write_all(&[0x05, 0x00]).await;

                // Step 3: Read connect request
                let mut header = [0u8; 4];
                if stream.read_exact(&mut header).await.is_err() {
                    return;
                }

                // Read address based on ATYP
                match header[3] {
                    0x03 => {
                        // Domain
                        let mut len = [0u8; 1];
                        let _ = stream.read_exact(&mut len).await;
                        let mut domain = vec![0u8; len[0] as usize];
                        let _ = stream.read_exact(&mut domain).await;
                        let mut port_buf = [0u8; 2];
                        let _ = stream.read_exact(&mut port_buf).await;
                    }
                    0x01 => {
                        // IPv4
                        let mut skip = [0u8; 6];
                        let _ = stream.read_exact(&mut skip).await;
                    }
                    _ => return,
                }

                // Step 4: Respond
                let rep = if success { 0x00 } else { 0x05 };
                // VER=5, REP, RSV=0, ATYP=1(IPv4), BND.ADDR=0.0.0.0, BND.PORT=0
                let response = [0x05, rep, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
                let _ = stream.write_all(&response).await;

                if success {
                    // Echo back data through the tunnel
                    let mut data = [0u8; 4096];
                    if let Ok(n) = stream.read(&mut data).await {
                        if n > 0 {
                            let _ = stream.write_all(&data[..n]).await;
                        }
                    }
                }
            }
        });

        (port, handle)
    }

    #[test]
    fn test_find_header_end_with_complete_headers() {
        let buf = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        assert!(find_header_end(buf).is_some());
        assert_eq!(find_header_end(buf), Some(38));
    }

    #[test]
    fn test_find_header_end_without_terminator() {
        let buf = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n";
        assert!(find_header_end(buf).is_none());
    }

    #[test]
    fn test_find_header_end_empty_buffer() {
        assert!(find_header_end(b"").is_none());
    }

    #[test]
    fn test_find_header_end_short_buffer() {
        assert!(find_header_end(b"\r\n").is_none());
    }

    #[test]
    fn test_find_header_end_exact() {
        assert_eq!(find_header_end(b"\r\n\r\n"), Some(4));
    }

    // === Integration tests with mock proxy servers ===

    #[tokio::test]
    async fn test_http_connect_success() {
        let (port, _handle) = mock_http_proxy(200).await;
        let proxy = Proxy::new("127.0.0.1".to_string(), port, Protocol::Http);

        let result = connect_through_proxy(&proxy, "example.com", 80, Duration::from_secs(5)).await;

        assert!(
            result.is_ok(),
            "HTTP CONNECT should succeed, got: {:?}",
            result.err()
        );

        // Verify the tunnel works — send data and get echo
        let mut stream = result.unwrap();
        stream.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[tokio::test]
    async fn test_http_connect_rejected() {
        let (port, _handle) = mock_http_proxy(403).await;
        let proxy = Proxy::new("127.0.0.1".to_string(), port, Protocol::Http);

        let result = connect_through_proxy(&proxy, "example.com", 80, Duration::from_secs(5)).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("403"),
            "Error should mention 403, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_http_connect_timeout() {
        // Connect to a non-routable IP to trigger timeout
        let proxy = Proxy::new("192.0.2.1".to_string(), 1, Protocol::Http);

        let result =
            connect_through_proxy(&proxy, "example.com", 80, Duration::from_millis(100)).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("timed out") || err.contains("failed"),
            "Should timeout, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_socks5_handshake_success() {
        let (port, _handle) = mock_socks5_proxy(true).await;
        let proxy = Proxy::new("127.0.0.1".to_string(), port, Protocol::Socks5);

        let result = connect_through_proxy(&proxy, "example.com", 80, Duration::from_secs(5)).await;

        assert!(
            result.is_ok(),
            "SOCKS5 should succeed, got: {:?}",
            result.err()
        );

        // Verify tunnel echo
        let mut stream = result.unwrap();
        stream.write_all(b"world").await.unwrap();
        let mut buf = [0u8; 5];
        let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"world");
    }

    #[tokio::test]
    async fn test_socks5_connection_refused() {
        let (port, _handle) = mock_socks5_proxy(false).await;
        let proxy = Proxy::new("127.0.0.1".to_string(), port, Protocol::Socks5);

        let result = connect_through_proxy(&proxy, "example.com", 80, Duration::from_secs(5)).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("refused"),
            "Error should mention refused, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_connect_to_target_uses_correct_protocol() {
        // HTTP proxy
        let (http_port, _h1) = mock_http_proxy(200).await;
        let http_proxy = Proxy::new("127.0.0.1".to_string(), http_port, Protocol::Http);
        assert!(connect_to_target(&http_proxy, "example.com", 80)
            .await
            .is_ok());

        // SOCKS5 proxy
        let (socks_port, _h2) = mock_socks5_proxy(true).await;
        let socks_proxy = Proxy::new("127.0.0.1".to_string(), socks_port, Protocol::Socks5);
        assert!(connect_to_target(&socks_proxy, "example.com", 80)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_http_connect_server_closes_connection() {
        // Server that accepts and immediately closes
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                drop(stream); // close immediately
            }
        });

        let proxy = Proxy::new("127.0.0.1".to_string(), port, Protocol::Http);
        let result = connect_through_proxy(&proxy, "example.com", 80, Duration::from_secs(5)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_socks5_wrong_version_response() {
        // Server that returns wrong SOCKS version
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 3];
                let _ = stream.read_exact(&mut buf).await;
                // Return SOCKS4 version instead of 5
                let _ = stream.write_all(&[0x04, 0x00]).await;
            }
        });

        let proxy = Proxy::new("127.0.0.1".to_string(), port, Protocol::Socks5);
        let result = connect_through_proxy(&proxy, "example.com", 80, Duration::from_secs(5)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("wrong version"));
    }

    #[tokio::test]
    async fn test_socks5_no_acceptable_auth() {
        // Server that says no acceptable auth
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 3];
                let _ = stream.read_exact(&mut buf).await;
                // Return 0xFF = no acceptable methods
                let _ = stream.write_all(&[0x05, 0xFF]).await;
            }
        });

        let proxy = Proxy::new("127.0.0.1".to_string(), port, Protocol::Socks5);
        let result = connect_through_proxy(&proxy, "example.com", 80, Duration::from_secs(5)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("auth"));
    }
}
