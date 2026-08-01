#!/usr/bin/env python3

import pathlib
import re
import sys
import tomllib

import yaml


ROOT = pathlib.Path(__file__).resolve().parents[2]
ROOT_CONFIG = ROOT / ".gitlab-ci.yml"
README = ROOT / ".gitlab" / "README.md"
TOOLCHAIN = ROOT / "ic" / "rust-toolchain.toml"
WORKSPACE_MANIFEST = ROOT / "ic" / "Cargo.toml"
DEFAULTS = ROOT / ".gitlab" / "ci" / "defaults.yml"
OPENCODE_DOCKERFILE = ROOT / "opencode4lunarwing" / "Dockerfile"
KIMI_DOCKERFILE = ROOT / "kimicode4lunarwing" / "Dockerfile"
EXPECTED_RUST_VERSION = "1.96.1"
EXPECTED_INCLUDES = [
    "/.gitlab/ci/workflow.yml",
    "/.gitlab/ci/defaults.yml",
    "/.gitlab/ci/validate.yml",
    "/.gitlab/ci/rust.yml",
    "/.gitlab/ci/integration.yml",
    "/.gitlab/ci/security.yml",
    "/.gitlab/ci/containers.yml",
]
IMAGE_PATTERN = re.compile(r"@sha256:[0-9a-f]{64}$")
DIGEST_PATTERN = re.compile(r"sha256:([0-9a-f]{64})")
DIRECT_CARGO_PATTERN = re.compile(r"(?:^|[;&|]\s*|\s)cargo(?:\s|$)")
RUST_IMAGE_RE = re.compile(r"rust:([^@]+)@sha256:([0-9a-f]{64})")
VARIABLE_REF_PATTERN = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)")


def load_yaml(path: pathlib.Path) -> dict:
    with path.open(encoding="utf-8") as stream:
        loaded = yaml.safe_load(stream)
    if not isinstance(loaded, dict):
        raise ValueError(f"{path.relative_to(ROOT)} must contain a YAML mapping")
    return loaded


def image_name(image: object) -> str | None:
    if isinstance(image, str):
        return image
    if isinstance(image, dict) and isinstance(image.get("name"), str):
        return image["name"]
    return None


def collect_variables(configs: list[tuple[pathlib.Path, dict]]) -> dict[str, str]:
    variables: dict[str, str] = {}
    for _path, config in configs:
        declared = config.get("variables")
        if not isinstance(declared, dict):
            continue
        for key, value in declared.items():
            if isinstance(value, str):
                variables[key] = value
    return variables


def resolve_variables(value: str, variables: dict[str, str], depth: int = 5) -> str:
    result = value
    for _ in range(depth):
        replaced = VARIABLE_REF_PATTERN.sub(
            lambda match: variables.get(match.group(1) or match.group(2), match.group(0)),
            result,
        )
        if replaced == result:
            break
        result = replaced
    return result


def extract_rust_image(
    defaults: dict, job_name: str, variables: dict[str, str]
) -> tuple[str, str] | None:
    rust_job = defaults.get(job_name, {})
    if not isinstance(rust_job, dict):
        return None
    image = image_name(rust_job.get("image"))
    if not image:
        return None
    match = RUST_IMAGE_RE.search(resolve_variables(image, variables))
    if not match:
        return None
    return match.group(1), match.group(2)


