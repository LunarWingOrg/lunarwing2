#!/usr/bin/env python3

import argparse
import base64
import hashlib
import json
import os
import pathlib
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone


DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
DOTENV_KEY_RE = re.compile(r"^[A-Z][A-Z0-9_]*$")
RELEASE_TAG_RE = re.compile(r"^v\d+\.\d+\.\d+\.\d+(?:-rc[1-9]\d*)?$")
MANIFEST_ACCEPT = ", ".join(
    (
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.v2+json",
    )
)


def required_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"required environment variable {name} is missing")
    return value


def candidate_metadata() -> dict[str, str]:
    allowed = {
        "WORKER_CANDIDATE_TAG",
        "WORKER_IMAGE_DIGEST",
        "WORKER_IMAGE_NAME",
        "WORKER_IMAGE_REF",
        "WORKER_REBUILD_DIGEST",
        "WORKER_REBUILD_REF",
    }
    values: dict[str, str] = {}
    for line in pathlib.Path("worker-image.env").read_text(encoding="utf-8").splitlines():
        key, separator, value = line.partition("=")
        if not separator or not DOTENV_KEY_RE.fullmatch(key) or key not in allowed:
            raise RuntimeError("candidate metadata contains an invalid field")
        if key in values or not value:
            raise RuntimeError(f"candidate metadata has invalid {key}")
        values[key] = value
    missing = allowed - values.keys()
    if missing:
        raise RuntimeError(f"candidate metadata is missing {sorted(missing)}")
    return values


def split_image_ref(registry: str, image_ref: str) -> tuple[str, str]:
    prefix = f"{registry}/"
    if not image_ref.startswith(prefix) or ":" not in image_ref.removeprefix(prefix):
        raise RuntimeError("candidate image is outside CI_REGISTRY or has no tag")
    repository, tag = image_ref.removeprefix(prefix).rsplit(":", 1)
    if not repository or not tag:
        raise RuntimeError("candidate image reference is malformed")
    return repository, tag


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return f"sha256:{digest.hexdigest()}"


