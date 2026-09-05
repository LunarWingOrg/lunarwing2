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
CARGO_CONFIG = ROOT / ".cargo" / "config.toml"
DEFAULTS = ROOT / ".gitlab" / "ci" / "defaults.yml"
OPENCODE_DOCKERFILE = ROOT / "opencode4lunarwing" / "Dockerfile"
KIMI_DOCKERFILE = ROOT / "kimicode4lunarwing" / "Dockerfile"
EXPECTED_RUST_VERSION = "1.96.1"
EXPECTED_NORMAL_DEBUG = "line-tables-only"
RELEASE_TAG_PREDICATE = r"^v\d+\.\d+\.\d+\.\d+(-rc[1-9]\d*)?$"
FULL_VALIDATION_RULES = (
    '$CI_PIPELINE_SOURCE == "schedule"',
    "$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH",
    r"$CI_COMMIT_BRANCH =~ /^aragents\/kimil00p\//",
    "$CI_MERGE_REQUEST_TARGET_BRANCH_NAME == $CI_DEFAULT_BRANCH",
    '$CI_PIPELINE_SOURCE == "push" && $CI_COMMIT_TAG =~ '
    rf'/{RELEASE_TAG_PREDICATE}/ && $CI_COMMIT_REF_PROTECTED == "true"',
)
FULL_VALIDATION_JOBS = (
    "rust-check-postgres",
    "rust-check-libsql",
    "rust-check-all-features",
    "rust-test-libsql",
    "rust-test-postgres-integration",
    "cavepony-wasm",
    "cargo-deny",
)
WORKER_NAMES = ("pebble", "opencode", "kimi")
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


def validate_cargo_profiles(defaults: dict, errors: list[str]) -> None:
    if not CARGO_CONFIG.is_file():
        errors.append(".cargo/config.toml missing")
        return

    with CARGO_CONFIG.open("rb") as stream:
        cargo_config = tomllib.load(stream)
    profiles = cargo_config.get("profile", {})
    for profile_name in ("dev", "test"):
        debug = profiles.get(profile_name, {}).get("debug")
        if debug != EXPECTED_NORMAL_DEBUG:
            errors.append(
                f".cargo/config.toml profile.{profile_name}.debug {debug!r} != "
                f"expected {EXPECTED_NORMAL_DEBUG!r}"
            )

    full_debug = profiles.get("full-debug", {})
    if full_debug.get("inherits") != "dev" or full_debug.get("debug") != "full":
        errors.append(".cargo/config.toml profile.full-debug must inherit dev with full debug")

    ci_test_debug = defaults.get("variables", {}).get("CARGO_PROFILE_TEST_DEBUG")
    if ci_test_debug != EXPECTED_NORMAL_DEBUG:
        errors.append(
            f"defaults.yml CARGO_PROFILE_TEST_DEBUG {ci_test_debug!r} != "
            f"expected {EXPECTED_NORMAL_DEBUG!r}"
        )


def extends_template(job: object, template: str) -> bool:
    if not isinstance(job, dict):
        return False
    extended = job.get("extends", [])
    if isinstance(extended, str):
        extended = [extended]
    return isinstance(extended, list) and template in extended


def rule_conditions(job: object) -> list[str]:
    if not isinstance(job, dict) or not isinstance(job.get("rules"), list):
        return []
    return [str(rule.get("if", "")) for rule in job["rules"] if isinstance(rule, dict)]


def matches_full_validation_context(context: dict[str, str]) -> bool:
    source = context.get("source", "")
    branch = context.get("branch", "")
    default_branch = context.get("default_branch", "master")
    tag = context.get("tag", "")
    return (
        source == "schedule"
        or branch == default_branch
        or bool(re.fullmatch(r"aragents/kimil00p/.+", branch))
        or context.get("target", "") == default_branch
        or (
            source == "push"
            and context.get("protected") == "true"
            and bool(re.fullmatch(RELEASE_TAG_PREDICATE, tag))
        )
    )


