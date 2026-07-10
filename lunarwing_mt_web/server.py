"""Flask server for the LunarWing MT Web Onboarding wizard."""

from __future__ import annotations

import argparse
import json

from flask import (
    Flask,
    Response,
    jsonify,
    render_template,
    request,
    stream_with_context,
)

from lunarwing_mt_onboard.config import TenantConfig
from lunarwing_mt_web.bridge import (
    get_session,
    start_provisioning,
    verify_existing,
)

app = Flask(
    __name__,
    template_folder="templates",
    static_folder="static",
)


@app.route("/")
def index():
    return render_template("index.html")


@app.route("/api/validate/name", methods=["POST"])
def validate_name():
    data = request.get_json(force=True)
    name = data.get("name", "")
    err = TenantConfig.validate_name(name)
    return jsonify({"valid": err is None, "error": err})


@app.route("/api/provision", methods=["POST"])
def provision():
    data = request.get_json(force=True)
    try:
        session = start_provisioning(data)
    except (ValueError, RuntimeError) as exc:
        return jsonify({"error": str(exc)}), 400
    return jsonify({"session_id": session.session_id})


@app.route("/api/provision/<session_id>/stream")
def provision_stream(session_id: str):
    session = get_session(session_id)
    if not session:
        return jsonify({"error": "session not found"}), 404

    def generate():
        import queue as q_module

        while True:
            try:
                line = session.output_queue.get(timeout=30)
                yield f"data: {json.dumps(line)}\n\n"
                if line == "[DONE]":
                    break
            except q_module.Empty:
                # Heartbeat keepalive
                yield f"data: {json.dumps('[HEARTBEAT]')}\n\n"

    return Response(
        stream_with_context(generate()),
        mimetype="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )


@app.route("/api/provision/<session_id>/status")
def provision_status(session_id: str):
    session = get_session(session_id)
    if not session:
        return jsonify({"error": "session not found"}), 404
    return jsonify(session.to_dict())


@app.route("/api/verify/<tenant>")
def verify(tenant: str):
    host = request.args.get("host", "127.0.0.1")
    port = int(request.args.get("port", 0))
    results = verify_existing(tenant, host, port)
    return jsonify({"results": results})


def main():
    parser = argparse.ArgumentParser(description="LunarWing MT Web Onboarding")
    parser.add_argument("--port", type=int, default=7424)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()
    app.run(host=args.host, port=args.port, debug=False, threaded=True)


if __name__ == "__main__":
    main()
