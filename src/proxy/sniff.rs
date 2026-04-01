/// Target determined by sniffing the first bytes of a connection.
#[derive(Debug, Clone)]
pub enum SniffedTarget {
    /// TLS SNI hostname extracted from ClientHello
    TlsSni(String),
    /// HTTP Host header
    HttpHost(String),
    /// Could not determine domain, only have IP
    Unknown,
}

/// Parse TLS SNI from a ClientHello message.
///
/// TLS Record format:
///   Byte 0:   0x16 (Handshake)
///   Byte 1-2: version (0x0301 usually)
///   Byte 3-4: record length
///   Byte 5:   handshake type (0x01 = ClientHello)
///   ...
///   Extensions contain SNI (type 0x0000)
pub fn parse_tls_sni(buf: &[u8]) -> Option<String> {
    // Minimum: TLS record header (5) + handshake header (4) + client hello fields
    if buf.len() < 43 {
        return None;
    }

    // Check TLS record
    if buf[0] != 0x16 {
        return None; // Not a Handshake record
    }

    let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    if buf.len() < 5 + record_len.min(buf.len() - 5) {
        // We might have a partial read, try anyway
    }

    // Handshake type = ClientHello (0x01)
    if buf[5] != 0x01 {
        return None;
    }

    // Skip handshake header (4 bytes: type + 3-byte length)
    // Skip client hello fixed fields:
    //   2 bytes version
    //   32 bytes random
    let mut pos = 5 + 4 + 2 + 32;

    if pos >= buf.len() {
        return None;
    }

    // Session ID (variable length)
    let session_id_len = buf[pos] as usize;
    pos += 1 + session_id_len;
    if pos + 2 > buf.len() {
        return None;
    }

    // Cipher suites (variable length)
    let cipher_suites_len = u16::from_be_bytes([buf[pos], buf[pos + 1]]) as usize;
    pos += 2 + cipher_suites_len;
    if pos + 1 > buf.len() {
        return None;
    }

    // Compression methods (variable length)
    let compression_len = buf[pos] as usize;
    pos += 1 + compression_len;
    if pos + 2 > buf.len() {
        return None;
    }

    // Extensions total length
    let extensions_len = u16::from_be_bytes([buf[pos], buf[pos + 1]]) as usize;
    pos += 2;

    let extensions_end = (pos + extensions_len).min(buf.len());

    // Iterate extensions
    while pos + 4 <= extensions_end {
        let ext_type = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let ext_len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;

        if ext_type == 0x0000 {
            // SNI extension
            // SNI list length (2 bytes) + type (1 byte) + name length (2 bytes)
            if pos + 5 > extensions_end {
                return None;
            }
            // Skip SNI list length
            let _sni_list_len = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
            let sni_type = buf[pos + 2]; // 0 = hostname
            let name_len = u16::from_be_bytes([buf[pos + 3], buf[pos + 4]]) as usize;
            pos += 5;

            if sni_type == 0 && pos + name_len <= extensions_end {
                return std::str::from_utf8(&buf[pos..pos + name_len])
                    .ok()
                    .map(|s| s.to_string());
            }
            return None;
        }

        pos += ext_len;
    }

    None
}

/// Parse HTTP Host header from the first line of a request.
///
/// Expects: "GET /path HTTP/1.1\r\nHost: example.com\r\n..."
/// or: "CONNECT example.com:443 HTTP/1.1\r\n..."
pub fn parse_http_host(buf: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(buf).ok()?;
    let first_line = text.lines().next()?;

    // CONNECT method: "CONNECT host:port HTTP/1.1"
    if first_line.starts_with("CONNECT ") {
        let target = first_line.split_whitespace().nth(1)?;
        let host = target.split(':').next()?;
        return Some(host.to_string());
    }

    // Other HTTP methods: look for Host header
    for line in text.lines().skip(1) {
        if line.to_lowercase().starts_with("host:") {
            let host = line[5..].trim();
            // Remove port if present
            let host = host.split(':').next().unwrap_or(host);
            return Some(host.to_string());
        }
        if line.is_empty() {
            break; // End of headers
        }
    }

    None
}

/// Sniff the target from the first bytes of a connection.
pub fn sniff(buf: &[u8]) -> SniffedTarget {
    if buf.is_empty() {
        return SniffedTarget::Unknown;
    }

    // Try TLS ClientHello first (most common for HTTPS)
    if buf[0] == 0x16 {
        if let Some(sni) = parse_tls_sni(buf) {
            return SniffedTarget::TlsSni(sni);
        }
    }

    // Try HTTP Host header
    if let Some(host) = parse_http_host(buf) {
        return SniffedTarget::HttpHost(host);
    }

    SniffedTarget::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_http_connect() {
        let req = b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n";
        assert_eq!(
            parse_http_host(req),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn test_parse_http_get() {
        let req = b"GET /path HTTP/1.1\r\nHost: www.example.com\r\nAccept: */*\r\n\r\n";
        assert_eq!(
            parse_http_host(req),
            Some("www.example.com".to_string())
        );
    }

    #[test]
    fn test_parse_http_host_with_port() {
        let req = b"GET / HTTP/1.1\r\nHost: example.com:8080\r\n\r\n";
        assert_eq!(
            parse_http_host(req),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn test_sniff_unknown() {
        let garbage = &[0x00, 0x01, 0x02, 0x03];
        assert!(matches!(sniff(garbage), SniffedTarget::Unknown));
    }

    #[test]
    fn test_sniff_empty_buffer() {
        assert!(matches!(sniff(&[]), SniffedTarget::Unknown));
    }

    #[test]
    fn test_parse_tls_sni_not_handshake() {
        let buf: &[u8] = &[0x17, 0x03, 0x03, 0x00, 0x10];
        assert_eq!(parse_tls_sni(buf), None);
    }

    #[test]
    fn test_parse_tls_sni_too_short() {
        let buf: &[u8] = &[0x16, 0x03, 0x01];
        assert_eq!(parse_tls_sni(buf), None);
    }

    #[test]
    fn test_parse_http_post() {
        let req = b"POST /api HTTP/1.1\r\nHost: api.example.com\r\n\r\n";
        assert_eq!(parse_http_host(req), Some("api.example.com".to_string()));
    }

    #[test]
    fn test_parse_http_no_host() {
        let req = b"GET /path HTTP/1.1\r\nAccept: */*\r\n\r\n";
        assert_eq!(parse_http_host(req), None);
    }

    #[test]
    fn test_parse_http_malformed() {
        assert_eq!(parse_http_host(b"INVALID"), None);
    }
}
