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
    """Collect top-level `variables` with GitLab's precedence: includes
    apply in order and the root file (configs[0]) wins overall. The
    previous root-first order let a root-file override (e.g. an unpinned
    CI_NODE_IMAGE) go unseen here while GitLab used the root value
    (MR !1647 review HP-2)."""
    variables: dict[str, str] = {}
    for _path, config in [*configs[1:], *configs[:1]]:
        declared = config.get("variables")
        if not isinstance(declared, dict):
            continue
        for key, value in declared.items():
            # GitLab's documented hash form (`value:` + `description:`)
            # defines the same variable; keeping only str values let a
            # root-file hash-form CI_NODE_IMAGE replace the include's pinned
            # string unseen (MR !1658 review IA-2).
            if isinstance(value, dict):
                value = value.get("value")
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


# MR !1647 review HP-2/HN-3: the depth budget must fail CLOSED. Truncation
# used to keep whatever keys were visible at the budget, so a gating key on
# a parent deeper than the budget (or hidden by a cycle) passed silently;
# truncation now marks the result with a sentinel key that the lane gates
# refuse. The budget exceeds any legitimate chain depth.
EXTENDS_DEPTH_BUDGET = 11
EXTENDS_TRUNCATED_KEY = "extends_truncated"


def resolve_extends(
    job: object, merged: dict, depth: int = EXTENDS_DEPTH_BUDGET
) -> dict:
    """Resolve a job's `extends` chain with GitLab merge semantics.

    MR !1627 review HE-1 (R4): a lane extending `.observational-job`
    inherits `allow_failure: true` and runs but cannot gate, so the
    validator must see the EFFECTIVE job, not the literal one. Parents are
    applied in list order so the last listed parent wins on conflicts, and
    the job's own keys win overall. The depth budget terminates cyclic or
    over-long extends chains; a truncated resolution carries the
    EXTENDS_TRUNCATED_KEY sentinel (propagated up through the merge) so the
    lane gates refuse the job instead of certifying only the keys visible
    at truncation (MR !1647 review HP-2/HN-3).
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
    if not names:
        return own
    if depth <= 0:
        return {**own, EXTENDS_TRUNCATED_KEY: True}
    inherited: dict = {}
    for name in names:
        parent = resolve_extends(merged.get(name), merged, depth - 1)
        inherited = merge_ci_hashes(inherited, parent)
    return merge_ci_hashes(inherited, own)


AUTOMATIC_WHEN = (None, "on_success", "always")


def mr_rule_is_exact(condition: object) -> bool:
    """True when `condition` is exactly the canonical MR predicate.

    MR !1644 review HN-1: the previous squash-and-compare deleted every
    whitespace character, including inside the quoted literal, so
    `"merge_request_event "` (trailing space) and `"merge_request _event"`
    passed while GitLab compares the literal exactly — on MR pipelines that
    rule never matches and the trailing `when: never` silently drops the
    lane. Compare the operands instead: spaces/tabs AROUND the tokens and
    either quote style are formatting; anything inside the quotes is
    literal.
    """
    if not isinstance(condition, str):
        return False
    lhs, sep, rhs = condition.partition("==")
    blank = " " + chr(9)
    return (
        sep == "=="
        and lhs.strip(blank) == "$CI_PIPELINE_SOURCE"
        and rhs.strip(blank) in ('"merge_request_event"', "'merge_request_event'")
    )


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
    is exactly the canonical MR predicate (operand-exact per MR !1644
    review HN-1: only whitespace around the tokens and the quote style may
    vary), and which runs automatically without rule-level allow_failure.
    An extends chain exceeding the depth budget carries the
    EXTENDS_TRUNCATED_KEY sentinel and fails closed (MR !1647 review
    HP-2/HN-3).
    """
    effective = resolve_extends(job, merged)
    if not effective:
        return False
    if effective.get(EXTENDS_TRUNCATED_KEY):
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
    if not mr_rule_is_exact(first.get("if")):
        return False
    if first.get("when") not in AUTOMATIC_WHEN:
        return False
    return not first.get("allow_failure")