def validate_release_gates(merged: dict, defaults: dict, errors: list[str]) -> None:
    template = defaults.get(".full-validation-context")
    conditions = rule_conditions(template)
    active_conditions = tuple(condition for condition in conditions if condition)
    if active_conditions != FULL_VALIDATION_RULES:
        errors.append("full-validation rules differ from the tested canonical predicate")
    release_conditions = [condition for condition in conditions if "CI_COMMIT_TAG" in condition]
    if len(release_conditions) != 1:
        errors.append("full-validation context must contain exactly one release-tag rule")
    elif RELEASE_TAG_PREDICATE not in release_conditions[0]:
        errors.append("full-validation release rule differs from release-version policy")
    if not any("CI_COMMIT_REF_PROTECTED" in condition for condition in release_conditions):
        errors.append("full-validation release rule must require a protected ref")
    if not any("CI_PIPELINE_SOURCE" in condition for condition in release_conditions):
        errors.append("full-validation release rule must require a push pipeline")

    context_matrix = (
        ({"source": "merge_request_event", "target": "master"}, True),
        ({"source": "push", "branch": "master"}, True),
        ({"source": "push", "branch": "aragents/kimil00p/release"}, True),
        ({"source": "schedule", "branch": "maintenance"}, True),
        ({"source": "push", "tag": "v2.1.0.0-rc1", "protected": "true"}, True),
        ({"source": "push", "tag": "v2.1.0.0", "protected": "true"}, True),
        ({"source": "push", "tag": "v2.1.0.0", "protected": "false"}, False),
        ({"source": "web", "tag": "v2.1.0.0", "protected": "true"}, False),
        ({"source": "push", "tag": "release", "protected": "true"}, False),
        ({"source": "push", "branch": "feature/item-28"}, False),
    )
    for context, expected in context_matrix:
        if matches_full_validation_context(context) is not expected:
            errors.append(f"full-validation context self-test failed for {context}")

    for job_name in FULL_VALIDATION_JOBS:
        if not extends_template(merged.get(job_name), ".full-validation-context"):
            errors.append(f"{job_name} must extend .full-validation-context")

    for job_name in ("version-policy", "panic-policy"):
        conditions = rule_conditions(merged.get(job_name))
        release = [condition for condition in conditions if "CI_COMMIT_TAG" in condition]
        if not release or RELEASE_TAG_PREDICATE not in release[0]:
            errors.append(f"{job_name} must run for protected release tags")


def validate_worker_publication(merged: dict, errors: list[str]) -> None:
    candidate = merged.get(".worker-candidate", {})
    promotion = merged.get(".worker-promotion", {})
    if candidate.get("interruptible") is not False or promotion.get("interruptible") is not False:
        errors.append("worker candidate and promotion templates must be non-interruptible")
    if candidate.get("retry") != 0 or promotion.get("retry") != 0:
        errors.append("worker publication must not retry after a timeout or partial push")
    for template_name in (".worker-candidate", ".worker-scan", ".worker-promotion"):
        image = image_name(merged.get(template_name, {}).get("image"))
        if not image or VARIABLE_REF_PATTERN.search(image) or not IMAGE_PATTERN.search(image):
            errors.append(f"{template_name} must use a literal digest-pinned image")
    scan_script = "\n".join(merged.get(".worker-scan", {}).get("script", []))
    if ". ./worker-image.env" not in scan_script:
        errors.append("worker scan must reload candidate values from its artifact")
    promotion_helper = ROOT / ".gitlab/scripts/promote-worker-image.py"
    if not promotion_helper.is_file() or "candidate_metadata()" not in promotion_helper.read_text(
        encoding="utf-8"
    ):
        errors.append("worker promotion must load candidate values from its artifact")
    build_helper = ROOT / ".gitlab/scripts/build-worker-image.sh"
    if not build_helper.is_file():
        errors.append("worker candidate helper is missing")
    else:
        helper_text = build_helper.read_text(encoding="utf-8")
        for fragment in (
            "--no-cache",
            "rewrite-timestamp=true",
            "WORKER_REBUILD_DIGEST",
            "clean rebuild digest mismatch",
        ):
            if fragment not in helper_text:
                errors.append(f"worker candidate helper lacks reproducibility control {fragment}")
    for worker_dir in ("pebble4lunarwing", "opencode4lunarwing", "kimicode4lunarwing"):
        dockerfile = (ROOT / worker_dir / "Dockerfile").read_text(encoding="utf-8")
        if "ARG SOURCE_REVISION" not in dockerfile or (
            'org.opencontainers.image.revision="${SOURCE_REVISION}"' not in dockerfile
        ):
            errors.append(f"{worker_dir}/Dockerfile must label the injected source revision")

    for template_name in (".worker-candidate", ".worker-scan", ".worker-promotion"):
        template = merged.get(template_name, {})
        rules = template.get("rules", []) if isinstance(template, dict) else []
        active_rules = [
            rule
            for rule in rules
            if isinstance(rule, dict) and rule.get("when") != "never"
        ]
        if not active_rules or any(
            "CI_COMMIT_REF_PROTECTED" not in str(rule.get("if", ""))
            or 'CI_PIPELINE_SOURCE == "push"' not in str(rule.get("if", ""))
            for rule in active_rules
        ):
            errors.append(f"{template_name} exposes a non-push or unprotected rule")

    for worker in WORKER_NAMES:
        candidate_name = f"worker-{worker}-candidate"
        scan_name = f"worker-{worker}-scan"
        promotion_name = f"worker-{worker}-promote"
        promotion_job = merged.get(promotion_name, {})
        if not isinstance(promotion_job, dict) or not promotion_job.get("resource_group"):
            errors.append(f"{promotion_name} must serialize moving-tag publication")
        needs = promotion_job.get("needs", []) if isinstance(promotion_job, dict) else []
        needed_jobs = {need.get("job") for need in needs if isinstance(need, dict)}
        if needed_jobs != {candidate_name, scan_name}:
            errors.append(f"{promotion_name} must need its exact candidate and scan")
        if not extends_template(merged.get(candidate_name), ".worker-candidate"):
            errors.append(f"{candidate_name} must extend .worker-candidate")
        if not extends_template(merged.get(scan_name), ".worker-scan"):
            errors.append(f"{scan_name} must extend .worker-scan")


