#!/usr/bin/env python3
"""Verify the exact LunarDepth candidate config and exported rootfs out of band."""

import json
import os
import pathlib
import sys
import tempfile

sys.dont_write_bytecode = True
REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / ".gitlab/scripts"))
sys.path.insert(0, str(REPO_ROOT / "lunardepth4lunarwing/scripts"))

from verify_exposure_out_of_band import validate_archive  # noqa: E402
from worker_candidate_config import fetch_candidate_config  # noqa: E402


def _load_candidate_fetcher():
    return fetch_candidate_config


def main() -> None:
    image_ref = os.environ.get("WORKER_IMAGE_REF", "")
    image_digest = os.environ.get("WORKER_IMAGE_DIGEST", "")
    if not image_ref or not image_digest:
        raise RuntimeError("WORKER_IMAGE_REF and WORKER_IMAGE_DIGEST are required")
    config = _load_candidate_fetcher()(image_ref, image_digest)
    with tempfile.TemporaryDirectory(prefix="lunardepth-oob-") as directory:
        config_path = pathlib.Path(directory) / "image-config.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")
        validate_archive(REPO_ROOT / "lunardepth-rootfs.tar", config_path, REPO_ROOT / "lunardepth4lunarwing")
    print(f"out-of-band LunarDepth verification PASSED for {image_ref}@{image_digest}")


if __name__ == "__main__":
    main()