# MR !1644 review HN-5: the effective script of each worker TS lane must
# equal these lines exactly (compared after extends resolution) — the
# earlier "fragment starts a segment" pins let the lane be hollowed out
# with a prepended `exit 0`, a dropped provisioning line, backgrounded or
# neutralised test commands, or deleted bun test lines. Any deliberate edit
# to a lane's script in .gitlab/ci/validate.yml must update the tuple in
# the same commit, which is the point. Each entry pairs the lane's
# digest-tied image variable with its pinned script lines.
WORKER_TS_LANE_SCRIPTS = {
    "worker-kimi-ts": (
        "$CI_NODE_IMAGE",
        (
            "rm -f /etc/apt/sources.list.d/debian.sources",
            "printf '%s\\n' \"deb http://snapshot.debian.org/archive/debian/${CI_DEBIAN_SNAPSHOT}/ bookworm main\" \"deb http://snapshot.debian.org/archive/debian/${CI_DEBIAN_SNAPSHOT}/ bookworm-updates main\" \"deb http://snapshot.debian.org/archive/debian-security/${CI_DEBIAN_SNAPSHOT}/ bookworm-security main\" > /etc/apt/sources.list",
            "apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=5 -o Acquire::http::Timeout=60 update && apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=60 install -y --no-install-recommends python3",
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
            "bun scripts/path_expand_test.ts",
            "bun scripts/opencode_result_test.ts",
            "bun scripts/task_prompt_test.ts",
            "bun scripts/lunarwing_bridge_test.ts",
            "bun scripts/opencode_task_executor_test.ts",
            "./node_modules/.bin/tsc --noEmit -p scripts/tsconfig.json",
        ),
    ),
}

# MR !1647 review HP-1: environment variables (including parallel matrices)
# and pre-clone hooks can bypass an unchanged pinned script. Additional job
# keywords require an explicit allowlist update alongside the lane change.
WORKER_TS_LANE_KEYS = frozenset({
    "stage", "needs", "image", "tags", "rules", "script", "allow_failure",
    "when", "before_script",
})
WORKER_TS_LANE_TAGS = ["$CI_LINT_RUNNER_TAG"]

# MR !1658 review IA-5: global variables reach both worker TS lanes, and a
# name denylist cannot enumerate every environment route that hollows a lane
# out (NODE_OPTIONS and npm_config_* for node/npm, BUN_OPTIONS for bun — a
# `--preload` runs before bun's main script — and PATH/GIT_CHECKOUT with one
# more artifact). Every top-level variable must therefore be listed here;
# adding a global variable means editing this set in the same commit.
GLOBAL_VARIABLE_ALLOWLIST = frozenset({
    "CARGO_HOME", "CARGO_INCREMENTAL", "CARGO_NET_GIT_FETCH_WITH_CLI",
    "CARGO_PROFILE_TEST_DEBUG", "CARGO_TERM_COLOR", "CI_BUN_IMAGE",
    "CI_DEBIAN_SNAPSHOT", "CI_GITLEAKS_IMAGE", "CI_LINT_IMAGE",
    "CI_LINT_RUNNER_TAG", "CI_NODE_IMAGE", "CI_RUST_IMAGE",
    "CI_RUST_RUNNER_TAG", "GIT_DEPTH", "GIT_STRATEGY",
    "GIT_SUBMODULE_STRATEGY", "PIP_CACHE_DIR", "PIP_DISABLE_PIP_VERSION_CHECK",
    "PIP_ROOT_USER_ACTION", "PYTHONDONTWRITEBYTECODE", "RUSTC_WRAPPER",
    "SCCACHE_CACHE_SIZE", "SCCACHE_DIR",
})

# MR !1658 review IA-9: the worker TS lanes (and every lint/rust job)
# select their runner through these tag variables. A root-file or
# later-include override reroutes the pinned jobs to a different runner
# while the tags lists and the variables allowlist stay clean, so the
# resolved values are pinned here; a deliberate runner change edits
# defaults.yml and this table in the same commit.
EXPECTED_RUNNER_TAGS = {
    "CI_LINT_RUNNER_TAG": "lint",
    "CI_RUST_RUNNER_TAG": "rust-x86_64",
}


def lane_bypass_variable(name: object) -> bool:
    """True for variables that can neutralise a worker TS lane's pinned
    script purely from the job environment.

    MR !1647 review (run-121 HP-2 reminder): a top-level `variables:`
    entry reaches every job including both lanes. NODE_OPTIONS makes node
    skip every test (`--test-skip-pattern=.`) or exit immediately
    (`--import=data:...`), and npm reads `npm_config_*` case-insensitively
    (`npm_config_script_shell=/bin/true` turns `npm test` into a no-op). bun
    honours BUN_OPTIONS too (a `--preload` runs before the main script;
    verified in the pinned image, MR !1658 review IA-5), and PATH or
    GIT_CHECKOUT can hollow a lane out with one more artifact, so a name
    denylist is not the defense: every global variable must be in
    GLOBAL_VARIABLE_ALLOWLIST, and this predicate only sharpens the message.
    """
    if not isinstance(name, str):
        return False
    return name == "NODE_OPTIONS" or name.lower().startswith("npm_config_")


