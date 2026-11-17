#!/usr/bin/env python3
"""WI#665: evaluate the managed Console policy on a built worker image.

Runs the lane's real opencode binary inside the image against a fake OpenCode
Console on loopback. A credential whose `metadata.server` names the fake makes
the plugin talk to it, so nothing leaves the container (`--network=none`).
Six runs:

  1. control: an empty directory in place of `/etc/opencode`;
  2. the image's own managed policy;
  3. the image's policy plus a project `opencode.json` that allows the
     integration;
  4. control with a credential that names an organization, so the policy
     resource is `opencode#<org>` (the form a saved Console login takes);
  5. the image's policy with that credential;
  6. the image's policy while the Console's config also carries a policy
     statement (`provider.use` deny for the lane's own provider).

The questions are answered by observation, not by reading:

  * does the policy make the plugin stop talking to the Console at all?
  * does a Console-authored skill, provider or MCP server still reach the
    session with it in place, for either form of the credential?
  * what is left of the Console's config document in `/api/config`?
  * does a policy statement in the Console's config still apply? Upstream
    keeps a denied integration's `policies` on purpose
    (`packages/core/src/config.ts:343-349` at the v2.2.7 pin), so the recorded
    answer is yes: the content is withheld, the policy applies. Run 6 asserts
    that behaviour as it is, so a bump that changes it is noticed.

Instructions and skills share one gate upstream
(`syncInstructionsAndSkills(allowed)`), and the skill list is the half the
server API exposes.

    python3 scripts/console_policy_eval.py --image localhost/lunarwing-worker-opencode2:candidate

Exit status is 0 when every run completed and each result is the recorded
one, 1 when a run failed, the deny did not hold or the recorded behaviour
changed, 2 when a control run proved nothing (so a silent policy run would
prove nothing either).
"""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

LANE_ROOT = pathlib.Path(__file__).resolve().parent.parent
RUNNER = LANE_ROOT / "scripts" / "console_policy_eval_runner.ts"
LANE_CONFIG = LANE_ROOT / "config" / "opencode2.json"

# The credential and skill names the runner uses, repeated here so the driver
# can judge the result without trusting the runner's own verdict.
SKILL_MARKER = "wi665-console-eval"
# The organization the saved credential names in runs 4 and 5.
ORGANIZATION = "org_01"


def _lane_provider() -> str:
    """The lane's own provider ID, the one run 6's Console policy denies."""
    providers = json.loads(LANE_CONFIG.read_text(encoding="utf-8")).get("providers")
    if not isinstance(providers, dict) or not providers:
        raise SystemExit(f"{LANE_CONFIG} defines no provider for the Console policy run")
    return sorted(providers)[0]


def _prepare(root: pathlib.Path) -> pathlib.Path:
    work = root / "workdir"
    work.mkdir(parents=True)
    shutil.copy(LANE_CONFIG, work / "opencode2.json")
    (root / "config-dir").mkdir()
    (root / "empty-etc").mkdir()
    shutil.copy(RUNNER, root / RUNNER.name)
    # The image runs as the task uid (10003); the mounted tree is read-only
    # but must be readable, and the working tree must be writable for the
    # server's own state.
    for path in (root, root / "config-dir", root / "empty-etc"):
        path.chmod(0o755)
    work.chmod(0o777)
    for path in (root.rglob("*")):
        if path.is_file():
            path.chmod(0o644)
    return root


def _set_project_allow(root: pathlib.Path, present: bool) -> None:
    """A project document that *allows* what the managed policy denies.

    Managed policies load last and evaluate first, so this must not lift the
    deny; a project file could not lift the Console document either (upstream:
    "a project file can never strip the Console document carrying org policy").
    """
    path = root / "workdir" / "opencode.json"
    if present:
        path.write_text(
            json.dumps(
                {
                    "policies": [
                        {"action": "integration.use", "resource": "opencode*", "effect": "allow"},
                    ]
                }
            ),
            encoding="utf-8",
        )
        path.chmod(0o644)
    elif path.exists():
        path.unlink()


def _run_container(
    image: str,
    root: pathlib.Path,
    *,
    control: bool,
    switches: dict[str, str] | None = None,
) -> dict:
    command = [
        "podman", "run", "--rm",
        "--network=none",
        "--volume", f"{root}:/eval:ro,z",
        "--workdir", "/eval/workdir",
        "--entrypoint", "bun",
    ]
    for name, value in (switches or {}).items():
        command += ["--env", f"{name}={value}"]
    if control:
        # Mask the managed policy so the same image shows the unguarded path.
        command += ["--volume", f"{root / 'empty-etc'}:/etc/opencode:ro,z"]
    command += [image, f"/eval/{RUNNER.name}"]
    print(f"$ {' '.join(command)}", file=sys.stderr)
    result = subprocess.run(command, capture_output=True, text=True, timeout=600)
    payloads = [
        line[len("EVAL_JSON:"):]
        for line in result.stdout.splitlines()
        if line.startswith("EVAL_JSON:")
    ]
    if not payloads:
        raise SystemExit(
            f"run produced no result (exit {result.returncode}):\n"
            f"{result.stdout[-4000:]}\n{result.stderr[-4000:]}"
        )
    payload = json.loads(payloads[-1])
    payload["exit_code"] = result.returncode
    payload["control"] = control
    return payload


