"""Net-manager main entry point.

Orchestrates UPnP port forwarding, IP detection, WireGuard config generation,
and the HTTP config server.
"""

import json
import logging
import os
import signal
import sys
import threading
import time
from pathlib import Path

from config_generator import generate_all_configs
from upnp_client import UPnPClient
from web_server import run_server

# ---------------------------------------------------------------------------
# Configuration from environment
# ---------------------------------------------------------------------------
DOCKER_HOST_IP = os.environ.get("DOCKER_HOST_IP", "")
WG_PORT = int(os.environ.get("WG_PORT", "51820"))
WG_CONFIG_DIR = os.environ.get("WG_CONFIG_DIR", "/wg-config")
OUTPUT_DIR = os.environ.get("OUTPUT_DIR", "/clients")
CONFIG_SERVER_PORT = int(os.environ.get("CONFIG_SERVER_PORT", "8088"))
UPNP_LEASE_DURATION = int(os.environ.get("UPNP_LEASE_DURATION", "3600"))
POLL_INTERVAL = int(os.environ.get("POLL_INTERVAL", "30"))

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    stream=sys.stdout,
)
logger = logging.getLogger("net-manager")

# ---------------------------------------------------------------------------
# Shared state (read by the web server, written by the main loop)
# ---------------------------------------------------------------------------
state: dict = {
    "lan_ip": "unknown",
    "wan_ip": None,
    "upnp_available": False,
    "peers": [],
    "output_dir": OUTPUT_DIR,
}

# Shutdown flag
_shutdown = threading.Event()


def _write_network_status(output_dir: str):
    """Persist current network status to a JSON file."""
    status_path = Path(output_dir) / "network-status.json"
    status_path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "lan_ip": state["lan_ip"],
        "wan_ip": state["wan_ip"],
        "upnp_available": state["upnp_available"],
        "wg_port": WG_PORT,
        "peer_count": len(state["peers"]),
        "peers": [
            {"name": p["name"], "address": p.get("address", "")}
            for p in state["peers"]
        ],
    }
    status_path.write_text(json.dumps(payload, indent=2))
    logger.debug("Wrote network status to %s", status_path)


def _regenerate_configs(lan_ip: str, wan_ip: str | None) -> list[dict]:
    """Generate all peer configs and return updated peer list."""
    peers = generate_all_configs(
        wg_config_dir=WG_CONFIG_DIR,
        output_dir=OUTPUT_DIR,
        lan_ip=lan_ip,
        wan_ip=wan_ip,
        wg_port=WG_PORT,
    )
    logger.info(
        "Regenerated configs for %d peer(s) (LAN=%s, WAN=%s)",
        len(peers),
        lan_ip,
        wan_ip,
    )
    return peers


def _ensure_port_mapping(upnp: UPnPClient, lan_ip: str) -> bool:
    """Add or refresh the UPnP port mapping."""
    return upnp.add_port_mapping(
        external_port=WG_PORT,
        internal_ip=lan_ip,
        internal_port=WG_PORT,
        protocol="UDP",
        duration=UPNP_LEASE_DURATION,
    )


def main():
    logger.info("net-manager starting")
    logger.info(
        "Config: WG_PORT=%d, WG_CONFIG_DIR=%s, OUTPUT_DIR=%s, "
        "CONFIG_SERVER_PORT=%d, UPNP_LEASE_DURATION=%d, POLL_INTERVAL=%d",
        WG_PORT,
        WG_CONFIG_DIR,
        OUTPUT_DIR,
        CONFIG_SERVER_PORT,
        UPNP_LEASE_DURATION,
        POLL_INTERVAL,
    )

    upnp = UPnPClient()

    # --- Initial discovery ---
    upnp.discover()
    state["upnp_available"] = upnp.available

    # Determine LAN IP
    if DOCKER_HOST_IP:
        lan_ip = DOCKER_HOST_IP
        logger.info("Using DOCKER_HOST_IP=%s as LAN IP", lan_ip)
    else:
        lan_ip = upnp.get_lan_ip()
        logger.info("Auto-detected LAN IP: %s", lan_ip)
    state["lan_ip"] = lan_ip

    # Determine WAN IP
    wan_ip = upnp.get_external_ip()
    state["wan_ip"] = wan_ip
    if wan_ip:
        logger.info("External (WAN) IP: %s", wan_ip)
    else:
        logger.warning("WAN IP unavailable; WAN configs will not be generated")

    # Initial port mapping
    if upnp.available:
        _ensure_port_mapping(upnp, lan_ip)

    # Generate initial configs
    state["peers"] = _regenerate_configs(lan_ip, wan_ip)
    _write_network_status(OUTPUT_DIR)

    # --- Start web server in background thread ---
    web_thread = threading.Thread(
        target=run_server,
        kwargs={"state": state, "port": CONFIG_SERVER_PORT},
        daemon=True,
    )
    web_thread.start()

    # --- Register signal handlers for graceful shutdown ---
    def _handle_signal(signum, frame):
        logger.info("Received signal %d, shutting down", signum)
        _shutdown.set()

    signal.signal(signal.SIGTERM, _handle_signal)
    signal.signal(signal.SIGINT, _handle_signal)

    # --- Main polling loop ---
    last_lease_time = time.monotonic()
    lease_renew_threshold = max(UPNP_LEASE_DURATION - 120, UPNP_LEASE_DURATION // 2)

    logger.info("Entering main loop (poll every %ds)", POLL_INTERVAL)

    while not _shutdown.is_set():
        _shutdown.wait(timeout=POLL_INTERVAL)
        if _shutdown.is_set():
            break

        changed = False

        # Check LAN IP
        if DOCKER_HOST_IP:
            new_lan_ip = DOCKER_HOST_IP
        else:
            new_lan_ip = upnp.get_lan_ip()

        if new_lan_ip != lan_ip:
            logger.info("LAN IP changed: %s -> %s", lan_ip, new_lan_ip)
            lan_ip = new_lan_ip
            state["lan_ip"] = lan_ip
            changed = True

        # Check WAN IP
        new_wan_ip = upnp.get_external_ip()
        if new_wan_ip != wan_ip:
            if new_wan_ip:
                logger.info("WAN IP changed: %s -> %s", wan_ip, new_wan_ip)
            else:
                logger.warning("WAN IP became unavailable (was %s)", wan_ip)
            wan_ip = new_wan_ip
            state["wan_ip"] = wan_ip
            changed = True

        # Regenerate configs if IPs changed
        if changed:
            state["peers"] = _regenerate_configs(lan_ip, wan_ip)
            if upnp.available:
                _ensure_port_mapping(upnp, lan_ip)
            last_lease_time = time.monotonic()

        # Renew UPnP lease if approaching expiry
        elapsed = time.monotonic() - last_lease_time
        if upnp.available and elapsed >= lease_renew_threshold:
            logger.info("Renewing UPnP port mapping lease")
            _ensure_port_mapping(upnp, lan_ip)
            last_lease_time = time.monotonic()

        # Persist status
        _write_network_status(OUTPUT_DIR)

    # --- Graceful shutdown ---
    logger.info("Shutting down: cleaning up UPnP port mapping")
    upnp.delete_port_mapping(WG_PORT, "UDP")
    logger.info("net-manager stopped")


if __name__ == "__main__":
    main()