def validate_lane_environment_routes(merged: dict, errors: list[str]) -> None:
    """Refuse every merged top-level route into a worker TS lane's
    environment (MR !1647 review HP-2, MR !1658 review IA-1/IA-3/IA-5):
    `default.before_script`/`default.hooks` and the deprecated top-level
    `before_script` are inherited by both lanes; any global variable
    outside GLOBAL_VARIABLE_ALLOWLIST reaches every job; and
    `workflow:rules` variables become pipeline-wide variables (overriding
    same-named globals) on the pipelines the rule matches, so a NODE_OPTIONS
    or an unpinned CI_NODE_IMAGE there bypasses both the script pin and the
    digest pins while the global `variables:` block stays clean. The runner
    tag variables must also resolve to their pinned values (MR !1658 review
    IA-9): an override reroutes the lanes to another runner with the tags
    lists themselves untouched."""
    default_block = merged.get("default")
    if isinstance(default_block, dict):
        for key in ("before_script", "hooks"):
            if default_block.get(key):
                errors.append(
                    f"default.{key} is inherited by the worker TS lanes and can "
                    "bypass the pinned script; remove it from the default block"
                )
    if merged.get("before_script"):  # deprecated global keyword, still honoured
        errors.append(
            "top-level before_script is inherited by the worker TS lanes and can "
            "bypass the pinned script; remove it"
        )
    top_variables = merged.get("variables")
    if isinstance(top_variables, dict):
        bypass = sorted(
            key for key in top_variables
            if lane_bypass_variable(key) or key not in GLOBAL_VARIABLE_ALLOWLIST
        )
        if bypass:
            errors.append(
                f"top-level variables must not set {bypass}; global variables "
                "reach both worker TS lanes (GLOBAL_VARIABLE_ALLOWLIST)"
            )
    for name, expected in EXPECTED_RUNNER_TAGS.items():
        value = top_variables.get(name) if isinstance(top_variables, dict) else None
        if isinstance(value, dict):  # GitLab's hash form (value:/description:)
            value = value.get("value")
        if value != expected:
            errors.append(
                f"top-level {name} must be {expected!r} (got {value!r}); the "
                "runner tag variables route the worker TS lanes and every "
                "pinned job to their runner, so an override must update "
                "EXPECTED_RUNNER_TAGS in the same commit (MR !1658 review IA-9)"
            )
    workflow = merged.get("workflow")
    workflow_rules = workflow.get("rules") if isinstance(workflow, dict) else None
    if isinstance(workflow_rules, list) and any(
        isinstance(rule, dict) and "variables" in rule for rule in workflow_rules
    ):
        errors.append(
            "workflow:rules must not set variables; they become pipeline-wide "
            "variables that reach both worker TS lanes"
        )