def _console_skills(run: dict) -> list[str]:
    return [skill for skill in run.get("consoleSkills", []) if SKILL_MARKER in skill]


def _console_content(run: dict) -> list[str]:
    """Everything Console-authored that reached the session in this run."""
    return [
        *_console_skills(run),
        *run.get("consoleProviders", []),
        *run.get("consoleMcpServers", []),
    ]


def _document_verdict(control: dict, guarded: dict) -> tuple[int, str]:
    """What /api/config lists for the Console integration, with and without the deny.

    The runner reports the documents the API lists without a path. Upstream
    replaces a denied integration's document with a copy that holds only its
    policies, so the guarded run should still list one, with no other key.
    """
    control_documents = [d for d in control.get("integrationDocuments", []) if isinstance(d, dict)]
    guarded_documents = [d for d in guarded.get("integrationDocuments", []) if isinstance(d, dict)]
    if not any("providers" in document and "mcp" in document for document in control_documents):
        return 2, (
            "RESULT: INCONCLUSIVE for the integration document — /api/config in the control "
            "run lists no path-less document with the Console's providers and MCP servers, "
            "so the probe does not see the document"
        )
    extra = sorted({key for document in guarded_documents for key in document} - {"policies"})
    if extra:
        return 1, (
            "RESULT: INTEGRATION DOCUMENT NOT REDUCED — with the image policy /api/config "
            f"still lists it with {extra}"
        )
    return 0, (
        "RESULT: INTEGRATION DOCUMENT REDUCED TO ITS POLICIES — the control's document "
        "carried the Console's providers and MCP servers; with the image policy "
        f"/api/config lists {json.dumps(guarded_documents)}"
    )


def _organization_verdict(control: dict, guarded: dict) -> tuple[int, str]:
    """Runs 4 and 5: the credential form a saved Console login takes."""
    resource = f"opencode#{ORGANIZATION}"
    if any(run.get("credentialOrganization") != ORGANIZATION for run in (control, guarded)):
        return 2, (
            "RESULT: INCONCLUSIVE for an organization credential — the runner did not "
            f"report a credential for {ORGANIZATION}"
        )
    arrived = (
        _console_skills(control)
        and control.get("consoleProviders")
        and control.get("consoleMcpServers")
    )
    if not arrived:
        return 2, (
            "RESULT: INCONCLUSIVE for an organization credential — its control run did "
            "not get the Console skill, provider and MCP server"
        )
    if _console_content(guarded):
        return 1, (
            f"RESULT: DENY DOES NOT HOLD FOR AN ORGANIZATION CREDENTIAL ({resource}) — "
            f"reached the session: {_console_content(guarded)}"
        )
    return 0, (
        f"RESULT: DENY HOLDS FOR AN ORGANIZATION CREDENTIAL ({resource}) — its control "
        "got the Console skill, provider and MCP server and the guarded run got none"
    )


def _console_policy_verdict(guarded: dict, console_policy: dict, provider: str) -> tuple[int, str]:
    """Run 6: the known residual, asserted as it is today."""
    if console_policy.get("consolePolicyDeniesProvider") != provider:
        return 2, (
            "RESULT: INCONCLUSIVE for a Console policy — the runner did not report a "
            f"Console policy statement for provider {provider!r}"
        )
    if _console_content(console_policy):
        return 1, (
            "RESULT: DENY DOES NOT HOLD WHEN THE CONSOLE CARRIES A POLICY — reached the "
            f"session: {_console_content(console_policy)}"
        )
    if provider not in guarded.get("providerIDs", []):
        return 2, (
            f"RESULT: INCONCLUSIVE for a Console policy — the lane's provider {provider!r} "
            "is not listed without one either, so its absence would prove nothing"
        )
    if provider in console_policy.get("providerIDs", []):
        return 1, (
            "RESULT: CHANGED — A CONSOLE POLICY NO LONGER APPLIES: the Console's "
            f"`provider.use` deny left the lane's provider {provider!r} listed. The "
            "recorded residual (docs/ops/WORKER-CONTAINERS.md, WI#665) says it applies; "
            "re-read upstream and update the record"
        )
    return 0, (
        "RESULT: KNOWN RESIDUAL, AS RECORDED — the Console's skill, provider and MCP "
        f"server were withheld, but its `provider.use` deny applied: the lane's provider "
        f"{provider!r} is listed without that statement and gone with it; /api/config "
        f"lists the integration document as {json.dumps(console_policy.get('integrationDocuments'))}"
    )


