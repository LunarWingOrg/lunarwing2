"""Background job model bridging the blocking runners to the async WebSocket.

Each job runs its (blocking) provisioning work on a daemon thread. The thread
pushes structured event dicts onto a thread-safe ``queue.Queue``; the WebSocket
handler drains that queue with ``asyncio.to_thread`` and forwards to the client.
A ``None`` sentinel marks end-of-stream.
"""

from __future__ import annotations

import os
import queue
import secrets
import signal
import subprocess
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

Event = dict[str, Any]
_SENTINEL: None = None
_TERMINATE_GRACE_SECONDS = 1.0
_TERMINATING_GROUPS: set[int] = set()
_TERMINATING_LOCK = threading.Lock()


def terminate_process_tree(proc: subprocess.Popen) -> None:
    """Terminate a script and descendants started in its POSIX session."""
    if os.name != "posix":
        if proc.poll() is None:
            proc.terminate()
        return

    process_group = proc.pid
    with _TERMINATING_LOCK:
        if process_group in _TERMINATING_GROUPS:
            return
        _TERMINATING_GROUPS.add(process_group)

    try:
        os.killpg(process_group, signal.SIGTERM)
    except ProcessLookupError:
        _discard_terminating_group(process_group)
        return
    except OSError:
        _discard_terminating_group(process_group)
        if proc.poll() is None:
            proc.terminate()
        return

    threading.Thread(
        target=_kill_process_group_after_grace,
        args=(process_group,),
        name=f"kill-process-group-{process_group}",
        daemon=True,
    ).start()


def _kill_process_group_after_grace(process_group: int) -> None:
    try:
        time.sleep(_TERMINATE_GRACE_SECONDS)
        try:
            os.killpg(process_group, 0)
        except ProcessLookupError:
            return
        os.killpg(process_group, signal.SIGKILL)
    except OSError:
        pass
    finally:
        _discard_terminating_group(process_group)


def _discard_terminating_group(process_group: int) -> None:
    with _TERMINATING_LOCK:
        _TERMINATING_GROUPS.discard(process_group)


@dataclass
class Job:
    id: str
    mode: str
    demo: bool
    queue: "queue.Queue[Event | None]" = field(default_factory=queue.Queue)
    stop_event: threading.Event = field(default_factory=threading.Event)
    thread: threading.Thread | None = None
    proc: subprocess.Popen | None = None
    status: str = "pending"  # pending | running | done | error | cancelled
    ok: bool | None = None

    def emit(self, **event: Any) -> None:
        self.queue.put(event)

    def cancelled(self) -> bool:
        return self.stop_event.is_set()


class JobManager:
    """Creates, runs, tracks, and cancels jobs."""

    def __init__(self, *, demo: bool = False, log_dir: str | None = None) -> None:
        self.demo = demo
        self.log_dir = log_dir
        self._jobs: dict[str, Job] = {}
        self._lock = threading.Lock()

    def create(self, mode: str) -> Job:
        job = Job(id=secrets.token_hex(6), mode=mode, demo=self.demo)
        with self._lock:
            self._jobs[job.id] = job
        return job

    def get(self, job_id: str) -> Job | None:
        return self._jobs.get(job_id)

    def start(self, job: Job, target: Callable[[Job], None]) -> None:
        def run() -> None:
            job.status = "running"
            try:
                target(job)
            except Exception as exc:  # surface any unexpected error to the client
                job.ok = False
                job.status = "error"
                job.emit(type="error", message=str(exc))
            finally:
                if job.status == "running":
                    job.status = "done"
                job.queue.put(_SENTINEL)

        t = threading.Thread(target=run, name=f"job-{job.id}", daemon=True)
        job.thread = t
        t.start()

    def cancel(self, job_id: str) -> bool:
        job = self._jobs.get(job_id)
        if not job:
            return False
        job.stop_event.set()
        proc = job.proc
        if proc is not None:
            terminate_process_tree(proc)
        return True
