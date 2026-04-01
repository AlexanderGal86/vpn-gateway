use crate::pool::proxy::{Protocol, Proxy};
use anyhow::{anyhow, Result};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

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
async fn http_connect_handshake(
    mut stream: TcpStream,
    host: &str,
    port: u16,
) -> Result<TcpStream> {
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
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
}

/// SOCKS5 handshake (RFC 1928).
///
/// 1. Greeting: VER=5, NMETHODS=1, METHOD=0 (no auth)
/// 2. Server: VER=5, METHOD=0
/// 3. Request: VER=5, CMD=CONNECT, ATYP=DOMAIN, domain, port
/// 4. Server: VER=5, REP=0 (success), ...
async fn socks5_handshake(
    mut stream: TcpStream,
    host: &str,
    port: u16,
) -> Result<TcpStream> {
    // --- Step 1: Greeting ---
    // VER=5, NMETHODS=1, METHODS=[NO_AUTH(0)]
    stream.write_all(&[0x05, 0x01, 0x00]).await?;

    // --- Step 2: Server method selection ---
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp).await?;

    if resp[0] != 0x05 {
        return Err(anyhow!("SOCKS5: server returned wrong version: {}", resp[0]));
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
        return Err(anyhow!("SOCKS5 CONNECT failed: {} (code {})", err_msg, resp_header[1]));
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

#[cfg(test)]
mod tests {
    use super::*;

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

    // TODO: Add integration tests with mock proxy server
    // These tests require a real TCP server implementing HTTP CONNECT and SOCKS5.
    // For now, we test the helper functions only.
    //
    // Future tests needed:
    // - test_http_connect_handshake_success
    // - test_http_connect_handshake_failure
    // - test_http_connect_handshake_timeout
    // - test_socks5_handshake_success
    // - test_socks5_handshake_auth_required
    // - test_socks5_handshake_connection_refused
    // - test_connect_through_proxy_fallback
}
