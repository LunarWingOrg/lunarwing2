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
# Fixed build-read inputs of the kimi worker image build (the Dockerfile
# cross-checks kimi-cli/package.json, npm-ci's both package dirs, tsc reads
# tsconfig.json, and the in-build test gate reads entrypoint.sh, hooks/ and
# config/; runtime COPYs also ship the health server and protocol JSON).
# validate_worker_input_manifest requires all of them plus every
# on-disk kimicode4lunarwing/scripts/*.ts.
KIMI_BUILD_INPUT_CORE = (
    "kimicode4lunarwing/Dockerfile",
    "kimicode4lunarwing/entrypoint.sh",
    "kimicode4lunarwing/health_server.py",
    "kimicode4lunarwing/agent_comm_protocol.json",
    "kimicode4lunarwing/config/config.toml",
    "kimicode4lunarwing/hooks/headless_command_guard.mjs",
    "kimicode4lunarwing/kimi-cli/package.json",
    "kimicode4lunarwing/kimi-cli/package-lock.json",
    "kimicode4lunarwing/scripts/package.json",
    "kimicode4lunarwing/scripts/package-lock.json",
    "kimicode4lunarwing/scripts/tsconfig.json",
)
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
    try:
        with path.open(encoding="utf-8") as stream:
            loaded = yaml.safe_load(stream)
    except yaml.YAMLError as error:
        # MR !1647 review HP-5: custom tags such as `!reference` make
        # yaml.safe_load raise a ConstructorError — fail closed with a clean
        # one-line error (via cli()), not a traceback.
        detail = getattr(error, "problem", None) or str(error).splitlines()[0]
        raise ValueError(f"{path.relative_to(ROOT)}: cannot parse YAML: {detail}") from None
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


# MR !1665 review IF-1: the release audit's advisory gates. Each must
# exist, carry exactly .full-validation-context's rules and the pinned
# runner tag, and set no job-level `when` — one rule-level
# `allow_failure: true` (GitLab's documented rules:allow_failure), a
# `rules: [{when: never}]` replacement, or a retag makes a gate optional,
# unreachable, or rerouted while every script pin stays green.
ADVISORY_GATE_JOBS = (
    "cargo-deny",
    "satellite-advisory-sweep",
    "lockfile-only-advisory-guard",
    "default-graph-advisory-guard",
)

# Run-135 IP-1 (MR !1680 review): a gate that never runs its checker is as
# optional as an allow_failure one. `exit 0` before the wrapper (job or
# template before_script, or the script itself), `|| true`, or an echoed
# wrapper name all kept the mention-only pins green, so each gate's effective
# script and bootstrap are pinned exactly and only these keys may survive
# extends resolution. A deliberate change edits these tuples in the same
# commit.
ADVISORY_GATE_SCRIPTS = {
    "cargo-deny": [
        "bash .gitlab/scripts/install-cargo-deny.sh",
        "bash .gitlab/scripts/cargo-deny-check.sh",
    ],
    "satellite-advisory-sweep": [
        "bash .gitlab/scripts/install-cargo-deny.sh",
        "bash .gitlab/scripts/check-satellite-advisories.sh",
    ],
    "lockfile-only-advisory-guard": [
        "bash .gitlab/scripts/check-lockfile-only-advisories.sh",
    ],
    "default-graph-advisory-guard": [
        "bash .gitlab/scripts/check-default-graph-advisories.sh",
    ],
}
ADVISORY_GATE_BEFORE_SCRIPT = [
    'df -h "$CI_PROJECT_DIR"',
    "bash .gitlab/scripts/bootstrap-rust.sh",
    "bash .gitlab/scripts/bootstrap-sccache.sh",
]
ADVISORY_GATE_EFFECTIVE_KEYS = frozenset({
    "after_script", "before_script", "cache", "image", "needs", "rules",
    "script", "stage", "tags",
})