def _verdict(runs: dict[str, dict], provider: str) -> tuple[int, list[str]]:
    lines = []
    control, guarded, project_allow = runs["control"], runs["guarded"], runs["project_allow"]
    control_skills = _console_skills(control)
    guarded_skills = _console_skills(guarded)
    allow_skills = _console_skills(project_allow)
    control_requests = control.get("consoleRequests", [])
    guarded_requests = guarded.get("consoleRequests", [])
    labels = {
        "control": "control (no managed policy)",
        "guarded": "guarded (image policy)",
        "project_allow": "guarded + project allow",
        "organization_control": f"control, credential for {ORGANIZATION}",
        "organization_guarded": f"guarded, credential for {ORGANIZATION}",
        "console_policy": f"guarded, Console policy denies provider {provider}",
    }

    for name, label in labels.items():
        run = runs[name]
        lines.append(
            f"{label}: ok={run.get('ok')} policy_present={run.get('managedPolicyPresent')} "
            f"requests={len(run.get('consoleRequests', []))} "
            f"console_skills={len(_console_skills(run))} "
            f"console_providers={run.get('consoleProviders', [])} "
            f"console_mcp={run.get('consoleMcpServers', [])} "
            f"lane_provider_listed={provider in run.get('providerIDs', [])}"
        )
        if run.get("error"):
            lines.append(f"  error: {run['error']}")
        for request in run.get("consoleRequests", []):
            lines.append(f"  {request}")

    if not all(run.get("ok") for run in runs.values()):
        lines.append("RESULT: INCONCLUSIVE — a run failed")
        return 1, lines
    if not control_skills:
        lines.append(
            "RESULT: INCONCLUSIVE — the control run registered no Console skill, so the "
            "guarded run could not distinguish a withheld skill from a broken harness"
        )
        return 2, lines
    if not control_requests:
        lines.append("RESULT: INCONCLUSIVE — the control run made no Console request")
        return 2, lines

    if guarded_skills:
        lines.append("RESULT: DENY DOES NOT HOLD — a Console-authored skill reached the session")
        status = 1
    else:
        lines.append(
            "RESULT: DENY HOLDS for Console content — the control registered a Console skill "
            "and the guarded run registered none"
        )
        status = 0
    if allow_skills:
        lines.append(
            "RESULT: A PROJECT ALLOW LIFTS THE DENY — managed policies were not decisive"
        )
        status = 1
    else:
        lines.append(
            "RESULT: A PROJECT ALLOW DOES NOT LIFT THE DENY — the managed statement outranks it"
        )
    control_merge = bool(control.get("consoleProviders")) or bool(control.get("consoleMcpServers"))
    guarded_merge = bool(guarded.get("consoleProviders")) or bool(guarded.get("consoleMcpServers"))
    if not control_merge:
        lines.append(
            "RESULT: INCONCLUSIVE for the config merge — the control run got no Console "
            "provider or MCP server, so the guarded run proves nothing there"
        )
        status = 2
    elif guarded_merge:
        lines.append(
            "RESULT: CONFIG MERGE NOT WITHHELD — a Console provider or MCP server reached "
            "the guarded session"
        )
        status = 1
    else:
        lines.append(
            "RESULT: CONFIG MERGE WITHHELD — the control got a Console provider and MCP "
            "server and the guarded run got neither"
        )
    if guarded_requests:
        lines.append(
            "RESULT: DENY DOES NOT STOP THE REQUESTS — the guarded run still contacted the "
            "Console; the policy withholds what it supplies, not the egress"
        )
    else:
        lines.append(
            "RESULT: DENY ALSO STOPS THE REQUESTS — the guarded run made no Console request"
        )
    for extra_status, line in (
        _document_verdict(control, guarded),
        _organization_verdict(runs["organization_control"], runs["organization_guarded"]),
        _console_policy_verdict(guarded, runs["console_policy"], provider),
    ):
        lines.append(line)
        # A failed deny (1) outranks an inconclusive control (2), which
        # outranks a pass.
        if extra_status == 1 or (extra_status == 2 and status == 0):
            status = extra_status
    return status, lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--image",
        default="localhost/lunarwing-worker-opencode2:candidate",
        help="built worker image to evaluate",
    )
    args = parser.parse_args()
    if not RUNNER.is_file():
        print(f"missing evaluation runner {RUNNER}", file=sys.stderr)
        return 1
    provider = _lane_provider()
    organization = {"WI665_EVAL_ORG": ORGANIZATION}

    runs = {}
    with tempfile.TemporaryDirectory(prefix="wi665-eval-") as temporary:
        root = _prepare(pathlib.Path(temporary).resolve())
        runs["control"] = _run_container(args.image, root, control=True)
        runs["guarded"] = _run_container(args.image, root, control=False)
        _set_project_allow(root, True)
        runs["project_allow"] = _run_container(args.image, root, control=False)
        _set_project_allow(root, False)
        runs["organization_control"] = _run_container(
            args.image, root, control=True, switches=organization,
        )
        runs["organization_guarded"] = _run_container(
            args.image, root, control=False, switches=organization,
        )
        runs["console_policy"] = _run_container(
            args.image, root, control=False, switches={"WI665_EVAL_DENY_PROVIDER": provider},
        )

    status, lines = _verdict(runs, provider)
    print("\n".join(lines))
    return status


if __name__ == "__main__":
    sys.exit(main())