def validate_worker_ts_lanes(merged: dict, errors: list[str]) -> None:
    """Both worker TS lanes must exist and keep their MR-pipeline wiring.

    MR !1601 review GH-2: neither TS lane was pinned by the validator, so
    deleting the whole block or replacing its rules with `when: never`
    passed every gate silently — exactly the regression class GB-2 was
    about. Require each lane to exist, to run on merge_request_event (per
    the HE-1 exact-predicate first-rule gate on the extends-resolved job),
    to use its digest-tied image variable, and to run exactly its pinned
    script (MR !1644 review HN-5: the extends-resolved script must equal
    WORKER_TS_LANE_SCRIPTS line for line, so a prepended `exit 0`, a
    dropped provisioning line, or a deleted test command fails here). A
    `before_script`, an image entrypoint override, or a non-empty `needs`
    could hollow out the lane just as well, so those stay pinned too
    (HE-2). The kimi lane's pinned commands include the python3
    provisioning (GH-1) from the pinned Debian snapshot (GV-4) with its apt
    retry options (HE-4). Only allowlisted job keys may survive extends
    resolution, and tags must select the lint runner (HP-1); job-level
    variables, parallel matrices and hooks cannot bypass the script pin.
    The same bypasses are refused when they arrive through the merged
    top-level blocks instead of the job: a `default.before_script` /
    `default.hooks` entry (or the deprecated top-level `before_script`) is
    inherited by both lanes, any global variable outside
    GLOBAL_VARIABLE_ALLOWLIST reaches every job, and `workflow:rules` or
    lane `rules` variables reach the pinned script on the pipelines they
    match (MR !1647 review HP-2, MR !1658 review IA-1..IA-6). An extends
    chain beyond the depth budget fails closed via EXTENDS_TRUNCATED_KEY.
    """
    validate_lane_environment_routes(merged, errors)
    for job_name, (image_variable, expected_script) in WORKER_TS_LANE_SCRIPTS.items():
        job = merged.get(job_name)
        if not isinstance(job, dict):
            errors.append(f"{job_name} job missing")
            continue
        effective = resolve_extends(job, merged)
        if effective.get(EXTENDS_TRUNCATED_KEY):
            errors.append(
                f"{job_name} extends chain exceeds the validation depth "
                f"budget ({EXTENDS_DEPTH_BUDGET}); refusing to certify a "
                "partially resolved job"
            )
        unexpected = sorted(set(effective) - WORKER_TS_LANE_KEYS - {EXTENDS_TRUNCATED_KEY})
        if unexpected:
            errors.append(
                f"{job_name} must not set job keys {unexpected}; job-level "
                "variables, parallel matrices or hooks can bypass the pinned script"
            )
        if effective.get("tags") != WORKER_TS_LANE_TAGS:
            errors.append(f"{job_name} tags must be exactly {WORKER_TS_LANE_TAGS}")
        image = effective.get("image")
        if image_name(image) != image_variable:
            errors.append(f"{job_name} must use the {image_variable} image")
        entrypoint = image.get("entrypoint") if isinstance(image, dict) else None
        if entrypoint is not None and entrypoint != [""]:
            errors.append(f"{job_name} must not override its image entrypoint")
        if effective.get("before_script"):
            errors.append(f"{job_name} must not set a before_script")
        rules = effective.get("rules")
        if isinstance(rules, list) and any(
            isinstance(rule, dict) and "variables" in rule for rule in rules
        ):
            errors.append(
                f"{job_name} rules must not set variables; rules:variables "
                "reach the pinned script on the pipelines that rule matches"
            )
        if not lane_runs_on_merge_requests(job, merged):
            errors.append(f"{job_name} must run on merge_request_event")
        script = effective.get("script")
        if not isinstance(script, list) or [str(line) for line in script] != list(expected_script):
            errors.append(
                f"{job_name} script must be exactly the pinned commands "
                f"{list(expected_script)!r} (got {script!r})"
            )


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

    # MR !1627 review HE-7, broadened by MR !1644 review HN-7: a
    # DEBIAN_SNAPSHOT build-arg or env override at image build time silently
    # detaches the CI_DEBIAN_SNAPSHOT tie (which binds only the Dockerfile
    # ARG default) from the snapshot the published image really builds with.
    # The guard covered one case block of one file; it now refuses the token
    # in every worker-image build entrypoint — the whole
    # build-worker-image.sh (shared post-case build_args and build_image()
    # included), mt-admin's tenant build, and the harness compose file.
    for label, path in (
        ("build-worker-image.sh", WORKER_BUILD_SCRIPT),
        (
            "ic/scripts/mt-admin-lib/05-tenant-build.sh",
            ROOT / "ic" / "scripts" / "mt-admin-lib" / "05-tenant-build.sh",
        ),
        ("tests/docker-compose.test.yml", ROOT / "tests" / "docker-compose.test.yml"),
    ):
        if not path.is_file():
            errors.append(
                f"{label} missing; cannot guard against a DEBIAN_SNAPSHOT build override"
            )
        elif "DEBIAN_SNAPSHOT" in path.read_text(encoding="utf-8"):
            errors.append(
                f"{label} must not reference DEBIAN_SNAPSHOT; a build-arg or env "
                "override at image build time would detach the CI_DEBIAN_SNAPSHOT "
                "tie from the Dockerfile ARG default"
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
    # ARG DEBIAN_SNAPSHOT must not silently dissolve the tie. The ARG match
    # tolerates Dockerfile-instruction case and indenting and strips quotes
    # (MR !1627 review HE-3), and every snapshot.debian.org URL in the
    # Dockerfile must reference ${DEBIAN_SNAPSHOT} itself — a hardcoded
    # timestamp or a renamed variable would detach the tie from what the
    # image actually builds with. The tie must also see the variants that
    # steer the build around the ARG (MR !1644 review HN-6): every line
    # mentioning DEBIAN_SNAPSHOT must be a plain ARG declaration or carry
    # the token inside a snapshot URL (an ENV after the ARG wins over it, a
    # RUN-level shell assignment re-points the RUN's URLs, a multi-name ARG
    # hides from the valued-ARG match, and shell substitution is not a
    # literal reference), and every apt `deb` source line must point at
    # snapshot.debian.org, so a builder moved back to the live mirror is
    # caught too. The host match is case-insensitive in both rules.
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
        snapshot_args = [
            match.strip("\"'")
            for match in re.findall(
                r"(?m)^\s*(?i:ARG)\s+DEBIAN_SNAPSHOT=(\S+?)\s*$", dockerfile_text
            )
        ]
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
        hardcoded_snapshots = [
            stamp
            for stamp in re.findall(
                r"(?i:snapshot\.debian\.org)/archive/[^/\s\"']+/([^/\s\"']+)/",
                dockerfile_text,
            )
            if stamp != "${DEBIAN_SNAPSHOT}"
        ]
        if hardcoded_snapshots:
            errors.append(
                "kimicode4lunarwing/Dockerfile snapshot.debian.org URLs must use "
                "${DEBIAN_SNAPSHOT}, not a hardcoded or renamed value: "
                f"{hardcoded_snapshots}"
            )
        # MR !1644 review HN-6: every DEBIAN_SNAPSHOT mention must be a
        # plain ARG declaration or a literal ${DEBIAN_SNAPSHOT} inside a
        # snapshot URL; every apt deb source must be a snapshot URL.
        snapshot_arg_line = re.compile(r"^\s*(?i:ARG)\s+DEBIAN_SNAPSHOT(?:=\S+)?\s*$")
        snapshot_url_reference = re.compile(
            r"(?i:snapshot\.debian\.org)/archive/[^/\s\"']+/\$\{DEBIAN_SNAPSHOT\}(?:[/\s]|$)"
        )
        stray_references = [
            line.strip()
            for line in dockerfile_text.splitlines()
            if "DEBIAN_SNAPSHOT" in line
            and not snapshot_arg_line.match(line)
            and not snapshot_url_reference.search(line)
        ]
        if stray_references:
            errors.append(
                "kimicode4lunarwing/Dockerfile DEBIAN_SNAPSHOT references must be "
                "ARG declarations or ${DEBIAN_SNAPSHOT} snapshot URLs: "
                f"{stray_references}"
            )
        deb_source = re.compile(r"(?:^|[\"'\s])deb\s+https?://")
        snapshot_source = re.compile(
            r"deb\s+https?://snapshot\.debian\.org/archive/", re.IGNORECASE
        )
        non_snapshot_sources = [
            line.strip()
            for line in dockerfile_text.splitlines()
            if deb_source.search(line) and not snapshot_source.search(line)
        ]
        if non_snapshot_sources:
            errors.append(
                "kimicode4lunarwing/Dockerfile apt deb sources must use "
                f"snapshot.debian.org: {non_snapshot_sources}"
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


def validate_include_shape(root: dict, loaded_configs: list, errors: list[str]) -> None:
    """Only plain local root includes, and no nested includes.

    MR !1658 review IA-4/IA-7: GitLab merges includes declared inside an
    included file (local, remote, template, project) and honours
    `include:rules` on a root entry, but this validator reads exactly the
    seven root entries. A nested `{remote: <url>}` in a later include would
    replace a pinned lane script with content that is not even in the
    repository, and a conditional root include would drop both lanes (and
    ci-config itself) from the pipelines its rule excludes. Refuse both
    shapes outright.
    """
    for item in root.get("include", []):
        if not isinstance(item, dict) or set(item) != {"local"}:
            errors.append(f"root include entries must be plain local includes: {item!r}")
    for path, config in loaded_configs[1:]:
        if isinstance(config, dict) and "include" in config:
            errors.append(
                f"{path.relative_to(ROOT)} must not include further files; nested "
                "includes are merged by GitLab but invisible to this validator"
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
    validate_include_shape(root, loaded_configs, errors)
    merged_variables = collect_variables(loaded_configs)
    # GitLab deep-merges the includes in order and then merges the root
    # file over them — the root always wins. The previous root-first
    # shallow update() let a root-file entry for a pinned job override the
    # include's definition while the validators below kept checking the
    # include's block (MR !1647 review HP-2). On the real tree no top-level
    # key is defined in two files, so this reorder leaves the merged view
    # identical.
    merged_config: dict = {}
    for _path, config in loaded_configs[1:]:
        merged_config = merge_ci_hashes(merged_config, config)
    merged_config = merge_ci_hashes(merged_config, loaded_configs[0][1])
    # The workflow is read from the merged view: a root-file `workflow:`
    # replaces the include's rules array in GitLab (MR !1658 review IA-1).
    workflow = dict(merged_config.get("workflow", {}))
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
