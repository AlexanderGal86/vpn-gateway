"""Flask web server for serving WireGuard configs and network status."""

import logging
import os
from pathlib import Path

from flask import Flask, abort, jsonify, render_template, send_file

logger = logging.getLogger(__name__)


def create_app(state: dict) -> Flask:
    """Create and configure the Flask application.

    Args:
        state: Shared mutable dict holding runtime state:
            - lan_ip: str
            - wan_ip: str | None
            - upnp_available: bool
            - peers: list[dict]
            - output_dir: str
    """
    app = Flask(
        __name__,
        template_folder=os.path.join(os.path.dirname(__file__), "templates"),
    )

    @app.route("/")
    def index():
        return render_template(
            "status.html",
            lan_ip=state.get("lan_ip", "unknown"),
            wan_ip=state.get("wan_ip"),
            upnp_available=state.get("upnp_available", False),
            peers=state.get("peers", []),
        )

    @app.route("/status")
    def status():
        return jsonify(
            {
                "lan_ip": state.get("lan_ip", "unknown"),
                "wan_ip": state.get("wan_ip"),
                "upnp_available": state.get("upnp_available", False),
                "peer_count": len(state.get("peers", [])),
            }
        )

    @app.route("/peers")
    def peers_list():
        peers = state.get("peers", [])
        result = []
        for p in peers:
            result.append(
                {
                    "name": p["name"],
                    "address": p.get("address", ""),
                    "has_lan_config": p.get("lan_config_path") is not None,
                    "has_wan_config": p.get("wan_config_path") is not None,
                }
            )
        return jsonify(result)

    @app.route("/peers/<name>/lan")
    def peer_lan_config(name: str):
        peer = _find_peer(state, name)
        if not peer:
            abort(404, description=f"Peer '{name}' not found")
        config_path = peer.get("lan_config_path")
        if not config_path or not Path(config_path).is_file():
            abort(404, description=f"LAN config for '{name}' not available")
        return send_file(
            config_path,
            mimetype="text/plain",
            as_attachment=True,
            download_name=f"{name}-lan.conf",
        )

    @app.route("/peers/<name>/wan")
    def peer_wan_config(name: str):
        peer = _find_peer(state, name)
        if not peer:
            abort(404, description=f"Peer '{name}' not found")
        config_path = peer.get("wan_config_path")
        if not config_path or not Path(config_path).is_file():
            abort(
                404,
                description=f"WAN config for '{name}' not available (no external IP)",
            )
        return send_file(
            config_path,
            mimetype="text/plain",
            as_attachment=True,
            download_name=f"{name}-wan.conf",
        )

    @app.route("/peers/<name>/lan/qr")
    def peer_lan_qr(name: str):
        peer = _find_peer(state, name)
        if not peer:
            abort(404, description=f"Peer '{name}' not found")
        qr_path = peer.get("lan_qr_path")
        if not qr_path or not Path(qr_path).is_file():
            abort(404, description=f"LAN QR code for '{name}' not available")
        return send_file(qr_path, mimetype="image/png")

    @app.route("/peers/<name>/wan/qr")
    def peer_wan_qr(name: str):
        peer = _find_peer(state, name)
        if not peer:
            abort(404, description=f"Peer '{name}' not found")
        qr_path = peer.get("wan_qr_path")
        if not qr_path or not Path(qr_path).is_file():
            abort(
                404,
                description=f"WAN QR code for '{name}' not available (no external IP)",
            )
        return send_file(qr_path, mimetype="image/png")

    return app


def _find_peer(state: dict, name: str) -> dict | None:
    """Look up a peer by name in the shared state."""
    for p in state.get("peers", []):
        if p["name"] == name:
            return p
    return None


def run_server(state: dict, host: str = "0.0.0.0", port: int = 8088):
    """Run the Flask server (blocking). Intended for use in a thread."""
    app = create_app(state)
    logger.info("Starting config web server on %s:%d", host, port)
    app.run(host=host, port=port, threaded=True, use_reloader=False)
