#!/usr/bin/env python3
"""WI#425 (run-109 GW-3) — durable out-of-band opencode2 image verification.

Runs in CI (worker-opencode2-exposure-verify) against the candidate image
BuildKit filesystem export produced by the candidate build, INSTEAD of
trusting the in-Dockerfile assert: the checker and its environment are baked
by the same Dockerfile they verify, so the only durable check is one that
runs outside the image, from the checkout that produced the build args.

Two halves:

1. Filesystem half — run the checkout's
   opencode2.4lunarwing/scripts/exposure_policy_assert.py with
   --image-root <export>, so the installed wrapper and real binary are
   verified under the exported tree (byte-identity against the checkout's
   wrapper, digest anchor, modes, parent dirs, tunnel-marker scan).

2. Image-config half — fetch the candidate image's config blob from the
   registry (by digest, through the job's registry credentials) and refuse:
   an Env entry retargeting OPENCODE_BIN or LUNARWING_REAL_OPENCODE, a
   missing OPENCODE_DISABLE_AUTOUPDATE=1, or any Shell override.

Failures exit 1 with the real cause named (GZ-2 shape).
"""

from __future__ import annotations

import base64
import json
import os
import pathlib
import subprocess
import sys
import urllib.error
import urllib.request

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
LANE = REPO_ROOT / "opencode2.4lunarwing"
ASSERT = LANE / "scripts" / "exposure_policy_assert.py"
EXPORT_DIR_NAME = "opencode2-rootfs-export"


def die(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def registry_token(repo: str) -> str:
    host = os.environ.get("CI_SERVER_HOST", "")
    user = os.environ.get("CI_REGISTRY_USER", "")
    password = os.environ.get("CI_REGISTRY_PASSWORD", "")
    if not (host and user and password and repo):
        die("CI_SERVER_HOST/CI_REGISTRY_USER/CI_REGISTRY_PASSWORD (or the image ref) "
            "are missing — the image-config half cannot fetch the config")
    auth = base64.b64encode(f"{user}:{password}".encode()).decode()
    url = (f"https://{host}/jwt/auth?service=container_registry&"
           f"scope=repository:{repo}:pull")
    request = urllib.request.Request(url, headers={"Authorization": f"Basic {auth}"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            payload = json.load(response)
    except (OSError, ValueError) as error:
        die(f"cannot obtain a registry pull token for {repo}: {error}")
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
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            return json.load(response)
    except (OSError, ValueError) as error:
        die(f"cannot fetch image config {digest} from {repo}: {error}")
    return {}  # unreachable


def check_filesystem_half(export_dir: pathlib.Path) -> None:
    if not (export_dir / "app" / "scripts").is_dir():
        die(f"{export_dir}/app/scripts is missing — the candidate job did not "
            "export the opencode2 filesystem (WI#425)")
    wrapper_anchor = os.environ.get("LUNARWING_WRAPPER_SHA256", "")
    if not wrapper_anchor:
        # Recompute from the checkout: the build script hashed this same file
        # into the build args, so this is the checkout-side anchor.
        import hashlib
        wrapper = LANE / "scripts" / "opencode-exposure-guard.sh"
        wrapper_anchor = hashlib.sha256(wrapper.read_bytes()).hexdigest()
    env = {**os.environ, "LUNARWING_WRAPPER_SHA256": wrapper_anchor}
    result = subprocess.run(
        [
            sys.executable, str(ASSERT),
            "--lane", str(export_dir / "app"),
            "--profile", "opencode2",
            "--image-filesystem",
            "--image-root", str(export_dir),
        ],
        env=env,
        capture_output=True,
        text=True,
        timeout=600,
    )
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    if result.returncode != 0:
        die("out-of-band filesystem verification FAILED (WI#425) — the exported "
            "image tree no longer satisfies the checkout's exposure policy")


def check_config_half(config: dict) -> None:
    env_entries = config.get("config", {}).get("Env") or []
    labels = {entry.split("=", 1)[0]: entry.split("=", 1)[1]
              for entry in env_entries if "=" in entry}
    failures = []
    for key in ("OPENCODE_BIN", "LUNARWING_REAL_OPENCODE"):
        if key in labels:
            failures.append(f"image config Env retargets {key}={labels[key]!r}")
    if labels.get("OPENCODE_DISABLE_AUTOUPDATE") != "1":
        failures.append("image config Env lost OPENCODE_DISABLE_AUTOUPDATE=1 — "
                        "the updater ignores the lane config's update pin")
    shell = config.get("config", {}).get("Shell")
    if shell:
        failures.append(f"image config carries a Shell override {shell!r}")
    for failure in failures:
        print(f"ERROR: {failure} (WI#425)", file=sys.stderr)
    if failures:
        raise SystemExit(1)
    print("out-of-band image-config verify: Env and Shell clean")


def main() -> int:
    image_ref = os.environ.get("WORKER_IMAGE_REF", "")
    image_digest = os.environ.get("WORKER_IMAGE_DIGEST", "")
    if not (image_ref and image_digest):
        die("WORKER_IMAGE_REF/WORKER_IMAGE_DIGEST are missing — source "
            "worker-image.env (the candidate's dotenv artifact) before running")
    export_dir = REPO_ROOT / EXPORT_DIR_NAME

    check_filesystem_half(export_dir)

    registry = os.environ.get("CI_REGISTRY", "")
    prefix = f"{registry}/" if registry else ""
    repo = image_ref[len(prefix):].rsplit(":", 1)[0] if image_ref.startswith(prefix) else image_ref.rsplit(":", 1)[0]

    # The metadata digest is the MANIFEST digest; the config blob digest must
    # come from the manifest. Fetch the manifest first.
    token = registry_token(repo)
    manifest_url = (f"https://{os.environ['CI_REGISTRY']}/v2/{repo}/"
                    f"manifests/{image_digest}")
    request = urllib.request.Request(
        manifest_url,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.docker.distribution.manifest.v2+json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            manifest = json.load(response)
    except (OSError, ValueError) as error:
        die(f"cannot fetch manifest {image_digest} from {repo}: {error}")
    config_digest = manifest.get("config", {}).get("digest")
    if not config_digest:
        die(f"manifest {image_digest} carries no config digest")
    config = image_config(repo, config_digest, token)
    check_config_half(config)

    print(f"out-of-band verification PASSED for {image_ref}@{image_digest} (WI#425)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