class RegistryClient:
    def __init__(self, registry: str, repository: str, username: str, password: str):
        self.base = f"https://{registry}/v2/{urllib.parse.quote(repository, safe='/')}"
        self.repository = repository
        self.basic_auth = base64.b64encode(f"{username}:{password}".encode()).decode()
        self.token: str | None = None

    def _login(self, challenge: str) -> None:
        fields = dict(re.findall(r'(\w+)="([^"]+)"', challenge))
        realm = fields.get("realm")
        if not realm:
            raise RuntimeError("registry authentication challenge has no realm")
        query = urllib.parse.urlencode(
            {
                "service": fields.get("service", "container_registry"),
                "scope": f"repository:{self.repository}:pull,push",
            }
        )
        request = urllib.request.Request(
            f"{realm}?{query}", headers={"Authorization": f"Basic {self.basic_auth}"}
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            payload = json.load(response)
        self.token = payload.get("token") or payload.get("access_token")
        if not self.token:
            raise RuntimeError("registry token response contained no token")

    def request(
        self,
        path: str,
        *,
        method: str = "GET",
        data: bytes | None = None,
        headers: dict[str, str] | None = None,
        missing_ok: bool = False,
    ) -> tuple[bytes, dict[str, str]] | None:
        request_headers = dict(headers or {})
        if self.token:
            request_headers["Authorization"] = f"Bearer {self.token}"
        request = urllib.request.Request(
            f"{self.base}/{path}", data=data, method=method, headers=request_headers
        )
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                return response.read(), dict(response.headers.items())
        except urllib.error.HTTPError as error:
            if error.code == 404 and missing_ok:
                return None
            if error.code != 401 or self.token:
                raise
            self._login(error.headers.get("WWW-Authenticate", ""))
            return self.request(
                path, method=method, data=data, headers=headers, missing_ok=missing_ok
            )

    def manifest(self, reference: str, *, missing_ok: bool = False):
        ref = urllib.parse.quote(reference, safe=":")
        return self.request(
            f"manifests/{ref}", headers={"Accept": MANIFEST_ACCEPT}, missing_ok=missing_ok
        )


def validate_pipeline_context() -> tuple[str, str]:
    source_sha = required_env("CI_COMMIT_SHA")
    release_tag = required_env("CI_COMMIT_TAG")
    if os.environ.get("CI_PIPELINE_SOURCE") != "push":
        raise RuntimeError("promotion requires a push-created tag pipeline")
    if os.environ.get("CI_COMMIT_REF_PROTECTED") != "true":
        raise RuntimeError("promotion requires a protected ref")
    if not RELEASE_TAG_RE.fullmatch(release_tag):
        raise RuntimeError(f"release tag {release_tag!r} does not match release policy")
    return source_sha, release_tag


def verify_inputs(source_sha: str) -> list[str]:
    lines = pathlib.Path("worker-inputs.sha256").read_text(encoding="utf-8").splitlines()
    hashes = [line for line in lines if re.match(r"^[0-9a-f]{64}\s", line)]
    if not hashes or f"source_sha  {source_sha}" not in lines:
        raise RuntimeError("candidate input manifest is incomplete or for another commit")
    return lines


def verify_scan(expected_target: str) -> dict:
    scan_path = pathlib.Path("worker-vulnerabilities.json")
    sbom_path = pathlib.Path("worker-sbom.cdx.json")
    scan = json.loads(scan_path.read_text(encoding="utf-8"))
    json.loads(sbom_path.read_text(encoding="utf-8"))
    if scan.get("ArtifactName") != expected_target:
        raise RuntimeError("vulnerability report targets a different image")
    critical = sum(
        1
        for result in scan.get("Results") or []
        for vulnerability in result.get("Vulnerabilities") or []
        if vulnerability.get("Severity") == "CRITICAL"
    )
    if critical:
        raise RuntimeError(f"image scan contains {critical} critical vulnerabilities")
    return {
        "critical_vulnerabilities": critical,
        "vulnerability_report": sha256_file(scan_path),
        "sbom": sha256_file(sbom_path),
    }


def verify_image_config(config: dict, source_sha: str) -> dict:
    runtime = config.get("config") or {}
    healthcheck = runtime.get("Healthcheck") or {}
    if not healthcheck.get("Test"):
        raise RuntimeError("candidate image has no health check")
    if not runtime.get("Entrypoint"):
        raise RuntimeError("candidate image has no entrypoint")
    labels = runtime.get("Labels") or {}
    if labels.get("org.opencontainers.image.revision") != source_sha:
        raise RuntimeError("candidate image revision label does not match CI_COMMIT_SHA")
    return {
        "rootfs_diff_ids": config.get("rootfs", {}).get("diff_ids", []),
        "entrypoint": runtime["Entrypoint"],
        "healthcheck": healthcheck["Test"],
        "user": runtime.get("User", ""),
    }


def promote() -> None:
    source_sha, release_tag = validate_pipeline_context()
    registry = required_env("CI_REGISTRY")
    metadata = candidate_metadata()
    candidate_ref = metadata["WORKER_IMAGE_REF"]
    expected_digest = metadata["WORKER_IMAGE_DIGEST"]
    rebuild_digest = metadata["WORKER_REBUILD_DIGEST"]
    if not DIGEST_RE.fullmatch(expected_digest):
        raise RuntimeError("WORKER_IMAGE_DIGEST is malformed")
    if rebuild_digest != expected_digest:
        raise RuntimeError("clean rebuild did not reproduce the candidate digest")
    repository, candidate_tag = split_image_ref(registry, candidate_ref)
    expected_tag = rf"{re.escape(source_sha)}-{re.escape(required_env('CI_PIPELINE_ID'))}-[1-9]\d*"
    if not re.fullmatch(expected_tag, candidate_tag):
        raise RuntimeError("candidate tag does not belong to this pipeline and source commit")
    if metadata["WORKER_CANDIDATE_TAG"] != candidate_tag:
        raise RuntimeError("candidate tag metadata is inconsistent")
    if repository.rsplit("/", 1)[-1] != metadata["WORKER_IMAGE_NAME"]:
        raise RuntimeError("candidate repository does not match its worker name")
    if metadata["WORKER_REBUILD_REF"] != f"{candidate_ref}-rebuild":
        raise RuntimeError("clean rebuild reference is inconsistent")
    client = RegistryClient(
        registry,
        repository,
        required_env("CI_REGISTRY_USER"),
        required_env("CI_REGISTRY_PASSWORD"),
    )

    manifest_result = client.manifest(candidate_tag)
    if manifest_result is None:
        raise RuntimeError("candidate manifest was not found")
    manifest_body, manifest_headers = manifest_result
    actual_digest = manifest_headers.get("Docker-Content-Digest")
    if actual_digest != expected_digest:
        raise RuntimeError(f"candidate digest changed: {actual_digest} != {expected_digest}")
    manifest = json.loads(manifest_body)
    config_digest = manifest.get("config", {}).get("digest")
    if not isinstance(config_digest, str) or not DIGEST_RE.fullmatch(config_digest):
        raise RuntimeError("candidate is not a single-platform image manifest")
    config_result = client.request(f"blobs/{urllib.parse.quote(config_digest, safe=':')}")
    if config_result is None:
        raise RuntimeError("candidate config blob was not found")
    config_body, _ = config_result
    config_summary = verify_image_config(json.loads(config_body), source_sha)
    input_lines = verify_inputs(source_sha)
    scan_summary = verify_scan(f"{candidate_ref}@{expected_digest}")

    existing = client.manifest(release_tag, missing_ok=True)
    if existing is not None:
        existing_digest = existing[1].get("Docker-Content-Digest")
        if existing_digest != expected_digest:
            raise RuntimeError(
                f"refusing to retarget existing {release_tag}: "
                f"{existing_digest} != {expected_digest}"
            )
    else:
        content_type = manifest_headers.get("Content-Type", MANIFEST_ACCEPT.split(", ")[0])
        response = client.request(
            f"manifests/{urllib.parse.quote(release_tag)}",
            method="PUT",
            data=manifest_body,
            headers={"Content-Type": content_type},
        )
        if response is None or response[1].get("Docker-Content-Digest") != expected_digest:
            raise RuntimeError("registry returned an unexpected promoted digest")

    evidence = {
        "candidate": candidate_ref,
        "digest": expected_digest,
        "image": f"{registry}/{repository}:{release_tag}",
        "inputs": input_lines,
        "pipeline_id": required_env("CI_PIPELINE_ID"),
        "pipeline_url": os.environ.get("CI_PIPELINE_URL", ""),
        "promoted_at": datetime.now(timezone.utc).isoformat(),
        "release_tag": release_tag,
        "rebuild_digest": rebuild_digest,
        "source_sha": source_sha,
        "config": config_summary,
        "scan": scan_summary,
    }
    pathlib.Path("worker-image-evidence.json").write_text(
        json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"Promoted {candidate_ref}@{expected_digest} to immutable tag {release_tag}")


def self_test() -> None:
    assert split_image_ref("registry.example", "registry.example/group/image:tag") == (
        "group/image",
        "tag",
    )
    for valid in ("v2.1.0.0", "v2.1.0.0-rc1", "v12.34.56.78-rc99"):
        assert RELEASE_TAG_RE.fullmatch(valid)
    for invalid in ("v2.1.0", "v2.1.0.0-rc0", "latest", "v2.1.0.0-beta1"):
        assert not RELEASE_TAG_RE.fullmatch(invalid)
    print("worker image promotion self-tests passed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("promote", "self-test"))
    args = parser.parse_args()
    try:
        self_test() if args.command == "self-test" else promote()
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