# Run-137 IU-1: artifacts can replace checkers or supply dotenv variables
# outside GLOBAL_VARIABLE_ALLOWLIST. Pin the allowed inputs as well as keys:
# no upstream artifacts or dotenv, exactly the Rust image and pull-only caches.
ADVISORY_GATE_PINNED_VALUES = {
    "image": {"name": "$CI_RUST_IMAGE"},
    "needs": [{"job": "cargo-metadata", "artifacts": False}],
    "cache": [
        {
            "key": {"prefix": "rust-1.96.1-dependencies", "files": ["ic/Cargo.lock"]},
            "paths": [
                ".ci-cache/cargo/registry/index/",
                ".ci-cache/cargo/registry/cache/",
                ".ci-cache/cargo/git/db/",
            ],
            "policy": "pull",
        },
        {
            "key": "rust-1.96.1-sccache",
            "paths": [".ci-cache/bin/", ".ci-cache/sccache/"],
            "policy": "pull",
        },
    ],
}


def validate_advisory_gates_not_optional(merged: dict, errors: list[str]) -> None:
    """The release audit's advisory gates must not be allowed to fail.

    MR !1658 review IA-9: a root-file (or direct) job-level
    `allow_failure: true` turns a red advisory gate advisory-only with a
    one-line diff, and the gates' own script pins here do not notice.
    MR !1665 review IF-1: the same holds one level down — a rule-level
    `allow_failure`, a replaced rules list (even `rules: [{when: never}]`),
    a job-level `when`, or a missing job each leaves the guard green while
    the gate cannot block, so each gate must exist and its extends-resolved
    rules must be exactly .full-validation-context's. The template's own
    rules may carry only `if` (plus the final `when: never`), so the
    equality check cannot inherit a hollowed-out template. The resolved
    tags must be exactly ['$CI_RUST_RUNNER_TAG'] (IF-2): a retagged gate
    runs on an unpinned runner. secret-history is deliberately
    allow_failure while historical findings are triaged and is NOT in this
    set — making another gate optional must stay as visible as that marked
    exception. Any non-false allow_failure value (including the exit_codes
    hash form) counts.

    MR !1665 review IF-2: the EXPECTED_RUNNER_TAGS pin in
    validate_lane_environment_routes covers only the merged top-level
    value; a job-level (or rule-level) `variables:` entry overrides it for
    that job and reroutes the pinned jobs while the top-level pin stays
    green, so no job may name a runner tag variable in any variables
    scope.

    MR !1680 review IP-1 (run-135): a gate that never runs its checker is
    as optional as an allow_failure one, and the checks above did not see
    it — `exit 0` as the first script line or in the job's (or the shared
    .rust-job template's) before_script, `|| true` on the wrapper, or an
    `echo` of the wrapper name all kept the mention-only pins green. Each
    gate's effective script and bootstrap are therefore pinned exactly
    (ADVISORY_GATE_SCRIPTS / ADVISORY_GATE_BEFORE_SCRIPT), and only
    ADVISORY_GATE_EFFECTIVE_KEYS may survive extends resolution —
    job-level variables, hooks, inherit, parallel or timeout can hollow
    out a gate just as well."""
    template = resolve_extends(merged.get(".full-validation-context"), merged)
    template_rules = template.get("rules")
    if not isinstance(template_rules, list) or not template_rules:
        errors.append(".full-validation-context must define rules")
        template_rules = None
    else:
        for rule in template_rules:
            if not isinstance(rule, dict) or not (
                set(rule) == {"if"} or rule == {"when": "never"}
            ):
                errors.append(
                    ".full-validation-context rules may carry only `if` (and a "
                    f"final `when: never`): {rule!r}"
                )
    for job_name in ADVISORY_GATE_JOBS:
        job = merged.get(job_name)
        if not isinstance(job, dict):
            errors.append(
                f"{job_name} is missing; every release-audit advisory gate "
                "must exist"
            )
            continue
        effective = resolve_extends(job, merged)
        if effective.get("allow_failure"):
            errors.append(
                f"{job_name} must not set allow_failure; an advisory gate whose "
                "failure is optional cannot gate the release audit"
            )
        if "when" in effective:
            errors.append(f"{job_name} must not set a job-level when")
        if template_rules is not None and effective.get("rules") != template_rules:
            errors.append(
                f"{job_name} rules must be exactly .full-validation-context's "
                "(no rule-level allow_failure, when or variables)"
            )
        if effective.get("tags") != ["$CI_RUST_RUNNER_TAG"]:
            errors.append(f"{job_name} tags must be exactly ['$CI_RUST_RUNNER_TAG']")
        if effective.get("script") != ADVISORY_GATE_SCRIPTS[job_name]:
            errors.append(
                f"{job_name} script must be exactly {ADVISORY_GATE_SCRIPTS[job_name]!r}"
            )
        if effective.get("before_script") != ADVISORY_GATE_BEFORE_SCRIPT:
            errors.append(
                f"{job_name} before_script must be exactly the pinned .rust-job "
                f"bootstrap {ADVISORY_GATE_BEFORE_SCRIPT!r}"
            )
        for key, expected in ADVISORY_GATE_PINNED_VALUES.items():
            if effective.get(key) != expected:
                errors.append(f"{job_name} {key} must be exactly {expected!r}")
        unexpected = sorted(set(effective) - ADVISORY_GATE_EFFECTIVE_KEYS)
        if unexpected:
            errors.append(
                f"{job_name} must not carry job keys {unexpected}; variables, "
                "hooks, inherit, parallel or timeout can hollow out a gate"
            )
    for job_name, job in merged.items():  # IF-2: runner-tag variables at job or rule level
        if not isinstance(job, dict) or job_name.startswith("."):
            continue
        effective = resolve_extends(job, merged)
        scopes = [effective.get("variables")]
        rules = effective.get("rules")
        if isinstance(rules, list):
            scopes.extend(rule.get("variables") for rule in rules if isinstance(rule, dict))
        for scope in scopes:
            if isinstance(scope, dict) and any(
                name in EXPECTED_RUNNER_TAGS for name in scope
            ):
                errors.append(
                    f"{job_name} must not override a runner tag variable; "
                    "EXPECTED_RUNNER_TAGS pins the routing for every job"
                )
                break


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
        "check advisories bans",
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
            "bun scripts/lunarwing_tool_proxy_test.ts",
            "bun scripts/lunarwing_tool_plugin_test.ts",
            "bun scripts/opencode_task_executor_test.ts",
            "./node_modules/.bin/tsc --noEmit -p scripts/tsconfig.json",
        ),
    ),
    # WI#375 DR-3: the opencode2 bridge's dependency-free TS suites (the
    # executor suite needs the image's built @opencode/client and stays
    # image-gated). Same shape as worker-opencode-ts: digest-tied bun image,
    # lint runner tags, no extends.
    "worker-opencode2-ts": (
        "$CI_BUN_IMAGE",
        (
            "cd opencode2.4lunarwing",
            "bun scripts/path_expand_test.ts",
            "bun scripts/task_prompt_test.ts",
            "bun scripts/opencode_result_test.ts",
            "bun scripts/sdk_surface_test.ts",
            "bun scripts/sdk_surface_assert_test.ts",
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
                "reach the pinned worker lanes (GLOBAL_VARIABLE_ALLOWLIST)"
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
            "variables that reach the pinned worker lanes"
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
        # MR !1691 run-137 review IX-2: lane_runs_on_merge_requests refuses a
        # non-empty needs but accepts a missing one, and GitLab hands a
        # lane without needs (or dependencies) every earlier-stage job's
        # artifacts and dotenv variables — which outrank the YAML variables
        # GLOBAL_VARIABLE_ALLOWLIST guards (NODE_OPTIONS/BUN_OPTIONS turn the
        # pinned test commands into no-ops).
        if effective.get("needs") != []:
            errors.append(
                f"{job_name} must declare needs: [] so no artifacts or dotenv "
                "variables from other jobs reach the pinned script"
            )
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


# WI#375 DR-3: the pebble worker's Rust suite runs in the worker-pebble-rust
# MR lane, no longer gated only at the protected image build. The lane
# extends .rust-job for the toolchain bootstrap and pull-only cache instead
# of carrying a digest-tied image variable, so it gets its own pin in the
# shape of validate_worker_ts_lanes: the extends-resolved job must still run
# on merge_request_event (the HE-1 exact-predicate first-rule gate), resolve
# to the $CI_RUST_IMAGE image and rust runner tag, and run exactly the
# pinned script; the job's own keys are limited to the wiring keys so a
# job-level before_script/after_script (replacing the audited bootstrap),
# image/tags override, variables, hooks, or a parallel matrix is refused.
# The same holds at the template level (MR !1669 review IK-8): the resolved
# job may carry only the audited .rust-job keys, and its inherited
# before_script must equal the pinned bootstrap line for line.
PEBBLE_RUST_LANE_JOB_KEYS = frozenset({
    "stage", "needs", "extends", "rules", "script", "allow_failure", "when",
})
# Keys allowed to survive extends resolution (the .rust-job template keys
# plus the job's own wiring keys), and the exact inherited bootstrap: a
# template-level variables/hooks/inherit/parallel entry or a before_script
# `exit 0` would hollow the lane out just like a job-level override (MR
# !1669 review IK-8). A deliberate change to .rust-job's bootstrap must
# update PEBBLE_RUST_LANE_BEFORE_SCRIPT in the same commit.
PEBBLE_RUST_LANE_EFFECTIVE_KEYS = PEBBLE_RUST_LANE_JOB_KEYS | frozenset({
    "image", "tags", "before_script", "after_script", "cache",
})
PEBBLE_RUST_LANE_BEFORE_SCRIPT = (
    'df -h "$CI_PROJECT_DIR"',
    "bash .gitlab/scripts/bootstrap-rust.sh",
    "bash .gitlab/scripts/bootstrap-sccache.sh",
)
PEBBLE_RUST_LANE_SCRIPT = (
    "bash .gitlab/scripts/cargo-ci.sh test -j2 --locked "
    "--manifest-path ../pebble4lunarwing/Cargo.toml",
)


def validate_pebble_rust_lane(merged: dict, errors: list[str]) -> None:
    job = merged.get("worker-pebble-rust")
    if not isinstance(job, dict):
        errors.append("worker-pebble-rust job missing")
        return
    if job.get("extends") != ".rust-job":
        errors.append(
            "worker-pebble-rust must extend .rust-job (the pinned rust "
            "toolchain bootstrap and pull-only cache template)"
        )
    unexpected = sorted(set(job) - PEBBLE_RUST_LANE_JOB_KEYS)
    if unexpected:
        errors.append(
            f"worker-pebble-rust must not set job keys {unexpected}; "
            "job-level variables, parallel matrices, hooks or a "
            "before_script/after_script can bypass the pinned script"
        )
    effective = resolve_extends(job, merged)
    if effective.get(EXTENDS_TRUNCATED_KEY):
        errors.append(
            f"worker-pebble-rust extends chain exceeds the validation depth "
            f"budget ({EXTENDS_DEPTH_BUDGET}); refusing to certify a "
            "partially resolved job"
        )
    inherited = sorted(
        set(effective) - PEBBLE_RUST_LANE_EFFECTIVE_KEYS - {EXTENDS_TRUNCATED_KEY}
    )
    if inherited:
        errors.append(
            f"worker-pebble-rust must not inherit job keys {inherited}; "
            "template-level variables, hooks, inherit or parallel can bypass "
            "the pinned script"
        )
    before_script = effective.get("before_script")
    if not isinstance(before_script, list) or [
        str(line) for line in before_script
    ] != list(PEBBLE_RUST_LANE_BEFORE_SCRIPT):
        errors.append(
            "worker-pebble-rust before_script must be exactly the pinned "
            f".rust-job bootstrap {list(PEBBLE_RUST_LANE_BEFORE_SCRIPT)!r}"
        )
    # MR !1691 run-137 review IX-1: the .rust-job image is a bare name. An
    # inherited entrypoint (or any other image key) can skip the job script
    # just like the TS lanes' entrypoint override (MR !1627 review HE-2),
    # so pin the whole value, not just image_name().
    if effective.get("image") not in ("$CI_RUST_IMAGE", {"name": "$CI_RUST_IMAGE"}):
        errors.append(
            "worker-pebble-rust must resolve to exactly the $CI_RUST_IMAGE "
            "image (no entrypoint or other image keys)"
        )
    # MR !1691 run-137 review IX-2: a lane without needs: [] in a later stage
    # receives every earlier-stage job's artifacts and dotenv variables
    # (CARGO_TARGET_<triple>_RUNNER=true turns cargo test into a no-op that
    # exits 0).
    if effective.get("needs") != []:
        errors.append(
            "worker-pebble-rust must declare needs: [] so no artifacts or "
            "dotenv variables from other jobs reach the pinned script"
        )
    if effective.get("tags") != ["$CI_RUST_RUNNER_TAG"]:
        errors.append(
            "worker-pebble-rust tags must be exactly ['$CI_RUST_RUNNER_TAG']"
        )
    rules = effective.get("rules")
    if isinstance(rules, list) and any(
        isinstance(rule, dict) and "variables" in rule for rule in rules
    ):
        errors.append(
            "worker-pebble-rust rules must not set variables; "
            "rules:variables reach the pinned script on the pipelines that "
            "rule matches"
        )
    if not lane_runs_on_merge_requests(job, merged):
        errors.append("worker-pebble-rust must run on merge_request_event")
    script = effective.get("script")
    if not isinstance(script, list) or [str(line) for line in script] != list(PEBBLE_RUST_LANE_SCRIPT):
        errors.append(
            "worker-pebble-rust script must be exactly the pinned commands "
            f"{list(PEBBLE_RUST_LANE_SCRIPT)!r} (got {script!r})"
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


CACHE_REFRESH_CONDITION = (
    '$CI_PIPELINE_SOURCE == "push" && $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH '
    '&& $CI_COMMIT_REF_PROTECTED == "true"'
)


def validate_cache_trust(merged: dict, errors: list[str]) -> None:
    """IU-3: validation jobs only read caches; one protected push job writes.

    Check the merged, extends-resolved jobs, including push-only, implicit
    pull-push and variable policies. A substring in the writer's rule is not
    sufficient: an OR branch could also enable it in an MR pipeline.
    """
    for template_name in (".python-job", ".rust-job", ".rust-msrv-job"):
        policies = cache_policies(resolve_extends(merged.get(template_name), merged))
        if not policies or any(policy != "pull" for policy in policies):
            errors.append(f"{template_name} validation caches must be fixed pull-only")

    if merged.get("cache"):
        errors.append("top-level cache is not allowed; declare caches on job templates")
    for name, job in merged.items():
        if not isinstance(job, dict):
            continue
        effective = resolve_extends(job, merged)
        if effective.get(EXTENDS_TRUNCATED_KEY):
            errors.append(f"{name} cache trust cannot validate a truncated extends chain")
        if name != "rust-cache-refresh" and any(
            policy != "pull" for policy in cache_policies(effective)
        ):
            errors.append(f"{name} validation caches must be fixed pull-only")
        cache = effective.get("cache", [])
        entries = cache if isinstance(cache, list) else [cache]
        if any(
            isinstance(entry, dict) and entry.get("unprotect", False) is not False
            for entry in entries
        ):
            errors.append(f"{name} must not use cache:unprotect; keep protected caches separate")

    refresh = resolve_extends(merged.get("rust-cache-refresh"), merged)
    policies = cache_policies(refresh)
    if not policies or any(policy != "pull-push" for policy in policies):
        errors.append("rust-cache-refresh must declare fixed pull-push caches")
    if refresh.get("rules") != [{"if": CACHE_REFRESH_CONDITION}, {"when": "never"}]:
        errors.append("rust-cache-refresh rules must be exactly the protected default-branch push gate")
    if refresh.get("variables"):
        errors.append("rust-cache-refresh must not override variables used by its protected push gate")
    # Exactly one default-feature check per context, while retaining the
    # literal pull-only cache policy on untrusted validation jobs.
    check = resolve_extends(merged.get("rust-check-default"), merged)
    if check.get("rules") != [
        {"if": CACHE_REFRESH_CONDITION, "when": "never"}, {"when": "on_success"}
    ]:
        errors.append("rust-check-default must complement the protected cache writer")
    if check.get("script") != refresh.get("script"):
        errors.append("the cache writer must perform the same default-feature check")
    if refresh.get("needs") != ["cargo-metadata"]:
        errors.append("rust-cache-refresh must check directly after cargo-metadata")
    for name in ("rust-test-lib", "cavepony-wasm"):
        needs = resolve_extends(merged.get(name), merged).get("needs", [])
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
    # included), mt-admin's tenant build, the harness compose file, and the
    # kimi worker's own docker-compose.yml, whose build.args take the same
    # override (MR !1665 review IF-3).
    for label, path in (
        ("build-worker-image.sh", WORKER_BUILD_SCRIPT),
        (
            "ic/scripts/mt-admin-lib/05-tenant-build.sh",
            ROOT / "ic" / "scripts" / "mt-admin-lib" / "05-tenant-build.sh",
        ),
        ("tests/docker-compose.test.yml", ROOT / "tests" / "docker-compose.test.yml"),
        (
            "kimicode4lunarwing/docker-compose.yml",
            ROOT / "kimicode4lunarwing" / "docker-compose.yml",
        ),
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


def worker_input_files(block: str) -> list[str]:
    """Extract a worker case block's input_files list (quoted multi-line or
    bare single-line form)."""
    match = re.search(r"(?m)^        input_files='([^']*)'$", block)
    if match is not None:
        return match.group(1).split("\n")
    match = re.search(r"(?m)^        input_files=(\S+)$", block)
    if match is not None:
        return [match.group(1)]
    return []


def input_files_assignments(build_script: str) -> int:
    """Count assignment lines, including export/declare/readonly and +=."""
    count = 0
    for line in build_script.splitlines():
        if "=" not in line or line.lstrip().startswith("#"):
            continue
        head = line.strip().split("=", 1)[0].split()
        if head and head[-1].rstrip("+") == "input_files":
            count += 1
    return count


def validate_worker_input_manifest(errors: list[str]) -> None:
    """The worker-inputs manifest is the promotion evidence of what went
    into a candidate image (CY-4: build-executed scripts must hash into it).

    Every listed entry must be a safe relative path that exists under the
    repo root — a missing file would otherwise only fail at the manual
    protected candidate build (sha256sum under `set -eu`). Each worker must
    assign input_files exactly once, with no assignments outside the worker
    blocks (IL-3): the shell must hash the same list this check inspects.
    For kimi, whose Dockerfile builder stage compiles and runs every
    `scripts/*.ts` via its in-build test gate, the manifest must additionally
    cover the fixed build-read set and every on-disk scripts/*.ts, so the Dockerfile's
    `COPY scripts/*.ts` glob cannot silently outgrow the recorded inputs.
    The on-disk glob reads the working tree; CI checkouts are clean, so an
    untracked scratch .ts only ever fails a local run."""
    build_script = WORKER_BUILD_SCRIPT.read_text(encoding="utf-8")
    blocks = worker_case_blocks(build_script)
    assignments = input_files_assignments(build_script)
    if assignments != len(blocks):
        errors.append(
            "build-worker-image.sh must assign input_files exactly once per worker "
            f"block ({len(blocks)}), found {assignments}; a reassignment can change "
            "the hashed manifest behind this check"
        )
    for worker in WORKER_NAMES:
        block = blocks.get(worker)
        if block is None:
            continue  # validate_worker_versions already reports the missing block
        if input_files_assignments(block) != 1:
            errors.append(
                f"build-worker-image.sh {worker} block must assign input_files exactly once"
            )
        entries = [entry for entry in worker_input_files(block) if entry]
        if not entries:
            errors.append(
                f"build-worker-image.sh {worker} block must declare a non-empty "
                "input_files manifest"
            )
            continue
        for entry in entries:
            if safe_relative_path(entry) is None:
                errors.append(
                    f"build-worker-image.sh {worker} input {entry!r} is not a "
                    "safe relative path"
                )
            elif not (ROOT / entry).is_file():
                errors.append(
                    f"build-worker-image.sh {worker} input {entry!r} does not exist"
                )
    kimi_block = blocks.get("kimi")
    if kimi_block is None:
        return
    kimi_inputs = set(worker_input_files(kimi_block))
    for required in KIMI_BUILD_INPUT_CORE:
        if required not in kimi_inputs:
            errors.append(
                f"kimi worker-inputs manifest omits build-read input {required!r}"
            )
    scripts_dir = ROOT / "kimicode4lunarwing" / "scripts"
    for source in sorted(scripts_dir.glob("*.ts")):
        relative = source.relative_to(ROOT).as_posix()
        if relative not in kimi_inputs:
            errors.append(
                f"kimi worker-inputs manifest omits {relative!r}; the Dockerfile's "
                "COPY scripts/*.ts makes every bridge source a build input"
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


def cross_check_image_digest(
    variables: dict[str, str],
    name: str,
    dockerfile: pathlib.Path,
    from_package: str,
    display: str,
    errors: list[str],
) -> None:
    """A CI lane image must test with the same content the worker image
    builds and runs with (MR !1552 review ER-5). Fail closed: a missing
    Dockerfile, an unpinned/malformed FROM, or no matching FROM at all must
    not silently dissolve the tie this check exists to enforce (MR !1566
    review EZ-3)."""
    match = DIGEST_PATTERN.search(variables.get(name, ""))
    if not match:
        return
    if not dockerfile.is_file():
        errors.append(
            f"{display} missing; cannot cross-check {name} against its "
            f"{from_package} FROM digest(s)"
        )
        return
    dockerfile_text = dockerfile.read_text(encoding="utf-8")
    # Tie every FROM whose repository's last path segment is the package's
    # (any registry or namespace: `docker.io/node`, a mirror path and a bare
    # `node` all count), followed by a tag/digest/whitespace boundary so a
    # `nodejs`-style name does not stand in for the pinned package (MR !1669
    # review IK-1: the previous full-repository match left `docker.io/node`
    # and mirror-path spellings untied).
    repository = re.escape(from_package.rsplit("/", 1)[-1])
    from_lines = [
        line.strip()
        for line in dockerfile_text.splitlines()
        if re.match(
            rf"^\s*(?i:FROM)\s+(?:--platform=\S+\s+)?(?:\S*/)?{repository}[:@\s]",
            line,
        )
    ]
    if not from_lines:
        errors.append(
            f"{display} has no {from_package} FROM line; cannot cross-check {name}"
        )
    dockerfile_digests = set()
    for from_line in from_lines:
        digest_match = re.search(r"@sha256:([0-9a-f]{64})(?![0-9a-fA-F])", from_line)
        if digest_match:
            dockerfile_digests.add(digest_match.group(1))
        else:
            errors.append(
                f"{display} {from_package} FROM is not pinned "
                f"to a 64-char lowercase sha256 digest: {from_line!r}"
            )
    if dockerfile_digests and dockerfile_digests != {match.group(1)}:
        errors.append(
            f"{name} digest must match the {from_package} digest(s) in {display}"
        )


def validate_ci_tool_image_pins(merged: dict, variables: dict[str, str], errors: list[str]) -> None:
    trust_sensitive = (
        "CI_LINT_IMAGE",
        "CI_RUST_IMAGE",
        "CI_GITLEAKS_IMAGE",
        "CI_BUN_IMAGE",
        "CI_NODE_IMAGE",
    )
    for name in trust_sensitive:
        value = variables.get(name, "")
        if not value:
            errors.append(f"defaults.yml must declare {name}")
        elif not IMAGE_PATTERN.search(value):
            errors.append(f"{name}={value!r} is not digest-pinned")

    # The worker-opencode-ts and worker-opencode2-ts lanes must test with the
    # same bun content the worker images build and run with (MR !1552 review
    # ER-5; the opencode2 tie was added after MR !1669 review IK-2 found the
    # lane's image untied).
    cross_check_image_digest(
        variables, "CI_BUN_IMAGE", OPENCODE_DOCKERFILE, "oven/bun",
        "opencode4lunarwing/Dockerfile", errors,
    )
    cross_check_image_digest(
        variables, "CI_BUN_IMAGE", OPENCODE2_DOCKERFILE, "oven/bun",
        "opencode2.4lunarwing/Dockerfile", errors,
    )
    # The worker-kimi-ts lane must test with the same node content the kimi
    # worker image builds and runs with (MR !1590 review GB-2).
    cross_check_image_digest(
        variables, "CI_NODE_IMAGE", KIMI_DOCKERFILE, "library/node",
        "kimicode4lunarwing/Dockerfile", errors,
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
        # Run-135 IP-2 (MR !1680 review; after MR !1665 review IF-3): every
        # apt source on a line must be a snapshot source, not just one of
        # them (a line-level check let one RUN line add a live mirror beside
        # a snapshot one). Any URL scheme counts (apt's mirror+http,
        # tor+https, file:), the type and field names are matched
        # case-insensitively, a literal printf newline or tab separates
        # tokens, and every URI of a deb822 URIs: field is checked.
        bs = chr(92)
        quotes = chr(34) + chr(39)
        sep = f"(?:{bs}s|{bs}{bs}t)+"
        boundary = f"(?:^|[{quotes}{bs}s]|{bs}{bs}[nt])"
        one_line_source = re.compile(
            f"{boundary}deb(?:-src)?{sep}(?:{bs}[[^{bs}]]*{bs}]{sep})?"
            f"([a-z][a-z0-9+.-]*:{bs}S*)",
            re.IGNORECASE,
        )
        deb822_uris = re.compile(f"{boundary}URIs:([^{quotes}]*)", re.IGNORECASE)
        snapshot_uri = re.compile(
            f"https?://snapshot{bs}.debian{bs}.org/archive/", re.IGNORECASE
        )

        def live_sources(line: str) -> list[str]:
            uris = [found.group(1) for found in one_line_source.finditer(line)]
            for found in deb822_uris.finditer(line):
                value = found.group(1).split(bs + "n")[0].replace(bs + "t", " ")
                uris.extend(value.split() or [""])
            return [uri for uri in uris if not snapshot_uri.match(uri)]

        non_snapshot_sources = [
            line.strip() for line in dockerfile_text.splitlines() if live_sources(line)
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
    validate_advisory_gates_not_optional(merged_config, errors)
    validate_worker_ts_lanes(merged_config, errors)
    validate_pebble_rust_lane(merged_config, errors)
    validate_cache_trust(merged_config, errors)
    validate_worker_versions(errors)
    validate_worker_input_manifest(errors)
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


def cli() -> int:
    """main() plus one clean error path for unreadable/unsupported YAML
    (MR !1647 review HP-5): a ValueError from load_yaml reports the file and
    the parser's problem on one ERROR line instead of a traceback."""
    try:
        return main()
    except ValueError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(cli())
