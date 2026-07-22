"""FastAPI application: static UI, REST job-start endpoints, and a per-job
WebSocket that streams provisioning events to the browser.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

import re

from fastapi import FastAPI, HTTPException, Query, Request, WebSocket, WebSocketDisconnect
from fastapi.exceptions import RequestValidationError
from fastapi.responses import FileResponse, HTMLResponse, JSONResponse
from fastapi.staticfiles import StaticFiles

from . import __version__, runner
from .jobs import JobConflictError, JobManager
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

    @app.exception_handler(RequestValidationError)
    async def safe_validation_error(
        _request: Request, exc: RequestValidationError
    ) -> JSONResponse:
        detail = [
            {
                "type": error.get("type", "value_error"),
                "loc": error.get("loc", ()),
                "msg": error.get("msg", "invalid request"),
            }
            for error in exc.errors()
        ]
        return JSONResponse(status_code=422, content={"detail": detail})

    def check(provided: str) -> None:
        if not token_matches(token, provided):
            raise HTTPException(
                status_code=401, detail="invalid or missing session token"
            )

    def create_job(mode: str):
        try:
            return manager.create(mode)
        except JobConflictError as exc:
            raise HTTPException(status_code=409, detail=str(exc)) from exc

    # -- static shell -------------------------------------------------------
    _ASSET_RE = re.compile(r'(href|src)="(/static/[^"?]+)"')

    def _versioned_index() -> str:
        """Return index.html with a ?v=<mtime> stamp on every /static asset.

        The SPA's JS/CSS are served at fixed URLs, so a browser will happily
        keep running a cached bundle after we ship a fix (the classic "my
        change doesn't show up" trap). Stamping each asset with the file's
        mtime makes the URL change whenever the file changes, so a reload
        always pulls the current code without disabling caching wholesale.
        """
        html = (STATIC_DIR / "index.html").read_text(encoding="utf-8")

        def stamp(m: "re.Match[str]") -> str:
            attr, path = m.group(1), m.group(2)
            fs_path = STATIC_DIR / path[len("/static/") :]
            try:
                ver = int(fs_path.stat().st_mtime)
            except OSError:
                return m.group(0)
            return f'{attr}="{path}?v={ver}"'

        return _ASSET_RE.sub(stamp, html)

    @app.get("/", response_class=HTMLResponse)
    async def index() -> HTMLResponse:
        return HTMLResponse(
            _versioned_index(),
            headers={"Cache-Control": "no-cache, must-revalidate"},
        )

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
        job = create_job("provision")
        manager.start(job, lambda j: runner.run_provision_job(j, req, log_dir=log_dir))
        return {"job_id": job.id}

    @app.post("/api/upgrade")
    async def start_upgrade(req: UpgradeRequest, token: str = Query("")) -> dict:
        check(token)
        job = create_job("upgrade")
        manager.start(job, lambda j: runner.run_upgrade_job(j, req, log_dir=log_dir))
        return {"job_id": job.id}

    @app.post("/api/export")
    async def start_export(req: ExportRequest, token: str = Query("")) -> dict:
        check(token)
        job = create_job("export")
        manager.start(job, lambda j: runner.run_export_job(j, req, log_dir=log_dir))
        return {"job_id": job.id}

    @app.post("/api/import")
    async def start_import(req: ImportRequest, token: str = Query("")) -> dict:
        check(token)
        job = create_job("import")
        manager.start(job, lambda j: runner.run_import_job(j, req, log_dir=log_dir))
        return {"job_id": job.id}

    @app.post("/api/secrets")
    async def start_secret(req: SecretRequest, token: str = Query("")) -> dict:
        check(token)
        job = create_job("secrets")
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
