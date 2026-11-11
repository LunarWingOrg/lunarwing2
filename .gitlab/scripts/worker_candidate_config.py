#!/usr/bin/env python3
"""Fetch a protected worker candidate's image config from its registry."""

from __future__ import annotations

import base64
import json
import os
import sys
import time
import urllib.error
import urllib.request

REGISTRY_RETRY_DELAYS = (15, 30, 60)
TRANSIENT_HTTP_STATUSES = frozenset((429, 500, 502, 503, 504))


def die(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def registry_json(
    request: urllib.request.Request,
    *,
    timeout: int,
    description: str,
) -> dict:
    attempt = 0
    while True:
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            status = error.code
            error.close()
            if status not in TRANSIENT_HTTP_STATUSES or attempt == len(REGISTRY_RETRY_DELAYS):
                die(f"{description}: HTTP Error {status}: {error.reason}")
            delay = REGISTRY_RETRY_DELAYS[attempt]
            attempt += 1
            print(
                f"WARNING: {description}: HTTP {status}; retrying in {delay} seconds",
                file=sys.stderr,
            )
            time.sleep(delay)
        except (OSError, ValueError) as error:
            die(f"{description}: {error}")


def registry_token(repo: str) -> str:
    host = os.environ.get("CI_SERVER_HOST", "")
    user = os.environ.get("CI_REGISTRY_USER", "")
    password = os.environ.get("CI_REGISTRY_PASSWORD", "")
    if not (host and user and password and repo):
        die(
            "CI_SERVER_HOST/CI_REGISTRY_USER/CI_REGISTRY_PASSWORD (or the image ref) "
            "are missing — the image-config half cannot fetch the config"
        )
    auth = base64.b64encode(f"{user}:{password}".encode()).decode()
    url = (
        f"https://{host}/jwt/auth?service=container_registry&"
        f"scope=repository:{repo}:pull"
    )
    request = urllib.request.Request(url, headers={"Authorization": f"Basic {auth}"})
    payload = registry_json(
        request,
        timeout=60,
        description=f"cannot obtain a registry pull token for {repo}",
    )
    token = payload.get("token")
    if not token:
        die(f"registry auth response for {repo} carried no token")
    return token


def image_config(repo: str, digest: str, token: str) -> dict:
    registry = os.environ.get("CI_REGISTRY", "")
    if not registry:
        die("CI_REGISTRY is missing")
    url = f"https://{registry}/v2/{repo}/blobs/{digest}"
    request = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
    return registry_json(
        request,
        timeout=120,
        description=f"cannot fetch image config {digest} from {repo}",
    )


def fetch_candidate_config(
    image_ref: str,
    image_digest: str,
    *,
    token_fetcher=registry_token,
    config_fetcher=image_config,
    json_fetcher=registry_json,
) -> dict:
    registry = os.environ.get("CI_REGISTRY", "")
    prefix = f"{registry}/" if registry else ""
    repo = image_ref[len(prefix):].rsplit(":", 1)[0] if image_ref.startswith(prefix) else image_ref.rsplit(":", 1)[0]
    token = token_fetcher(repo)
    manifest_url = f"https://{os.environ['CI_REGISTRY']}/v2/{repo}/manifests/{image_digest}"
    request = urllib.request.Request(
        manifest_url,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.docker.distribution.manifest.v2+json",
        },
    )
    manifest = json_fetcher(
        request,
        timeout=120,
        description=f"cannot fetch manifest {image_digest} from {repo}",
    )
    config_digest = manifest.get("config", {}).get("digest")
    if not config_digest:
        die(f"manifest {image_digest} carries no config digest")
    return config_fetcher(repo, config_digest, token)
