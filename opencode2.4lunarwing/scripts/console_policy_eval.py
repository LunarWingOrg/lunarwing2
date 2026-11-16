#!/usr/bin/env python3
"""WI#665: evaluate the managed Console policy on a built worker image.

Runs the lane's real opencode binary twice inside the image — once with an
empty directory in place of `/etc/opencode` (the control) and once with the
image's own managed policy — against a fake OpenCode Console on loopback. A
credential whose `metadata.server` names the fake makes the plugin talk to it,
so nothing leaves the container (`--network=none`).

The question the item asks is answered by observation, not by reading:

  * does the policy make the plugin stop talking to the Console at all?
  * does a Console-authored skill still reach the session with it in place?

Instructions and skills share one gate upstream
(`syncInstructionsAndSkills(allowed)`), and the skill list is the half the
server API exposes.

    python3 scripts/console_policy_eval.py --image localhost/lunarwing-worker-opencode2:candidate

Exit status is 0 when both runs completed and the result is unambiguous, 1
when a run failed, 2 when the control run proved nothing (so a silent policy
run would prove nothing either).
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


def _prepare(root: pathlib.Path) -> pathlib.Path:
    work = root / "workdir"
    work.mkdir(parents=True)
    shutil.copy(LANE_CONFIG, work / "opencode2.json")
    (root / "config-dir").mkdir()
    (root / "empty-etc").mkdir()
    shutil.copy(RUNNER, root / RUNNER.name)
    return root


def _run_container(image: str, root: pathlib.Path, *, control: bool) -> dict:
    command = [
        "podman", "run", "--rm",
        "--network=none",
        "--volume", f"{root}:/eval:ro,z",
        "--workdir", "/eval/workdir",
        "--entrypoint", "bun",
    ]
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


def _verdict(control: dict, guarded: dict) -> tuple[int, list[str]]:
    lines = []
    control_skills = [s for s in control.get("consoleSkills", []) if SKILL_MARKER in s]
    guarded_skills = [s for s in guarded.get("consoleSkills", []) if SKILL_MARKER in s]
    control_requests = control.get("consoleRequests", [])
    guarded_requests = guarded.get("consoleRequests", [])

    for label, run in (("control (no managed policy)", control), ("guarded (image policy)", guarded)):
        lines.append(
            f"{label}: ok={run.get('ok')} policy_present={run.get('managedPolicyPresent')} "
            f"requests={len(run.get('consoleRequests', []))} "
            f"console_skills={len([s for s in run.get('consoleSkills', []) if SKILL_MARKER in s])}"
        )
        if run.get("error"):
            lines.append(f"  error: {run['error']}")
        for request in run.get("consoleRequests", []):
            lines.append(f"  {request}")

    if not control.get("ok") or not guarded.get("ok"):
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
    if guarded_requests:
        lines.append(
            "RESULT: DENY DOES NOT STOP THE REQUESTS — the guarded run still contacted the "
            "Console; the policy withholds what it supplies, not the egress"
        )
    else:
        lines.append(
            "RESULT: DENY ALSO STOPS THE REQUESTS — the guarded run made no Console request"
        )
    document = guarded.get("configDocument")
    lines.append(f"guarded integration document: {json.dumps(document)}")
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

    with tempfile.TemporaryDirectory(prefix="wi665-eval-") as temporary:
        root = _prepare(pathlib.Path(temporary).resolve())
        control = _run_container(args.image, root, control=True)
        guarded = _run_container(args.image, root, control=False)

    status, lines = _verdict(control, guarded)
    print("\n".join(lines))
    return status


if __name__ == "__main__":
    sys.exit(main())
