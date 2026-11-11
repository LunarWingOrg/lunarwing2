#!/usr/bin/env python3
"""Render or verify LunarDepth's non-secret effective overlay."""

import argparse
import ipaddress
import os
import re
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

PLACEHOLDER_URL = "https://set-lunardepth-base-url.invalid/openai/v1"
PLACEHOLDER_MODEL = "tensormoon::function_name::lunarwing"
MODEL_PATTERN = re.compile(r"^tensormoon::[A-Za-z0-9_-]+::lunarwing$")


def validate_route(base_url: str, model: str, allow_insecure: bool) -> None:
    if not MODEL_PATTERN.fullmatch(model):
        raise ValueError("model must have the tensormoon::<function_name>::lunarwing form")
    if any(ord(character) < 0x21 or ord(character) == 0x7F for character in base_url):
        raise ValueError("base URL must not contain whitespace or control characters")
    parsed = urlsplit(base_url)
    if parsed.scheme not in {"https", "http"} or not parsed.hostname:
        raise ValueError("base URL must be an absolute HTTP(S) URL")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("base URL must not contain credentials, query, or fragment")
    if parsed.path.rstrip("/").endswith("/chat/completions"):
        raise ValueError("base URL must be the API root, not /chat/completions")
    if "set-lunardepth-base-url.invalid" in (parsed.hostname or ""):
        raise ValueError("the placeholder base URL cannot start a worker")
    host = (parsed.hostname or "").lower()
    loopback = host == "localhost" or ipaddress.ip_address(host).is_loopback if _is_ip(host) else host == "localhost"
    if parsed.scheme == "http" and not loopback and not allow_insecure:
        raise ValueError("cleartext HTTP beyond loopback requires LUNARDEPTH_ALLOW_INSECURE_HTTP=true")


def _is_ip(host: str) -> bool:
    try:
        ipaddress.ip_address(host)
        return True
    except ValueError:
        return False


def yaml_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def render(template: str, base_url: str, model: str, allow_insecure: bool) -> str:
    validate_route(base_url, model, allow_insecure)
    if template.count(PLACEHOLDER_URL) != 1 or template.count(PLACEHOLDER_MODEL) != 3:
        raise ValueError("overlay template placeholders do not match the reviewed pin")
    rendered = template.replace(PLACEHOLDER_URL, yaml_string(base_url)).replace(PLACEHOLDER_MODEL, model)
    expected = "allowInsecureHTTP: true" if allow_insecure else "allowInsecureHTTP: false"
    rendered = rendered.replace("allowInsecureHTTP: false", expected)
    if "baseURLEnv:" in rendered or "AGENT_AUTH_TOKEN" in rendered or "LUNARDEPTH_API_KEY:" in rendered:
        raise ValueError("overlay contains a forbidden environment or credential value")
    return rendered


def write_atomic(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o555)
    if path.parent.is_symlink() or not path.parent.is_dir():
        raise ValueError("overlay parent must be a real directory")
    if path.exists() and (path.is_symlink() or not path.is_file()):
        raise ValueError("overlay output must be a regular non-symlink file")
    descriptor, temporary = tempfile.mkstemp(prefix=".lunardepth-overlay-", dir=path.parent)
    try:
        os.fchmod(descriptor, 0o444)
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o444)
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        Path(temporary).unlink(missing_ok=True)
        raise


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--template", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--check", type=Path)
    parser.add_argument(
        "--stdout",
        action="store_true",
        help="print the rendered overlay; mt-admin runs this inside the worker image and stores the bytes",
    )
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--allow-insecure-http", action="store_true")
    args = parser.parse_args()
    if sum((args.check is not None, args.output is not None, args.stdout)) != 1:
        parser.error("choose exactly one of --check, --output or --stdout")
    template = args.template.read_text(encoding="utf-8")
    rendered = render(template, args.base_url, args.model, args.allow_insecure_http)
    if args.check is not None:
        if args.check.read_text(encoding="utf-8") != rendered:
            raise ValueError(f"effective overlay drift: {args.check}")
        return
    if args.stdout:
        # Bytes, not text: --check compares against a UTF-8 file, so the host
        # must receive exactly what write_atomic would have stored.
        sys.stdout.buffer.write(rendered.encode("utf-8"))
        sys.stdout.buffer.flush()
        return
    write_atomic(args.output, rendered)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        print(f"lunardepth-overlay: {error}", file=sys.stderr)
        sys.exit(1)
