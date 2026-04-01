"""UPnP IGD client wrapper for port forwarding and external IP discovery."""

import logging
import socket

import miniupnpc

logger = logging.getLogger(__name__)


class UPnPClient:
    """Wrapper around miniupnpc providing UPnP IGD operations."""

    def __init__(self):
        self._upnp = miniupnpc.UPnP()
        self._upnp.discoverdelay = 2000
        self._available = False

    @property
    def available(self) -> bool:
        return self._available

    def discover(self) -> bool:
        """Discover UPnP gateway via SSDP.

        Returns True if a valid IGD was found, False otherwise.
        """
        try:
            devices = self._upnp.discover()
            logger.info("UPnP discovery found %d device(s)", devices)
            if devices == 0:
                logger.warning("No UPnP devices found on the network")
                self._available = False
                return False

            self._upnp.selectigd()
            logger.info(
                "UPnP IGD selected: %s (status %s, connection type %s)",
                self._upnp.lanaddr,
                self._upnp.statusinfo(),
                self._upnp.connectiontype(),
            )
            self._available = True
            return True
        except Exception:
            logger.exception("UPnP discovery failed")
            self._available = False
            return False

    def get_external_ip(self) -> str | None:
        """Get external IP address via UPnP GetExternalIPAddress."""
        if not self._available:
            logger.debug("UPnP not available; cannot get external IP")
            return None
        try:
            ip = self._upnp.externalipaddress()
            logger.debug("UPnP external IP: %s", ip)
            return ip
        except Exception:
            logger.exception("Failed to get external IP via UPnP")
            return None

    def add_port_mapping(
        self,
        external_port: int,
        internal_ip: str,
        internal_port: int,
        protocol: str = "UDP",
        duration: int = 3600,
        description: str = "vpn-proxy-wg",
    ) -> bool:
        """Add a UPnP port mapping.

        Returns True on success, False on failure.
        """
        if not self._available:
            logger.warning("UPnP not available; skipping port mapping")
            return False
        try:
            result = self._upnp.addportmapping(
                external_port,
                protocol,
                internal_ip,
                internal_port,
                description,
                "",
                duration,
            )
            if result:
                logger.info(
                    "UPnP port mapping added: %s %d -> %s:%d (duration %ds)",
                    protocol,
                    external_port,
                    internal_ip,
                    internal_port,
                    duration,
                )
            else:
                logger.warning(
                    "UPnP addportmapping returned False for %s %d -> %s:%d",
                    protocol,
                    external_port,
                    internal_ip,
                    internal_port,
                )
            return bool(result)
        except Exception:
            logger.exception(
                "Failed to add UPnP port mapping %s %d -> %s:%d",
                protocol,
                external_port,
                internal_ip,
                internal_port,
            )
            return False

    def delete_port_mapping(
        self, external_port: int, protocol: str = "UDP"
    ) -> bool:
        """Delete a UPnP port mapping.

        Returns True on success, False on failure.
        """
        if not self._available:
            return False
        try:
            result = self._upnp.deleteportmapping(external_port, protocol)
            if result:
                logger.info(
                    "UPnP port mapping deleted: %s %d", protocol, external_port
                )
            return bool(result)
        except Exception:
            logger.exception(
                "Failed to delete UPnP port mapping %s %d",
                protocol,
                external_port,
            )
            return False

    def get_lan_ip(self) -> str:
        """Detect own LAN IP address.

        Uses the UPnP-reported LAN address if available, otherwise falls back
        to a UDP socket trick to determine the default route IP.
        """
        if self._available:
            lan_ip = self._upnp.lanaddr
            if lan_ip:
                return lan_ip

        return self._detect_lan_ip_via_socket()

    @staticmethod
    def _detect_lan_ip_via_socket() -> str:
        """Detect LAN IP by opening a UDP socket to a public address."""
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
                s.connect(("8.8.8.8", 80))
                return s.getsockname()[0]
        except Exception:
            logger.warning("Could not detect LAN IP via socket; using 127.0.0.1")
            return "127.0.0.1"