def validate_test_shapes(merged: dict, errors: list[str]) -> None:
    expected = {
        "rust-check-postgres": ("--tests", "--features postgres"),
        "rust-check-libsql": ("--tests", "--features libsql"),
        "rust-test-postgres-integration": (
            "--features postgres,integration",
            "--test workspace_integration",
        ),
    }
    for job_name, fragments in expected.items():
        job = merged.get(job_name, {})
        script = job.get("script", []) if isinstance(job, dict) else []
        commands = "\n".join(script if isinstance(script, list) else [str(script)])
        if any(fragment not in commands for fragment in fragments):
            errors.append(f"{job_name} does not enforce its required backend test shape")


def cache_policies(job: object) -> list[str]:
    if not isinstance(job, dict):
        return []
    cache = job.get("cache", [])
    entries = cache if isinstance(cache, list) else [cache]
    return [str(entry.get("policy", "pull-push")) for entry in entries if isinstance(entry, dict)]


def validate_cache_trust(merged: dict, defaults: dict, errors: list[str]) -> None:
    for template_name in (".python-job", ".rust-job", ".rust-msrv-job"):
        policies = cache_policies(defaults.get(template_name))
        if not policies or any(policy != "pull" for policy in policies):
            errors.append(f"{template_name} validation caches must be fixed pull-only")

    writers = {
        name
        for name, job in merged.items()
        if isinstance(job, dict) and "pull-push" in cache_policies(job)
    }
    if writers != {"rust-cache-refresh"}:
        errors.append(f"unexpected writable cache jobs: {sorted(writers)}")
    refresh = merged.get("rust-cache-refresh", {})
    active_rules = [
        rule
        for rule in refresh.get("rules", [])
        if isinstance(rule, dict) and rule.get("when") != "never"
    ] if isinstance(refresh, dict) else []
    if len(active_rules) != 1:
        errors.append("rust-cache-refresh must have one active rule")
    else:
        condition = str(active_rules[0].get("if", ""))
        required = (
            'CI_PIPELINE_SOURCE == "push"',
            "CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH",
            'CI_COMMIT_REF_PROTECTED == "true"',
        )
        if any(fragment not in condition for fragment in required):
            errors.append("rust-cache-refresh must require a protected default-branch push")


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

            rules = value.get("rules", [])
            if isinstance(rules, list):
                for index, rule in enumerate(rules, start=1):
                    if (
                        isinstance(rule, dict)
                        and rule.get("when") == "manual"
                        and "allow_failure" not in rule
                    ):
                        errors.append(
                            f"{path.relative_to(ROOT)}:{name} manual rule {index} must "
                            "explicitly set allow_failure"
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
    merged_config: dict = {}
    for _path, config in loaded_configs:
        merged_config.update(config)
    validate_cargo_profiles(defaults, errors)
    validate_release_gates(merged_config, defaults, errors)
    validate_worker_publication(merged_config, errors)
    validate_test_shapes(merged_config, errors)
    validate_cache_trust(merged_config, defaults, errors)
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
