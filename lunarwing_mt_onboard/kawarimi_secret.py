"""Ephemeral passphrase transport for Kawarimi subprocesses."""

from __future__ import annotations

import os
import stat
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Iterator

MAX_PASSPHRASE_LENGTH = 1024


def validate_passphrase(value: str, *, min_length: int = 1) -> str | None:
    """Return a validation error for an unsafe passphrase, or ``None``."""
    if not value:
        return "passphrase is required"
    if len(value) < min_length:
        return f"passphrase must be at least {min_length} characters"
    if len(value) > MAX_PASSPHRASE_LENGTH:
        return f"passphrase must be at most {MAX_PASSPHRASE_LENGTH} characters"
    if "\n" in value or "\r" in value:
        return "passphrase must not contain line breaks"
    return None


@dataclass(frozen=True, slots=True)
class PassphraseTransport:
    """Environment and descriptors needed by one child process."""

    env: dict[str, str]
    pass_fds: tuple[int, ...]


@contextmanager
def passphrase_transport(passphrase: str) -> Iterator[PassphraseTransport]:
    """Expose *passphrase* to one POSIX child through an anonymous pipe."""
    inherited_fd = os.environ.get("KAWARIMI_PASS_FD", "")
    inherited_passphrase = os.environ.get("KAWARIMI_PASS", "")
    inherited_file = os.environ.get("KAWARIMI_PASS_FILE", "")

    if not passphrase and inherited_fd:
        if not inherited_fd.isdigit():
            raise ValueError("KAWARIMI_PASS_FD must be a file descriptor number")
        fd = int(inherited_fd)
        try:
            os.fstat(fd)
        except OSError as exc:
            raise ValueError("KAWARIMI_PASS_FD is not open") from exc
        yield PassphraseTransport({"KAWARIMI_PASS_FD": inherited_fd}, (fd,))
        return

    if not passphrase and inherited_passphrase:
        passphrase = inherited_passphrase
    if not passphrase and inherited_file:
        file_stat = os.lstat(inherited_file)
        if not stat.S_ISREG(file_stat.st_mode):
            raise ValueError("KAWARIMI_PASS_FILE must be a regular, non-symlink file")
        if hasattr(os, "geteuid") and file_stat.st_uid != os.geteuid():
            raise ValueError("KAWARIMI_PASS_FILE must be owned by the current user")
        if file_stat.st_mode & 0o077:
            raise ValueError("KAWARIMI_PASS_FILE must not be accessible by group or others")
        with open(inherited_file, encoding="utf-8") as passphrase_file:
            passphrase = passphrase_file.read().rstrip("\n")

    if not passphrase:
        yield PassphraseTransport({}, ())
        return

    error = validate_passphrase(passphrase)
    if error:
        raise ValueError(error)
    if os.name != "posix":
        raise RuntimeError("ephemeral Kawarimi passphrase transport requires POSIX")

    read_fd, write_fd = os.pipe()
    payload = bytearray(passphrase.encode("utf-8") + b"\n")
    try:
        offset = 0
        while offset < len(payload):
            offset += os.write(write_fd, payload[offset:])
    finally:
        os.close(write_fd)
        payload[:] = b"\0" * len(payload)

    try:
        yield PassphraseTransport(
            {"KAWARIMI_PASS_FD": str(read_fd)},
            (read_fd,),
        )
    finally:
        os.close(read_fd)
