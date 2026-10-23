#!/usr/bin/env python3
"""Bind the WI#454 task-runner socket as root, then exec the runner (NL-7).

Usage (root launcher only, see run_task_runner in entrypoint.sh):

    python3 -I task_runner_bind.py SOCKET_PATH -- PROGRAM [ARGS...]

!1873 run 200 NL-7: the runner used to bind its own socket in a directory
the task uid owned, so any task (same uid) could replace the socket and
receive the next task's id and prompt, with its forged result accepted. Here
the launcher, still root, binds the listening socket in a directory only its
own uid can enter (0700, owned by the euid), and PROGRAM (setpriv -> env ->
node task_runner.js) inherits just the listening descriptor, announced in
LUNARWING_TASK_RUNNER_LISTEN_FD. The task uid can neither replace the path
nor connect to it; only root (the bridge) connects.

This process never stays resident: it execs PROGRAM, so the runner keeps
this pid (NL-4: the launcher's $! is the runner itself).

Residual: the runner and every task share one uid, so task code that may
ptrace the runner (or pidfd_getfd its descriptor) can still take the
listener over. Only the two-container split (WI#454) removes that.
"""

import os
import socket
import stat
import sys

LISTEN_FD = 3
BACKLOG = 64


def refuse(message: str) -> None:
    print(f"task_runner_bind: {message}", file=sys.stderr)
    sys.exit(1)


def prepare_directory(directory: str) -> None:
    euid = os.geteuid()
    try:
        os.mkdir(directory, 0o700)
    except FileExistsError:
        pass
    st = os.lstat(directory)
    if not stat.S_ISDIR(st.st_mode):
        refuse(f"{directory} is not a directory (symlinks are refused)")
    if st.st_uid != euid:
        refuse(f"{directory} is owned by uid {st.st_uid}, expected {euid}")
    if st.st_mode & 0o077:
        refuse(f"{directory} must not be accessible to group or others (mode {stat.S_IMODE(st.st_mode):o})")


def bind_listener(path: str) -> socket.socket:
    try:
        existing = os.lstat(path)
    except FileNotFoundError:
        existing = None
    if existing is not None:
        if not stat.S_ISSOCK(existing.st_mode):
            refuse(f"{path} exists and is not a socket")
        # A previous runner generation's socket: nothing listens on it now.
        os.unlink(path)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    previous = os.umask(0o177)
    try:
        listener.bind(path)
    finally:
        os.umask(previous)
    listener.listen(BACKLOG)
    return listener


def main(argv: list[str]) -> None:
    if len(argv) < 3 or argv[1] != "--":
        refuse("usage: task_runner_bind.py SOCKET_PATH -- PROGRAM [ARGS...]")
    path, program = argv[0], argv[2:]
    if not os.path.isabs(path):
        refuse(f"socket path {path!r} must be absolute")
    if not os.path.isabs(program[0]):
        refuse(f"program {program[0]!r} must be an absolute path (no PATH lookup as root)")
    prepare_directory(os.path.dirname(path))
    listener = bind_listener(path)
    if listener.fileno() != LISTEN_FD:
        os.dup2(listener.fileno(), LISTEN_FD)
    # Python creates every descriptor close-on-exec; the runner needs this
    # one across the exec chain (setpriv and env keep inherited descriptors).
    os.set_inheritable(LISTEN_FD, True)
    env = dict(os.environ)
    env["LUNARWING_TASK_RUNNER_LISTEN_FD"] = str(LISTEN_FD)
    os.execve(program[0], program, env)


if __name__ == "__main__":
    main(sys.argv[1:])
