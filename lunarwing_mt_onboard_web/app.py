"""FastAPI application: static UI, REST job-start endpoints, and a per-job
WebSocket that streams provisioning events to the browser.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

from fastapi import FastAPI, HTTPException, Query, WebSocket, WebSocketDisconnect
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles

from . import __version__, runner
from .jobs import JobManager
from .models import (
    ExportRequest,
    ImportRequest,
    ProvisionRequest,
    SecretRequest,
    UpgradeRequest,
)
from .security import token_matches

PACKAGE_DIR = Path(__file__).resolve().parent
STATIC_DIR = PACKAGE_DIR / "static"
REPO_ROOT = PACKAGE_DIR.parent
LOGO_PATH = REPO_ROOT / "crszsslw2412c09c1-8ea9-46bd-8063-084ccdd2f332.jpg"


def create_app(*, token: str, demo: bool, log_dir: str | None) -> FastAPI:
    app = FastAPI(title="LunarWing MT Onboard - Web", docs_url=None, redoc_url=None)
    manager = JobManager(demo=demo, log_dir=log_dir)
    app.state.token = token

    def check(provided: str) -> None:
        if not token_matches(token, provided):
            raise HTTPException(
                status_code=401, detail="invalid or missing session token"
            )

    # -- static shell -------------------------------------------------------
    @app.get("/")
    async def index() -> FileResponse:
        return FileResponse(STATIC_DIR / "index.html")

    @app.get("/assets/logo.jpg")
    async def logo() -> FileResponse:
        if LOGO_PATH.is_file():
            return FileResponse(LOGO_PATH, media_type="image/jpeg")
        raise HTTPException(status_code=404, detail="logo not found")

    # -- read APIs ----------------------------------------------------------
    @app.get("/api/config")
    async def api_config(token: str = Query("")) -> dict:
        check(token)
        return {"demo": demo, "version": __version__}

    @app.get("/api/tenants")
    async def api_tenants(token: str = Query("")) -> dict:
        check(token)
        return {"tenants": runner.list_tenants(demo)}

    # -- job-start APIs -----------------------------------------------------
    @app.post("/api/provision")
    async def start_provision(req: ProvisionRequest, token: str = Query("")) -> dict:
        check(token)
        job = manager.create("provision")
        manager.start(job, lambda j: runner.run_provision_job(j, req, log_dir=log_dir))
        return {"job_id": job.id}

    @app.post("/api/upgrade")
    async def start_upgrade(req: UpgradeRequest, token: str = Query("")) -> dict:
        check(token)
        job = manager.create("upgrade")
        manager.start(job, lambda j: runner.run_upgrade_job(j, req, log_dir=log_dir))
        return {"job_id": job.id}

    @app.post("/api/export")
    async def start_export(req: ExportRequest, token: str = Query("")) -> dict:
        check(token)
        job = manager.create("export")
        manager.start(job, lambda j: runner.run_export_job(j, req, log_dir=log_dir))
        return {"job_id": job.id}

    @app.post("/api/import")
    async def start_import(req: ImportRequest, token: str = Query("")) -> dict:
        check(token)
        job = manager.create("import")
        manager.start(job, lambda j: runner.run_import_job(j, req, log_dir=log_dir))
        return {"job_id": job.id}

    @app.post("/api/secrets")
    async def start_secret(req: SecretRequest, token: str = Query("")) -> dict:
        check(token)
        job = manager.create("secrets")
        manager.start(job, lambda j: runner.run_secret_job(j, req, log_dir=log_dir))
        return {"job_id": job.id}

    # -- live stream --------------------------------------------------------
    @app.websocket("/api/jobs/{job_id}/ws")
    async def job_ws(ws: WebSocket, job_id: str) -> None:
        if not token_matches(app.state.token, ws.query_params.get("token", "")):
            await ws.close(code=4401)
            return
        await ws.accept()
        job = manager.get(job_id)
        if job is None:
            await ws.send_json({"type": "error", "message": "unknown job id"})
            await ws.close()
            return

        async def receiver() -> None:
            try:
                while True:
                    msg = await ws.receive_json()
                    if isinstance(msg, dict) and msg.get("action") == "cancel":
                        manager.cancel(job_id)
            except Exception:
                pass

        recv_task = asyncio.create_task(receiver())
        try:
            while True:
                event = await asyncio.to_thread(job.queue.get)
                if event is None:
                    break
                await ws.send_json(event)
        except (WebSocketDisconnect, RuntimeError):
            pass
        finally:
            recv_task.cancel()
            try:
                await ws.close()
            except Exception:
                pass

    app.mount("/static", StaticFiles(directory=str(STATIC_DIR)), name="static")
    return app
