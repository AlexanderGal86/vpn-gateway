"""WireGuard config generator for linuxserver/wireguard peer layout."""

import logging
import os
import re
from pathlib import Path

import qrcode

logger = logging.getLogger(__name__)

# Regex to extract Address from an existing peer conf
_ADDRESS_RE = re.compile(r"^\s*Address\s*=\s*(.+)$", re.MULTILINE)
_ALLOWED_IPS_RE = re.compile(r"^\s*AllowedIPs\s*=\s*(.+)$", re.MULTILINE)
_DNS_RE = re.compile(r"^\s*DNS\s*=\s*(.+)$", re.MULTILINE)

_PEER_DIR_RE = re.compile(r"^peer\d+$")


def scan_peers(wg_config_dir: str) -> list[dict]:
    """Scan for peer directories in the WireGuard config directory.

    The linuxserver/wireguard image stores peers as:
        <wg_config_dir>/peer1/peer1.conf
        <wg_config_dir>/peer1/privatekey-peer1
        <wg_config_dir>/peer1/publickey-peer1

    Returns a list of dicts with keys: name, dir, private_key, public_key,
    address, allowed_ips, dns.
    """
    config_path = Path(wg_config_dir)
    peers = []

    if not config_path.is_dir():
        logger.warning("WireGuard config dir does not exist: %s", wg_config_dir)
        return peers

    for entry in sorted(config_path.iterdir()):
        if not entry.is_dir() or not _PEER_DIR_RE.match(entry.name):
            continue

        peer_name = entry.name
        private_key_file = entry / f"privatekey-{peer_name}"
        public_key_file = entry / f"publickey-{peer_name}"
        conf_file = entry / f"{peer_name}.conf"

        if not private_key_file.is_file():
            logger.warning("Missing private key for %s, skipping", peer_name)
            continue

        private_key = private_key_file.read_text().strip()
        public_key = (
            public_key_file.read_text().strip()
            if public_key_file.is_file()
            else ""
        )

        # Parse existing conf for Address and DNS
        address = ""
        allowed_ips = "0.0.0.0/0, ::/0"
        dns = "10.13.13.1"
        if conf_file.is_file():
            conf_text = conf_file.read_text()
            m = _ADDRESS_RE.search(conf_text)
            if m:
                address = m.group(1).strip()
            m = _ALLOWED_IPS_RE.search(conf_text)
            if m:
                allowed_ips = m.group(1).strip()
            m = _DNS_RE.search(conf_text)
            if m:
                dns = m.group(1).strip()

        peers.append(
            {
                "name": peer_name,
                "dir": str(entry),
                "private_key": private_key,
                "public_key": public_key,
                "address": address,
                "allowed_ips": allowed_ips,
                "dns": dns,
            }
        )

    logger.info("Found %d peer(s) in %s", len(peers), wg_config_dir)
    return peers


def read_server_public_key(wg_config_dir: str) -> str:
    """Read the WireGuard server public key.

    Expected location: <wg_config_dir>/server/publickey
    """
    key_path = Path(wg_config_dir) / "server" / "publickey"
    if not key_path.is_file():
        logger.error("Server public key not found at %s", key_path)
        return ""
    key = key_path.read_text().strip()
    logger.debug("Server public key loaded from %s", key_path)
    return key


def generate_peer_config(
    peer_name: str,
    peer_private_key: str,
    peer_address: str,
    server_public_key: str,
    endpoint: str,
    dns: str = "10.13.13.1",
    allowed_ips: str = "0.0.0.0/0, ::/0",
) -> str:
    """Generate a WireGuard client config string."""
    return (
        f"[Interface]\n"
        f"PrivateKey = {peer_private_key}\n"
        f"Address = {peer_address}\n"
        f"DNS = {dns}\n"
        f"\n"
        f"[Peer]\n"
        f"PublicKey = {server_public_key}\n"
        f"AllowedIPs = {allowed_ips}\n"
        f"Endpoint = {endpoint}\n"
        f"PersistentKeepalive = 25\n"
    )


def generate_all_configs(
    wg_config_dir: str,
    output_dir: str,
    lan_ip: str,
    wan_ip: str | None,
    wg_port: int = 51820,
) -> list[dict]:
    """Generate LAN and WAN configs for all discovered peers.

    Writes config files and QR code PNGs to output_dir/<peer_name>/.
    Returns the list of peer info dicts augmented with config paths.
    """
    server_pubkey = read_server_public_key(wg_config_dir)
    if not server_pubkey:
        logger.error("Cannot generate configs without server public key")
        return []

    peers = scan_peers(wg_config_dir)
    out = Path(output_dir)
    out.mkdir(parents=True, exist_ok=True)

    for peer in peers:
        peer_out = out / peer["name"]
        peer_out.mkdir(parents=True, exist_ok=True)

        # LAN config
        lan_endpoint = f"{lan_ip}:{wg_port}"
        lan_conf = generate_peer_config(
            peer["name"],
            peer["private_key"],
            peer["address"],
            server_pubkey,
            lan_endpoint,
            dns=peer["dns"],
            allowed_ips=peer["allowed_ips"],
        )
        lan_conf_path = peer_out / f"{peer['name']}-lan.conf"
        lan_conf_path.write_text(lan_conf)
        lan_qr_path = peer_out / f"{peer['name']}-lan-qr.png"
        generate_qr(lan_conf, str(lan_qr_path))
        peer["lan_config_path"] = str(lan_conf_path)
        peer["lan_qr_path"] = str(lan_qr_path)

        # WAN config (only if we have a WAN IP)
        if wan_ip:
            wan_endpoint = f"{wan_ip}:{wg_port}"
            wan_conf = generate_peer_config(
                peer["name"],
                peer["private_key"],
                peer["address"],
                server_pubkey,
                wan_endpoint,
                dns=peer["dns"],
                allowed_ips=peer["allowed_ips"],
            )
            wan_conf_path = peer_out / f"{peer['name']}-wan.conf"
            wan_conf_path.write_text(wan_conf)
            wan_qr_path = peer_out / f"{peer['name']}-wan-qr.png"
            generate_qr(wan_conf, str(wan_qr_path))
            peer["wan_config_path"] = str(wan_conf_path)
            peer["wan_qr_path"] = str(wan_qr_path)
        else:
            peer["wan_config_path"] = None
            peer["wan_qr_path"] = None

        logger.info("Generated configs for %s", peer["name"])

    return peers


def generate_qr(config_text: str, output_path: str) -> bool:
    """Generate a QR code PNG from a WireGuard config string.

    Returns True on success, False on failure.
    """
    try:
        qr = qrcode.QRCode(
            version=None,
            error_correction=qrcode.constants.ERROR_CORRECT_M,
            box_size=10,
            border=4,
        )
        qr.add_data(config_text)
        qr.make(fit=True)
        img = qr.make_image(fill_color="black", back_color="white")
        img.save(output_path)
        logger.debug("QR code saved to %s", output_path)
        return True
    except Exception:
        logger.exception("Failed to generate QR code at %s", output_path)
        return False
