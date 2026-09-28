#!/usr/bin/env python3
"""Admit a relay consumer and reload its password at the exec boundary.

The shared lock is close-on-exec: rotation takes the exclusive lock BEFORE
stopping consumers, so every admitted process is covered by the subsequent
init stop. Starts during rotation fail immediately (including supervisor
respawns). No lock is inherited by daemon workloads or a detached tmux server.
"""

import argparse
import fcntl
import os
import stat
import sys


def private_file(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    info = os.fstat(fd)
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1):
        os.close(fd)
        raise ValueError("unsafe relay file")
    return fd


def relay_password(path):
    with os.fdopen(private_file(path), encoding="utf-8") as source:
        passwords = [line.rstrip("\r\n").split("=", 1)[1]
                     for line in source if line.startswith("RELAY_PASSWORD=")]
    if len(passwords) != 1:
        raise ValueError("missing or duplicate relay password")
    value = passwords[0]
    if value.startswith(('"', "'")):
        if len(value) < 2 or value[-1] != value[0]:
            raise ValueError("invalid relay password quoting")
        value = value[1:-1]
    if not value or "\0" in value:
        raise ValueError("empty or invalid relay password")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--env-file", required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command and command[0] == "--":
        command = command[1:]
    if not command or not os.path.isabs(command[0]):
        parser.error("an absolute command is required after --")

    # Like the OpenRC env launcher, never open tenant-controlled data as root.
    if os.geteuid() == 0 or not os.path.isabs(args.env_file):
        raise ValueError("relay launcher must run as the tenant")
    parent = os.path.dirname(args.env_file)
    info = os.lstat(parent)
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) != 0o700):
        raise ValueError("unsafe relay environment directory")

    lock_fd = private_file(os.path.join(parent, ".relay-start.lock"))
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
        if os.path.lexists(os.path.join(parent, ".relay-rotation-blocked")):
            raise ValueError("relay rotation requires recovery")
        # Init may have read EnvironmentFile before the rotation, or tmux may
        # retain an older environment. Always read this value AFTER admission.
        os.environ["RELAY_PASSWORD"] = relay_password(args.env_file)
        os.execvpe(command[0], command, os.environ)
    finally:
        os.close(lock_fd)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError):
        # Never echo file contents, environment, or the failed command.
        print("error: relay start blocked; check rotation state and credential files", file=sys.stderr)
        sys.exit(75)
