//! UDP Relay - DNS and general UDP forwarding

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::pool::state::SharedState;

struct UdpSession {
    upstream: UdpSocket,
    last_activity: Instant,
}

type SessionTable = Arc<RwLock<HashMap<SocketAddr, UdpSession>>>;

pub async fn start(_state: SharedState, port: u16) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", port);
    let socket = Arc::new(UdpSocket::bind(&addr).await?);

    info!("UDP relay listening on {}", addr);

    let sessions: SessionTable = Arc::new(RwLock::new(HashMap::new()));

    let sessions_clone = sessions.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            cleanup_sessions(&sessions_clone).await;
        }
    });

    let mut buf = [0u8; 65535];

    loop {
        let (len, client_addr) = match socket.recv_from(&mut buf).await {
            Ok((n, addr)) => (n, addr),
            Err(e) => {
                warn!("UDP recv error: {}", e);
                continue;
            }
        };

        let data = buf[..len].to_vec();
        let sessions = sessions.clone();
        let socket = socket.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_udp_packet(socket, client_addr, &data, &sessions).await {
                debug!("UDP packet error from {}: {}", client_addr, e);
            }
        });
    }
}

async fn handle_udp_packet(
    server_socket: Arc<UdpSocket>,
    client_addr: SocketAddr,
    data: &[u8],
    sessions: &SessionTable,
) -> anyhow::Result<()> {
    let needs_new_session = {
        let sessions_read = sessions.read().await;
        !sessions_read.contains_key(&client_addr)
    };

    if needs_new_session {
        let upstream = UdpSocket::bind("0.0.0.0:0").await?;

        {
            let mut sessions_write = sessions.write().await;
            sessions_write.insert(
                client_addr,
                UdpSession {
                    upstream,
                    last_activity: Instant::now(),
                },
            );
        }
    }

    {
        let sessions_read = sessions.read().await;
        if let Some(session) = sessions_read.get(&client_addr) {
            let upstream_addr = detect_upstream(data);

            session.upstream.send_to(data, upstream_addr).await?;

            let mut response = [0u8; 65535];
            let timeout = Duration::from_secs(5);

            match tokio::time::timeout(timeout, session.upstream.recv_from(&mut response)).await {
                Ok(Ok((len, _))) => {
                    server_socket.send_to(&response[..len], client_addr).await?;
                }
                Ok(Err(e)) => {
                    warn!("UDP upstream error: {}", e);
                }
                Err(_) => {
                    debug!("UDP timeout for {}", client_addr);
                }
            }
        }
    }

    {
        let mut sessions_write = sessions.write().await;
        if let Some(session) = sessions_write.get_mut(&client_addr) {
            session.last_activity = Instant::now();
        }
    }

    Ok(())
}

fn detect_upstream(data: &[u8]) -> &'static str {
    // Check if it looks like a DNS query (QR bit = 0, standard query opcode = 0)
    if data.len() > 12 {
        let flags = u16::from_be_bytes([data[2], data[3]]);
        let qr = (flags >> 15) & 1;
        let opcode = (flags >> 11) & 0xF;
        if qr == 0 && opcode == 0 {
            return "10.13.13.1:53"; // Unbound via WireGuard
        }
    }
    // Default: Unbound for all UDP traffic
    "10.13.13.1:53"
}

async fn cleanup_sessions(sessions: &SessionTable) {
    let mut sessions_write = sessions.write().await;
    let now = Instant::now();

    sessions_write.retain(|_, session| {
        now.duration_since(session.last_activity) < Duration::from_secs(300)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_upstream_dns_query() {
        // DNS query: QR=0 (query), opcode=0 (standard)
        // Standard DNS header: ID(2) + Flags(2) + QDCOUNT(2) + ANCOUNT(2) + NSCOUNT(2) + ARCOUNT(2)
        // Flags: 0x0100 = standard query, recursion desired
        let dns_query: Vec<u8> = vec![
            0x12, 0x34, // Transaction ID
            0x01, 0x00, // Flags: standard query, RD=1
            0x00, 0x01, // QDCOUNT: 1
            0x00, 0x00, // ANCOUNT: 0
            0x00, 0x00, // NSCOUNT: 0
            0x00, 0x00, // ARCOUNT: 0
        ];
        assert_eq!(detect_upstream(&dns_query), "10.13.13.1:53");
    }

    #[test]
    fn test_detect_upstream_dns_response() {
        // DNS response: QR=1 (response)
        let dns_response: Vec<u8> = vec![
            0x12, 0x34, // Transaction ID
            0x81, 0x80, // Flags: response, RD=1, RA=1
            0x00, 0x01, // QDCOUNT: 1
            0x00, 0x01, // ANCOUNT: 1
            0x00, 0x00, // NSCOUNT: 0
            0x00, 0x00, // ARCOUNT: 0
        ];
        // Still routes to Unbound (default)
        assert_eq!(detect_upstream(&dns_response), "10.13.13.1:53");
    }

    #[test]
    fn test_detect_upstream_short_packet() {
        // Packet too short to be DNS
        let short: Vec<u8> = vec![0x01, 0x02, 0x03];
        assert_eq!(detect_upstream(&short), "10.13.13.1:53");
    }

    #[test]
    fn test_detect_upstream_empty_packet() {
        assert_eq!(detect_upstream(&[]), "10.13.13.1:53");
    }

    // TODO: Add integration test with real UDP socket
    // Test that UDP relay correctly forwards packets to Unbound
    // Requires running Unbound instance or mock UDP server
}
