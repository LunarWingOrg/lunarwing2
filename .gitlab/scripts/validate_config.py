#!/usr/bin/env python3

import fnmatch
import hashlib
import json
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
OPENCODE2_DOCKERFILE = ROOT / "opencode2.4lunarwing" / "Dockerfile"
KIMI_DOCKERFILE = ROOT / "kimicode4lunarwing" / "Dockerfile"
KIMI_CLI_LOCK = ROOT / "kimicode4lunarwing" / "kimi-cli" / "package-lock.json"
PEBBLE_DOCKERFILE = ROOT / "pebble4lunarwing" / "Dockerfile"
WORKER_BUILD_SCRIPT = ROOT / ".gitlab" / "scripts" / "build-worker-image.sh"
DELIVERABLES = ROOT / ".gitlab" / "deliverables.json"
DELIVERABLE_STATUSES = {"production", "experimental", "fixture", "mirror", "retired", "staging"}
DELIVERABLE_REQUIRED_KEYS = ("name", "status", "owner", "manifest", "lockfile", "targets", "ci", "triggers")
EXPECTED_RUST_VERSION = "1.96.1"
EXPECTED_NORMAL_DEBUG = "line-tables-only"
RELEASE_TAG_PREDICATE = r"^v\d+\.\d+\.\d+\.\d+(-rc[1-9]\d*)?$"
FULL_VALIDATION_RULES = (
    '$LUNARWING_CI_FULL_VALIDATION == "1"',
    '$CI_PIPELINE_SOURCE == "schedule"',
    "$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH",
    r"$CI_COMMIT_BRANCH =~ /^aragents\/kimil00p\//",
    "$CI_MERGE_REQUEST_TARGET_BRANCH_NAME == $CI_DEFAULT_BRANCH",
    r"$CI_MERGE_REQUEST_TARGET_BRANCH_NAME =~ /^aragents\/kimil00p\//",
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
    "architecture-boundaries",
    "rust-clippy-all",
    "cargo-deny",
    "satellite-advisory-sweep",
)
WORKER_NAMES = ("pebble", "opencode", "opencode2", "kimi")
EXPECTED_INCLUDES = [
    "/.gitlab/ci/workflow.yml",
    "/.gitlab/ci/defaults.yml",
    "/.gitlab/ci/validate.yml",
    "/.gitlab/ci/rust.yml",
    "/.gitlab/ci/integration.yml",
    "/.gitlab/ci/security.yml",
    "/.gitlab/ci/containers.yml",
]
EXPECTED_STAGES = ("validate", "check", "test", "security", "containers")
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
    target = context.get("target", "")
    return (
        context.get("full_validation") == "1"
        or source == "schedule"
        or branch == default_branch
        or bool(re.fullmatch(r"aragents/kimil00p/.+", branch))
        or target == default_branch
        or bool(re.fullmatch(r"aragents/kimil00p/.+", target))
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
        ({"source": "merge_request_event", "target": "aragents/kimil00p/v2.1.4.0"}, True),
        ({"source": "merge_request_event", "target": "feature/integration"}, False),
        ({"source": "push", "branch": "cmc/g1/v2.1.4.0", "full_validation": "1"}, True),
        ({"source": "push", "branch": "cmc/g1/v2.1.4.0", "full_validation": "0"}, False),
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
    if candidate.get("needs") != []:
        errors.append("worker candidates must bypass stage barriers with needs: []")
    for rule in candidate.get("rules", []):
        if rule.get("when") != "never" and rule.get("when") != "manual":
            errors.append("worker candidates must remain manual")
    for rule in merged.get(".worker-scan", {}).get("rules", []):
        if rule.get("when", "on_success") not in ("on_success", "never"):
            errors.append("worker scans must automatically follow their candidates")
    for template_name in (".worker-candidate", ".worker-scan", ".worker-promotion"):
        image = image_name(merged.get(template_name, {}).get("image"))
        if not image or VARIABLE_REF_PATTERN.search(image) or not IMAGE_PATTERN.search(image):
            errors.append(f"{template_name} must use a literal digest-pinned image")
    scan_script = "\n".join(merged.get(".worker-scan", {}).get("script", []))
    if ". ./worker-image.env" not in scan_script:
        errors.append("worker scan must reload candidate values from its artifact")
    scan_artifacts = merged.get(".worker-scan", {}).get("artifacts", {})
    if isinstance(scan_artifacts, dict):
        reports = scan_artifacts.get("reports", {})
        if not isinstance(reports, dict) or "container_scanning" not in reports:
            errors.append("worker scan must emit a container_scanning report")
        elif "gl-container-scanning-report.json" not in str(reports.get("container_scanning", "")):
            errors.append("worker scan container_scanning report path is unexpected")
    else:
        errors.append("worker scan must declare artifacts with a container_scanning report")
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
    for worker_dir in ("pebble4lunarwing", "opencode4lunarwing", "opencode2.4lunarwing", "kimicode4lunarwing"):
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
        expected_tag = f"$CI_COMMIT_TAG-{worker}"
        if promotion_job.get("variables", {}).get("WORKER_PROMOTION_TAG") != expected_tag:
            errors.append(f"{promotion_name} must use unique tag {expected_tag}")
        needs = promotion_job.get("needs", []) if isinstance(promotion_job, dict) else []
        needed_jobs = {need.get("job") for need in needs if isinstance(need, dict)}
        if needed_jobs != {candidate_name, scan_name}:
            errors.append(f"{promotion_name} must need its exact candidate and scan")
        if not extends_template(merged.get(candidate_name), ".worker-candidate"):
            errors.append(f"{candidate_name} must extend .worker-candidate")
        if not extends_template(merged.get(scan_name), ".worker-scan"):
            errors.append(f"{scan_name} must extend .worker-scan")
        if merged.get(scan_name, {}).get("needs") != [
            {"job": candidate_name, "artifacts": True}
        ]:
            errors.append(f"{scan_name} must download artifacts from its exact candidate")


def validate_test_shapes(merged: dict, errors: list[str]) -> None:
    # Host-side runner limits control compile concurrency. A resource group
    # here would serialize independent hosts across all project pipelines.
    for name, job in merged.items():
        if (name.startswith("rust-") or name == "cavepony-wasm") and isinstance(job, dict):
            if job.get("resource_group"):
                errors.append(f"{name} must use runner concurrency, not a project resource lock")
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

    cavepony_script = merged.get("cavepony-wasm", {}).get("script", [])
    if not any(
        isinstance(command, str) and all(fragment in command for fragment in (
            "cargo-ci.sh build", "--locked", "--release", "--target wasm32-wasip2",
            "--manifest-path tools-src/cavepony/Cargo.toml",
        )) for command in cavepony_script
    ):
        errors.append("cavepony-wasm must build its committed WASM source with --locked")

    junit_jobs = {
        "python-unit": ("junit-python.xml", ".gitlab/scripts/python_tests.py"),
        "shell-harnesses": ("junit-shell.xml", ".gitlab/scripts/shell-harnesses-junit.sh"),
    }
    for job_name, (report, script_fragment) in junit_jobs.items():
        job = merged.get(job_name, {})
        if not isinstance(job, dict):
            errors.append(f"{job_name} job missing")
            continue
        commands = "\n".join(job.get("script", []))
        if script_fragment not in commands:
            errors.append(f"{job_name} must run {script_fragment}")
        artifacts = job.get("artifacts", {})
        reports = artifacts.get("reports", {}) if isinstance(artifacts, dict) else {}
        if not isinstance(reports, dict) or reports.get("junit") != report:
            errors.append(f"{job_name} must report junit artifact {report}")


def validate_cargo_deny_gate(merged: dict, errors: list[str]) -> None:
    """The cargo-deny job must run unvendored so yanked = "deny" is enforced.

    cargo-deny resolves the crates.io index only when it is not run from
    inside ic/, whose .cargo/config.toml source-replaces crates.io with
    vendor/. From ic/ every crate degrades to warning[index-failure] and the
    check silently passes (pipeline 4811 also lost its verdict to the 4 MB
    trace cap those diagnostics caused). Require the wrapper script, and
    forbid the vendored-context invocations inside it.
    """
    job = merged.get("cargo-deny", {})
    script = job.get("script", []) if isinstance(job, dict) else []
    commands = script if isinstance(script, list) else [str(script)]
    if not any(
        isinstance(command, str) and "cargo-deny-check.sh" in command
        for command in commands
    ):
        errors.append(
            "cargo-deny must invoke .gitlab/scripts/cargo-deny-check.sh "
            "(unvendored context) instead of cargo-ci.sh deny check"
        )

    wrapper = ROOT / ".gitlab" / "scripts" / "cargo-deny-check.sh"
    try:
        text = wrapper.read_text(encoding="utf-8")
    except OSError as error:
        errors.append(f"cannot read cargo-deny wrapper: {error}")
        return
    required_fragments = (
        "--manifest-path",
        "--hide-inclusion-graph",
        "--workspace --all-features",
        'grep -q "index-failure"',
        "advisory-db commit",
    )
    for fragment in required_fragments:
        if fragment not in text:
            errors.append(f"cargo-deny-check.sh lost its {fragment} guard")


def validate_satellite_advisory_sweep(merged: dict, errors: list[str]) -> None:
    """The satellite sweep keeps the release audit reproducible (A5).

    MR !1371 review: the audit's "every committed lockfile is clean" claim
    was produced by an uncommitted evaluator. The committed sweep script
    plus this wiring assertion make the claim a CI gate instead.
    """
    job = merged.get("satellite-advisory-sweep", {})
    script = job.get("script", []) if isinstance(job, dict) else []
    commands = script if isinstance(script, list) else [str(script)]
    if not any(
        isinstance(command, str) and "check-satellite-advisories.sh" in command
        for command in commands
    ):
        errors.append(
            "satellite-advisory-sweep must invoke "
            ".gitlab/scripts/check-satellite-advisories.sh"
        )

    wrapper = ROOT / ".gitlab" / "scripts" / "check-satellite-advisories.sh"
    try:
        text = wrapper.read_text(encoding="utf-8")
    except OSError as error:
        errors.append(f"cannot read satellite advisory sweep: {error}")
        return
    required_fragments = (
        "ls-files",
        "ic/deny.toml",
        "--workspace --all-features",
        "check advisories",
    )
    for fragment in required_fragments:
        if fragment not in text:
            errors.append(f"check-satellite-advisories.sh lost its {fragment} guard")


def merge_ci_hashes(base: dict, overlay: dict) -> dict:
    """GitLab `extends` merge step: hashes merge key-wise, arrays and
    scalars are replaced, and the overlay wins on conflicts ("keys from the
    last member always override anything defined on other levels")."""
    merged = dict(base)
    for key, value in overlay.items():
        existing = merged.get(key)
        if isinstance(existing, dict) and isinstance(value, dict):
            merged[key] = merge_ci_hashes(existing, value)
        else:
            merged[key] = value
    return merged


def resolve_extends(job: object, merged: dict, depth: int = 8) -> dict:
    """Resolve a job's `extends` chain with GitLab merge semantics.

    MR !1627 review HE-1 (R4): a lane extending `.observational-job`
    inherits `allow_failure: true` and runs but cannot gate, so the
    validator must see the EFFECTIVE job, not the literal one. Parents are
    applied in list order so the last listed parent wins on conflicts, and
    the job's own keys win overall. The depth budget makes cyclic extends
    chains terminate; a truncated cycle keeps whatever keys it set, which
    fails closed for the gating keys (they cannot be deleted by a cycle).
    """
    if not isinstance(job, dict):
        return {}
    own = {key: value for key, value in job.items() if key != "extends"}
    extends = job.get("extends")
    if isinstance(extends, str):
        names = [extends]
    elif isinstance(extends, list):
        names = [name for name in extends if isinstance(name, str)]
    else:
        names = []
    if depth <= 0 or not names:
        return own
    inherited: dict = {}
    for name in names:
        parent = resolve_extends(merged.get(name), merged, depth - 1)
        inherited = merge_ci_hashes(inherited, parent)
    return merge_ci_hashes(inherited, own)


AUTOMATIC_WHEN = (None, "on_success", "always")
MR_RULE_IF = '$CI_PIPELINE_SOURCE=="merge_request_event"'


def normalize_rule_if(condition: object) -> str:
    """Whitespace- and quote-insensitive form of a rules:if predicate."""
    return re.sub(r"\s+", "", str(condition or "").replace("'", '"'))


def lane_runs_on_merge_requests(job: object, merged: dict) -> bool:
    """Exact-predicate first-rule gate for the MR pipeline case.

    MR !1618 review GV-1 walked the rules in order but still matched the
    `if` by substring; MR !1627 review HE-1 showed eight further shapes
    pass on the real tree while the lane cannot gate MRs: a job-level
    `when: manual` (R1), a catch-all shadow rule placed first via
    `if: $CI_PIPELINE_SOURCE`, `exists:`, or `changes:` (R2), an inverted
    (`!=`) or `&&`-narrowed MR predicate (R3), and `extends:
    .observational-job` inheriting `allow_failure: true` (R4). GitLab's
    first match wins, so the lane gates MRs exactly when the FIRST rule
    matches MR pipelines and runs them automatically. After resolving
    `extends`, require: no job-level `allow_failure`, no non-automatic
    job-level `when`, no `only`/`except`, no non-empty `needs`, and a
    first rule whose keys are only `if`/`when`/`allow_failure`, whose `if`
    normalises to exactly `$CI_PIPELINE_SOURCE == "merge_request_event"`,
    and which runs automatically without rule-level allow_failure.
    """
    effective = resolve_extends(job, merged)
    if not effective:
        return False
    if effective.get("allow_failure"):
        return False
    if effective.get("when") not in AUTOMATIC_WHEN:
        return False
    if "only" in effective or "except" in effective:
        return False
    if effective.get("needs"):
        return False
    rules = effective.get("rules")
    if not isinstance(rules, list) or not rules:
        return False
    first = rules[0]
    if not isinstance(first, dict) or not set(first) <= {"if", "when", "allow_failure"}:
        return False
    if normalize_rule_if(first.get("if")) != MR_RULE_IF:
        return False
    if first.get("when") not in AUTOMATIC_WHEN:
        return False
    return not first.get("allow_failure")


def script_runs_command(script: object, fragment: str) -> bool:
    """True when `fragment` starts a command or an &&/; segment of a script
    line (MR !1627 review HE-2). A substring anywhere in the line is not
    enough: `'# npm test'`, `echo npm test`, and `true || npm test` all
    contain the fragment without the lane running it unconditionally.
    Segments containing `||` do not count either — `npm test || true`
    cannot fail the lane. Accepted residual limits: shell-variable
    indirection (`$RUN_TESTS`) and here-doc/multi-line constructs are not
    followed; the fragment check guards against accidental deletion and
    casual bypass, not a fully adversarial shell rewrite.
    """
    commands = script if isinstance(script, list) else [script]
    for command in commands:
        for line in str(command).splitlines():
            for segment in re.split(r"&&|;", line):
                stripped = segment.strip()
                if "||" in stripped:
                    continue
                if stripped.startswith(fragment) and (
                    len(stripped) == len(fragment) or stripped[len(fragment)] in " \t"
                ):
                    return True
    return False


def validate_worker_ts_lanes(merged: dict, errors: list[str]) -> None:
    """Both worker TS lanes must exist and keep their MR-pipeline wiring.

    MR !1601 review GH-2: neither TS lane was pinned by the validator, so
    deleting the whole block or replacing its rules with `when: never`
    passed every gate silently — exactly the regression class GB-2 was
    about. Require each lane to exist, to run on merge_request_event (per
    the HE-1 exact-predicate first-rule gate on the extends-resolved job),
    to use its digest-tied image variable, and to keep its core script
    commands — each pinned fragment must start a command or an &&/;
    segment (HE-2), so commenting a command out or neutralising it with
    `true ||`/`|| true` fails here. A `before_script`, an image entrypoint
    override, or a non-empty `needs` could hollow out the lane just as
    well, so those are pinned too (HE-2). The kimi lane's pinned commands
    include the python3 provisioning (GH-1) from the pinned Debian
    snapshot (GV-4) with its apt retry options (HE-4).
    """
    expected = {
        "worker-kimi-ts": (
            "$CI_NODE_IMAGE",
            (
                "apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 "
                "-o Acquire::http::Timeout=60 update",
                "apt-get install -y --no-install-recommends python3",
                "printf '%s\\n' \"deb http://snapshot.debian.org/archive/debian/"
                "${CI_DEBIAN_SNAPSHOT}/ bookworm main\"",
                "cd kimicode4lunarwing/scripts",
                "npm ci --ignore-scripts",
                "npx tsc -p tsconfig.json",
                "npm test",
            ),
        ),
        "worker-opencode-ts": (
            "$CI_BUN_IMAGE",
            (
                "cd opencode4lunarwing",
                "bun install --frozen-lockfile",
                "./node_modules/.bin/tsc --noEmit -p scripts/tsconfig.json",
            ),
        ),
    }
    for job_name, (image_variable, fragments) in expected.items():
        job = merged.get(job_name)
        if not isinstance(job, dict):
            errors.append(f"{job_name} job missing")
            continue
        effective = resolve_extends(job, merged)
        image = effective.get("image")
        if image_name(image) != image_variable:
            errors.append(f"{job_name} must use the {image_variable} image")
        entrypoint = image.get("entrypoint") if isinstance(image, dict) else None
        if entrypoint is not None and entrypoint != [""]:
            errors.append(f"{job_name} must not override its image entrypoint")
        if effective.get("before_script"):
            errors.append(f"{job_name} must not set a before_script")
        if not lane_runs_on_merge_requests(job, merged):
            errors.append(f"{job_name} must run on merge_request_event")
        script = effective.get("script", [])
        for fragment in fragments:
            if not script_runs_command(script, fragment):
                errors.append(f"{job_name} lost its required script fragment: {fragment}")


def validate_cavepony_version(merged: dict, errors: list[str]) -> None:
    """Catch release metadata drift before the expensive WASM/host test job."""
    expected = merged.get("cavepony-wasm", {}).get("variables", {}).get("CAVEPONY_EXPECTED_VERSION")
    if not isinstance(expected, str) or not re.fullmatch(r"\d+\.\d+\.\d+", expected):
        errors.append("cavepony-wasm must declare CAVEPONY_EXPECTED_VERSION")
        return

    source = "ic/tools-src/cavepony"
    versions = {}
    for relative in (
        "ic/registry/tools/cavepony-tool.json", f"{source}/cavepony-tool.capabilities.json",
        f"{source}/Cargo.toml", f"{source}/Cargo.lock",
    ):
        try:
            text = (ROOT / relative).read_text(encoding="utf-8")
            if relative.endswith(".json"):
                versions[relative] = [json.loads(text).get("version")]
            elif relative.endswith("Cargo.toml"):
                versions[relative] = [tomllib.loads(text).get("package", {}).get("version")]
            else:
                versions[relative] = [
                    package.get("version") for package in tomllib.loads(text).get("package", [])
                    if package.get("name") == "cavepony-tool" and "source" not in package
                ]
        except (OSError, ValueError) as error:
            errors.append(f"cannot read Cavepony release metadata {relative}: {error}")
    for relative, actual in versions.items():
        if actual != [expected]:
            errors.append(f"{relative} Cavepony version {actual!r} != CI expectation {expected!r}")


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
        # Exactly one default-feature check per context, while retaining the
        # literal pull-only cache policy on untrusted validation jobs.
        check = merged.get("rust-check-default", {})
        if check.get("rules") != [
            {"if": condition, "when": "never"}, {"when": "on_success"}
        ]:
            errors.append("rust-check-default must complement the protected cache writer")
        if check.get("script") != refresh.get("script"):
            errors.append("the cache writer must perform the same default-feature check")
        if refresh.get("needs") != ["cargo-metadata"]:
            errors.append("rust-cache-refresh must check directly after cargo-metadata")
        for name in ("rust-test-lib", "cavepony-wasm"):
            needs = merged.get(name, {}).get("needs", [])
            for producer in ("rust-check-default", "rust-cache-refresh"):
                if {"job": producer, "artifacts": False, "optional": True} not in needs:
                    errors.append(f"{name} must wait for whichever default check is present")


def extract_arg_default(text: str, arg: str) -> str | None:
    match = re.search(rf"^ARG {arg}=(\S+)", text, re.MULTILINE)
    return match.group(1) if match else None


def worker_case_blocks(build_script: str) -> dict[str, str]:
    """Extract each worker's case block from build-worker-image.sh so pin
    checks bind to the right lane — opencode and opencode2 share the
    OPENCODE_REF/OPENCODE_VERSION arg names, so a whole-script search can be
    satisfied by the wrong lane's value (WI#303 review)."""
    blocks: dict[str, str] = {}
    for match in re.finditer(
        r"(?ms)^    (pebble|opencode2|opencode|kimi)\)\n(.*?)^        ;;", build_script
    ):
        blocks[match.group(1)] = match.group(2)
    return blocks


def validate_worker_versions(errors: list[str]) -> None:
    """Cross-check worker version pins: Dockerfile ARG defaults, the build
    script's per-worker build_args and source_ref, and Kimi's committed npm
    lockfile must agree."""
    build_script = WORKER_BUILD_SCRIPT.read_text(encoding="utf-8")
    pebble_text = PEBBLE_DOCKERFILE.read_text(encoding="utf-8")
    opencode_text = OPENCODE_DOCKERFILE.read_text(encoding="utf-8")
    opencode2_text = OPENCODE2_DOCKERFILE.read_text(encoding="utf-8")
    kimi_text = KIMI_DOCKERFILE.read_text(encoding="utf-8")

    lock_path = ROOT / "pebble4lunarwing/upstream/Cargo.lock"
    lock_digest = extract_arg_default(pebble_text, "PEBBLE_LOCK_SHA256") or ""
    if not re.fullmatch(r"[0-9a-f]{64}", lock_digest):
        errors.append("Pebble Dockerfile must declare a reviewed PEBBLE_LOCK_SHA256 default")
    elif not lock_path.is_file() or hashlib.sha256(lock_path.read_bytes()).hexdigest() != lock_digest:
        errors.append("Pebble upstream Cargo.lock differs from the Dockerfile's reviewed SHA256")

    blocks = worker_case_blocks(build_script)
    canonical_pins = (
        ("pebble", "PEBBLE_REF", pebble_text, "PEBBLE_REF"),
        ("opencode", "OPENCODE_REF", opencode_text, "OPENCODE_REF"),
        ("opencode", "OPENCODE_VERSION", opencode_text, "OPENCODE_VERSION"),
        ("opencode2", "OPENCODE2_REF", opencode2_text, "OPENCODE_REF"),
        ("opencode2", "OPENCODE2_VERSION", opencode2_text, "OPENCODE_VERSION"),
        ("kimi", "KIMI_CODE_VERSION", kimi_text, "KIMI_CODE_VERSION"),
    )
    for worker, label, dockerfile_text, arg_name in canonical_pins:
        value = extract_arg_default(dockerfile_text, arg_name)
        block = blocks.get(worker)
        if not value:
            errors.append(f"worker Dockerfiles missing canonical {label} default")
        elif block is None:
            errors.append(f"build-worker-image.sh has no case block for worker {worker!r}")
        elif not re.search(rf"{arg_name}={re.escape(value)}\b", block):
            errors.append(
                f"build-worker-image.sh {worker} block {arg_name}={value!r} differs from the "
                f"Dockerfile {label} canonical declaration"
            )

    # The provenance-recording source_ref must match the reviewed pin too —
    # it lands in worker-inputs.sha256 but was historically never checked.
    for worker, label, dockerfile_text, arg_name in canonical_pins:
        if arg_name not in ("PEBBLE_REF", "OPENCODE_REF", "KIMI_CODE_VERSION"):
            continue
        value = extract_arg_default(dockerfile_text, arg_name) or ""
        block = blocks.get(worker) or ""
        match = re.search(r"(?m)^        source_ref=(\S+)$", block)
        if match is None:
            errors.append(f"build-worker-image.sh {worker} block must declare a source_ref")
        elif value and match.group(1) != value:
            errors.append(
                f"build-worker-image.sh {worker} source_ref {match.group(1)!r} differs from the "
                f"Dockerfile {label} canonical declaration"
            )

    for worker, label, dockerfile_text, arg_name in canonical_pins:
        if arg_name not in ("OPENCODE_REF", "PEBBLE_REF"):
            continue
        value = extract_arg_default(dockerfile_text, arg_name) or ""
        if not re.fullmatch(r"[0-9a-f]{40}", value):
            errors.append(f"{label} must pin a full 40-hex-char commit, got {value!r}")

    kimi_version = extract_arg_default(kimi_text, "KIMI_CODE_VERSION") or ""
    if not KIMI_CLI_LOCK.is_file():
        errors.append("kimicode4lunarwing/kimi-cli/package-lock.json missing")
    else:
        try:
            lock = json.loads(KIMI_CLI_LOCK.read_text(encoding="utf-8"))
            pinned = lock.get("packages", {}).get("", {}).get("dependencies", {}).get(
                "@moonshot-ai/kimi-code"
            )
        except json.JSONDecodeError:
            pinned = None
            errors.append("kimicode4lunarwing/kimi-cli/package-lock.json is not valid JSON")
        if pinned != kimi_version:
            errors.append(
                "kimi-cli/package-lock.json pins @moonshot-ai/kimi-code "
                f"{pinned!r} != Dockerfile KIMI_CODE_VERSION {kimi_version!r}"
            )


def collect_tracked_lockfiles() -> set[str]:
    tracked = set()
    for lockfile in ROOT.glob("**/Cargo.lock"):
        relative = lockfile.relative_to(ROOT).as_posix()
        if relative.startswith("ic/vendor/"):
            continue
        tracked.add(relative)
    return tracked


def safe_relative_path(path: object) -> str | None:
    """Reject absolute or repository-escaping paths before any filesystem use."""
    if not isinstance(path, str) or not path:
        return None
    normalized = pathlib.PurePosixPath(path)
    if normalized.is_absolute() or ".." in normalized.parts:
        return None
    return path


def validate_build_inputs(data: dict, merged: dict, errors: list[str]) -> dict[str, str]:
    """Lock-only build overrides are inputs, not standalone Cargo roots."""
    inputs = data.get("build_inputs", [])
    if not isinstance(inputs, list):
        errors.append(".gitlab/deliverables.json build_inputs must be a list")
        return {}
    consumers = {entry.get("name"): entry for entry in data.get("deliverables", [])}
    seen_names = set(consumers)
    lockfiles: dict[str, str] = {}
    required = ("name", "kind", "owner", "consumer", "lockfile", "dockerfile", "ci", "triggers")
    for entry in inputs:
        if not isinstance(entry, dict):
            errors.append("build input entry must be a JSON object")
            continue
        name = entry.get("name", "<unnamed>")
        for key in required:
            if key not in entry:
                errors.append(f"build input {name!r} missing key {key!r}")
        if name in seen_names:
            errors.append(f"build input {name!r} is declared twice")
        seen_names.add(name)
        if entry.get("kind") != "cargo-lock-override":
            errors.append(f"build input {name!r} has unknown kind {entry.get('kind')!r}")
        if not isinstance(entry.get("owner"), str) or not entry["owner"]:
            errors.append(f"build input {name!r} must declare an owner")
        consumer = consumers.get(entry.get("consumer"), {})
        if consumer.get("status") != "production":
            errors.append(f"build input {name!r} must reference a production consumer")
        for key in ("lockfile", "dockerfile"):
            safe = safe_relative_path(entry.get(key))
            if safe is None or not (ROOT / safe).is_file():
                errors.append(f"build input {name!r} {key} is missing or unsafe")
        lockfile = entry.get("lockfile")
        if isinstance(lockfile, str):
            if not lockfile.endswith("/Cargo.lock"):
                errors.append(f"build input {name!r} must classify a Cargo.lock")
            if lockfile in lockfiles:
                errors.append(f"build input {name!r} repeats lockfile {lockfile}")
            lockfiles[lockfile] = name
        dockerfile = safe_relative_path(entry.get("dockerfile"))
        if dockerfile is not None and (ROOT / dockerfile).is_file() and isinstance(lockfile, str):
            text = (ROOT / dockerfile).read_text(encoding="utf-8")
            if not re.search(rf"^COPY\s+{re.escape(lockfile)}\s", text, re.MULTILINE):
                errors.append(f"build input {name!r} is not copied by its Dockerfile")
        ci = entry.get("ci", [])
        if not isinstance(ci, list) or "ci-config" not in ci:
            errors.append(f"build input {name!r} must declare ci-config coverage")
        else:
            for job in ci:
                if not isinstance(job, str) or job not in merged or job.startswith("."):
                    errors.append(f"build input {name!r} references unknown CI job {job!r}")
            if not any(extends_template(merged.get(job), ".worker-candidate") for job in ci if isinstance(job, str)):
                errors.append(f"build input {name!r} must declare worker-candidate coverage")
            if "standalone-deliverables" in ci:
                errors.append(f"build input {name!r} has no standalone Cargo manifest")
        triggers = entry.get("triggers", [])
        if not isinstance(triggers, list) or not any(
            isinstance(pattern, str) and isinstance(lockfile, str)
            and fnmatch.fnmatchcase(lockfile, pattern) for pattern in triggers
        ):
            errors.append(f"build input {name!r} triggers must cover its lockfile")
    return lockfiles


def validate_deliverables_manifest(merged: dict, errors: list[str]) -> None:
    if not DELIVERABLES.is_file():
        errors.append(".gitlab/deliverables.json missing")
        return
    try:
        data = json.loads(DELIVERABLES.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        errors.append(f".gitlab/deliverables.json is not valid JSON: {error}")
        return
    if data.get("schema") != 1:
        errors.append(".gitlab/deliverables.json schema must be 1")
        return
    entries = data.get("deliverables")
    if not isinstance(entries, list) or not entries:
        errors.append(".gitlab/deliverables.json must contain a non-empty deliverables list")
        return

    known_jobs = {
        name
        for name, value in merged.items()
        if isinstance(value, dict) and not name.startswith(".")
    }
    seen_names: set[str] = set()
    seen_lockfiles: dict[str, str] = {}
    for entry in entries:
        name = entry.get("name", "<unnamed>")
        for key in DELIVERABLE_REQUIRED_KEYS:
            if key not in entry:
                errors.append(f"deliverable {name!r} missing key {key!r}")
        if name in seen_names:
            errors.append(f"deliverable {name!r} is declared twice")
        seen_names.add(name)
        status = entry.get("status")
        if status not in DELIVERABLE_STATUSES:
            errors.append(
                f"deliverable {name!r} has unknown status {status!r}; "
                f"expected one of {sorted(DELIVERABLE_STATUSES)}"
            )
        for key in ("manifest", "lockfile"):
            path = entry.get(key)
            safe = safe_relative_path(path)
            if safe is None:
                errors.append(f"deliverable {name!r} {key} {path!r} is not a safe repository path")
            elif not (ROOT / safe).is_file():
                errors.append(f"deliverable {name!r} {key} {path!r} does not exist")
        lockfile = entry.get("lockfile")
        if isinstance(lockfile, str):
            if lockfile in seen_lockfiles:
                errors.append(
                    f"deliverable {name!r} and {seen_lockfiles[lockfile]!r} share "
                    f"lockfile {lockfile}"
                )
            seen_lockfiles[lockfile] = name
        ci = entry.get("ci")
        if status == "production" and not ci:
            errors.append(
                f"production deliverable {name!r} must declare at least one CI job"
            )
        if isinstance(ci, list):
            for job in ci:
                if not isinstance(job, str) or not job:
                    errors.append(f"deliverable {name!r} ci entry {job!r} is not a job name")
                elif job not in known_jobs:
                    errors.append(
                        f"deliverable {name!r} references unknown CI job {job!r}"
                    )

    build_lockfiles = validate_build_inputs(data, merged, errors)
    for lockfile, name in build_lockfiles.items():
        if lockfile in seen_lockfiles:
            errors.append(f"build input {name!r} shares deliverable lockfile {lockfile}")
        seen_lockfiles[lockfile] = name
    tracked = collect_tracked_lockfiles()
    unclassified = sorted(tracked - set(seen_lockfiles))
    if unclassified:
        errors.append(
            "tracked Cargo.lock roots missing from .gitlab/deliverables.json: "
            f"{unclassified}"
        )
    vanished = sorted(set(seen_lockfiles) - tracked)
    if vanished:
        errors.append(
            f".gitlab/deliverables.json classifies lockfiles that are not tracked: {vanished}"
        )


def validate_ci_tool_image_pins(merged: dict, variables: dict[str, str], errors: list[str]) -> None:
    trust_sensitive = (
        "CI_LINT_IMAGE", "CI_RUST_IMAGE", "CI_GITLEAKS_IMAGE", "CI_BUN_IMAGE", "CI_NODE_IMAGE"
    )
    for name in trust_sensitive:
        value = variables.get(name, "")
        if not value:
            errors.append(f"defaults.yml must declare {name}")
        elif not IMAGE_PATTERN.search(value):
            errors.append(f"{name}={value!r} is not digest-pinned")

    # The worker-opencode-ts lane must test with the same bun content the
    # worker image builds and runs with (MR !1552 review ER-5). Fail closed:
    # a missing Dockerfile, an unpinned/malformed oven/bun FROM, or no
    # oven/bun FROM at all must not silently dissolve the tie this check
    # exists to enforce (MR !1566 review EZ-3).
    bun_match = DIGEST_PATTERN.search(variables.get("CI_BUN_IMAGE", ""))
    if bun_match:
        if not OPENCODE_DOCKERFILE.is_file():
            errors.append(
                "opencode4lunarwing/Dockerfile missing; cannot cross-check "
                "CI_BUN_IMAGE against its oven/bun FROM digest(s)"
            )
        else:
            dockerfile_text = OPENCODE_DOCKERFILE.read_text(encoding="utf-8")
            bun_from_lines = [
                line.strip()
                for line in dockerfile_text.splitlines()
                if re.match(r"^\s*(?i:FROM)\s+(?:--platform=\S+\s+)?\S*oven/bun", line)
            ]
            if not bun_from_lines:
                errors.append(
                    "opencode4lunarwing/Dockerfile has no oven/bun FROM line; "
                    "cannot cross-check CI_BUN_IMAGE"
                )
            dockerfile_bun_digests = set()
            for from_line in bun_from_lines:
                digest_match = re.search(r"@sha256:([0-9a-f]{64})(?![0-9a-fA-F])", from_line)
                if digest_match:
                    dockerfile_bun_digests.add(digest_match.group(1))
                else:
                    errors.append(
                        "opencode4lunarwing/Dockerfile oven/bun FROM is not pinned "
                        "to a 64-char lowercase sha256 digest: "
                        f"{from_line!r}"
                    )
            if dockerfile_bun_digests and dockerfile_bun_digests != {bun_match.group(1)}:
                errors.append(
                    "CI_BUN_IMAGE digest must match the oven/bun digest(s) in "
                    "opencode4lunarwing/Dockerfile"
                )

    # The worker-kimi-ts lane must test with the same node content the worker
    # image builds and runs with (MR !1590 review GB-2). Fail closed like the
    # bun tie above: a missing Dockerfile, an unpinned/malformed node FROM, or
    # no node FROM at all must not silently dissolve the tie.
    node_match = DIGEST_PATTERN.search(variables.get("CI_NODE_IMAGE", ""))
    if node_match:
        if not KIMI_DOCKERFILE.is_file():
            errors.append(
                "kimicode4lunarwing/Dockerfile missing; cannot cross-check "
                "CI_NODE_IMAGE against its node FROM digest(s)"
            )
        else:
            dockerfile_text = KIMI_DOCKERFILE.read_text(encoding="utf-8")
            node_from_lines = [
                line.strip()
                for line in dockerfile_text.splitlines()
                if re.match(r"^\s*(?i:FROM)\s+(?:--platform=\S+\s+)?\S*/node[:@]", line)
            ]
            if not node_from_lines:
                errors.append(
                    "kimicode4lunarwing/Dockerfile has no node FROM line; "
                    "cannot cross-check CI_NODE_IMAGE"
                )
            dockerfile_node_digests = set()
            for from_line in node_from_lines:
                digest_match = re.search(r"@sha256:([0-9a-f]{64})(?![0-9a-fA-F])", from_line)
                if digest_match:
                    dockerfile_node_digests.add(digest_match.group(1))
                else:
                    errors.append(
                        "kimicode4lunarwing/Dockerfile node FROM is not pinned "
                        "to a 64-char lowercase sha256 digest: "
                        f"{from_line!r}"
                    )
            if dockerfile_node_digests and dockerfile_node_digests != {node_match.group(1)}:
                errors.append(
                    "CI_NODE_IMAGE digest must match the node digest(s) in "
                    "kimicode4lunarwing/Dockerfile"
                )

    # The worker-kimi-ts lane installs its python3 apt prerequisite inside
    # the bare node image; it must come from the same pinned Debian snapshot
    # the image builds with, not the live deb.debian.org mirror (MR !1618
    # review GV-4). Fail closed like the image ties: a missing/malformed
    # variable, a missing Dockerfile, or a missing/divergent valued
    # ARG DEBIAN_SNAPSHOT must not silently dissolve the tie.
    snapshot = variables.get("CI_DEBIAN_SNAPSHOT", "")
    if not re.fullmatch(r"\d{8}T\d{6}Z", snapshot):
        errors.append(
            "defaults.yml CI_DEBIAN_SNAPSHOT must be a snapshot timestamp "
            f"(YYYYMMDDTHHMMSSZ), got {snapshot!r}"
        )
    elif not KIMI_DOCKERFILE.is_file():
        errors.append(
            "kimicode4lunarwing/Dockerfile missing; cannot cross-check "
            "CI_DEBIAN_SNAPSHOT against its ARG DEBIAN_SNAPSHOT"
        )
    else:
        dockerfile_text = KIMI_DOCKERFILE.read_text(encoding="utf-8")
        snapshot_args = re.findall(r"(?m)^ARG\s+DEBIAN_SNAPSHOT=(\S+)$", dockerfile_text)
        if not snapshot_args:
            errors.append(
                "kimicode4lunarwing/Dockerfile has no valued ARG DEBIAN_SNAPSHOT; "
                "cannot cross-check CI_DEBIAN_SNAPSHOT"
            )
        elif set(snapshot_args) != {snapshot}:
            errors.append(
                "CI_DEBIAN_SNAPSHOT must match the ARG DEBIAN_SNAPSHOT value(s) in "
                "kimicode4lunarwing/Dockerfile"
            )

    literal_images = {
        ".worker-candidate": "docker.io/moby/buildkit:v0.25.1-rootless@sha256:0c0ce4929976ace509f7a53878303504b2de8857829aea4932af9f7325a30f51",
        ".worker-scan": "docker.io/aquasec/trivy:0.74.0@sha256:62b1e65e8869bc4b4c6aa4fa2b21595256c7c2f6018a9d9ad61caf87187c1969",
        ".worker-promotion": "docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b",
    }
    for template_name, expected in literal_images.items():
        image = image_name(merged.get(template_name, {}).get("image"))
        if image != expected:
            errors.append(
                f"{template_name} image {image!r} differs from the reviewed pin {expected!r}"
            )

    pg_service = merged.get("rust-test-postgres-integration", {}).get("services", [])
    if not any(
        isinstance(service, dict)
        and service.get("name", "").startswith("docker.io/pgvector/pgvector:pg16@sha256:")
        for service in pg_service
    ):
        errors.append("rust-test-postgres-integration must use a digest-pinned pgvector service")


def validate_stage_consistency(root: dict, errors: list[str]) -> None:
    """Root `stages` must be a superset of every job/template `stage:` so a
    job pointing at a stage GitLab does not know about is rejected here, at
    validation time, instead of failing the whole pipeline after push."""
    stages = root.get("stages")
    if not isinstance(stages, list) or not stages:
        errors.append(".gitlab-ci.yml must declare a non-empty stages list")
        return

    if tuple(stages) != EXPECTED_STAGES:
        errors.append(f"root stages differ from expected order/names: {stages}")

    unknown_stages = set()
    for include in EXPECTED_INCLUDES:
        path = ROOT / include.lstrip("/")
        if not path.is_file():
            continue
        config = load_yaml(path)
        for name, entry in config.items():
            if not isinstance(entry, dict):
                continue
            stage = entry.get("stage")
            if not isinstance(stage, str):
                continue
            if stage not in stages:
                unknown_stages.add(f"{path.relative_to(ROOT)}:{name} -> {stage}")
            elif stage.startswith("."):
                errors.append(
                    f"{path.relative_to(ROOT)}:{name} declares dotted stage {stage!r}"
                )

    if unknown_stages:
        errors.append(
            "jobs reference stages missing from the root stages list: "
            + ", ".join(sorted(unknown_stages))
        )


def main() -> int:
    errors: list[str] = []
    root = load_yaml(ROOT_CONFIG)
    validate_stage_consistency(root, errors)
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
    validate_cavepony_version(merged_config, errors)
    validate_cargo_deny_gate(merged_config, errors)
    validate_satellite_advisory_sweep(merged_config, errors)
    validate_worker_ts_lanes(merged_config, errors)
    validate_cache_trust(merged_config, defaults, errors)
    validate_worker_versions(errors)
    validate_deliverables_manifest(merged_config, errors)
    validate_ci_tool_image_pins(merged_config, merged_variables, errors)
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

    if OPENCODE2_DOCKERFILE.is_file():
        dockerfile_text = OPENCODE2_DOCKERFILE.read_text(encoding="utf-8")
        toolchain_match = re.search(r"--default-toolchain\s+([^\s\\]+)", dockerfile_text)
        if not toolchain_match:
            errors.append("opencode2.4lunarwing/Dockerfile missing default Rust toolchain")
        elif toolchain_match.group(1) != EXPECTED_RUST_VERSION:
            errors.append(
                "opencode2.4lunarwing/Dockerfile Rust toolchain "
                f"{toolchain_match.group(1)!r} != expected {EXPECTED_RUST_VERSION!r}"
            )
    else:
        errors.append("opencode2.4lunarwing/Dockerfile missing")

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