def main() -> int:
    errors: list[str] = []
    root = load_yaml(ROOT_CONFIG)
    includes = [item.get("local") for item in root.get("include", [])]
    if includes != EXPECTED_INCLUDES:
        errors.append(f"root includes differ from expected order: {includes}")

    configs = [ROOT_CONFIG]
    for include in EXPECTED_INCLUDES:
        path = ROOT / include.lstrip("/")
        if not path.is_file():
            errors.append(f"missing local include: {include}")
        else:
            configs.append(path)

    loaded_configs = [(path, load_yaml(path)) for path in configs]
    merged_variables = collect_variables(loaded_configs)
    workflow = dict(loaded_configs[1][1].get("workflow", {}))
    workflow_rules = workflow.get("rules", [])
    conditions = "\n".join(str(rule.get("if", "")) for rule in workflow_rules)
    if "merge_request_event" not in conditions or "CI_OPEN_MERGE_REQUESTS" not in conditions:
        errors.append("workflow must enable MR pipelines and suppress duplicate branch pipelines")

    for path, config in loaded_configs:
        for name, value in config.items():
            if not isinstance(value, dict):
                continue
            image = image_name(value.get("image"))
            if image:
                resolved_image = resolve_variables(image, merged_variables)
                if not IMAGE_PATTERN.search(resolved_image):
                    errors.append(
                        f"{path.relative_to(ROOT)}:{name} uses unpinned image "
                        f"{image} (resolved: {resolved_image})"
                    )

            script = value.get("script", [])
            if isinstance(script, str):
                script = [script]
            for command in script:
                if isinstance(command, str) and DIRECT_CARGO_PATTERN.search(command):
                    errors.append(
                        f"{path.relative_to(ROOT)}:{name} bypasses .gitlab/scripts/cargo-ci.sh"
                    )

    for script in (ROOT / ".gitlab" / "scripts").iterdir():
        if script.suffix in {".sh", ".py"} and not script.is_file():
            errors.append(f"invalid CI helper: {script.relative_to(ROOT)}")

    defaults_path = ROOT / ".gitlab" / "ci" / "defaults.yml"
    defaults = load_yaml(defaults_path)
    rust_image = extract_rust_image(defaults, ".rust-job", merged_variables)
    msrv_image = extract_rust_image(defaults, ".rust-msrv-job", merged_variables)
    if rust_image is None:
        errors.append("could not extract Rust image tag/digest from defaults.yml .rust-job")
    if msrv_image is None:
        errors.append("could not extract Rust image tag/digest from defaults.yml .rust-msrv-job")
    if WORKSPACE_MANIFEST.is_file():
        with WORKSPACE_MANIFEST.open("rb") as stream:
            workspace_manifest = tomllib.load(stream)
        rust_version = workspace_manifest.get("package", {}).get("rust-version")
        if rust_version != EXPECTED_RUST_VERSION:
            errors.append(
                f"ic/Cargo.toml rust-version {rust_version!r} != expected "
                f"{EXPECTED_RUST_VERSION!r}"
            )
    else:
        errors.append("ic/Cargo.toml missing")

    if OPENCODE_DOCKERFILE.is_file():
        dockerfile_text = OPENCODE_DOCKERFILE.read_text(encoding="utf-8")
        toolchain_match = re.search(r"--default-toolchain\s+([^\s\\]+)", dockerfile_text)
        if not toolchain_match:
            errors.append("opencode4lunarwing/Dockerfile missing default Rust toolchain")
        elif toolchain_match.group(1) != EXPECTED_RUST_VERSION:
            errors.append(
                "opencode4lunarwing/Dockerfile Rust toolchain "
                f"{toolchain_match.group(1)!r} != expected {EXPECTED_RUST_VERSION!r}"
            )
    else:
        errors.append("opencode4lunarwing/Dockerfile missing")

    if KIMI_DOCKERFILE.is_file():
        dockerfile_text = KIMI_DOCKERFILE.read_text(encoding="utf-8")
        toolchain_match = re.search(r"--default-toolchain\s+([^\s\\]+)", dockerfile_text)
        if toolchain_match and toolchain_match.group(1) != EXPECTED_RUST_VERSION:
            errors.append(
                "kimicode4lunarwing/Dockerfile Rust toolchain "
                f"{toolchain_match.group(1)!r} != expected {EXPECTED_RUST_VERSION!r}"
            )
    else:
        errors.append("kimicode4lunarwing/Dockerfile missing")

    if rust_image is not None:
        rust_tag, rust_digest = rust_image
        rust_image_version = rust_tag.split("-")[0]
        if rust_image_version != EXPECTED_RUST_VERSION:
            errors.append(
                f"defaults.yml .rust-job image version {rust_image_version!r} != expected "
                f"{EXPECTED_RUST_VERSION!r}"
            )

        if msrv_image is not None:
            msrv_tag, _ = msrv_image
            msrv_image_version = msrv_tag.split("-")[0]
            if msrv_image_version != EXPECTED_RUST_VERSION:
                errors.append(
                    f"defaults.yml .rust-msrv-job image version {msrv_image_version!r} != "
                    f"expected {EXPECTED_RUST_VERSION!r}"
                )
            if msrv_image != rust_image:
                errors.append(
                    "defaults.yml .rust-msrv-job image tag/digest must match .rust-job"
                )

        readme_text = README.read_text(encoding="utf-8") if README.is_file() else ""
        readme_digests = set(DIGEST_PATTERN.findall(readme_text))
        if rust_digest not in readme_digests:
            errors.append(
                f".gitlab/README.md does not reference Rust image digest "
                f"{rust_digest[:12]}... from defaults.yml"
            )

        stale_rust = re.findall(r"rust:1\.9[0-6](?:\.\d+)?-bookworm", readme_text)
        stale_rust = [t for t in stale_rust if t != f"rust:{rust_tag}"]
        if stale_rust:
            errors.append(
                f".gitlab/README.md references stale Rust image tags: {stale_rust}"
            )

        if TOOLCHAIN.is_file():
            toolchain_text = TOOLCHAIN.read_text(encoding="utf-8")
            channel_match = re.search(r'channel\s*=\s*"([^"]+)"', toolchain_text)
            if not channel_match:
                errors.append("ic/rust-toolchain.toml missing channel declaration")
            else:
                channel = channel_match.group(1)
                if channel != EXPECTED_RUST_VERSION:
                    errors.append(
                        f"ic/rust-toolchain.toml channel {channel!r} != expected "
                        f"{EXPECTED_RUST_VERSION!r}"
                    )
        else:
            errors.append("ic/rust-toolchain.toml missing")

    if errors:
        print("\n".join(f"ERROR: {error}" for error in errors), file=sys.stderr)
        return 1

    print(f"Validated {len(configs)} GitLab CI YAML files and local helper references.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
