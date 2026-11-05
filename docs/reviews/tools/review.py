#!/usr/bin/env python3
"""Harness-agnostic helpers for the open-MR review loop (see ../REVIEW-RUNBOOK.md).

Subcommands:
  status                 open MRs against the target vs the state file (NEW/CHANGED/UNCHANGED)
  jobs PIPELINE          job counts and every non-success job of one pipeline
  post IID FILE          post a staged note file as a new discussion thread on an MR
  note-id IID NOTE_ID    record a note id in the state file by hand
  merge-state FRAGMENT   merge a run fragment (mrs, runs_entry, scalars) into the state file
  prefix [N]             print the next N review prefixes (does not consume them)

Only the Python standard library is used. GitLab access needs GITLAB_TOKEN, or
GITLAB_TOKEN_FILE naming a file that holds the token (a bare token or a KEY=value line).
Nothing here ever prints the token.
"""

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
STATE_DEFAULT = HERE.parent / "open-mr-review-state.json"
URL_DEFAULT = os.environ.get("GITLAB_URL", "https://git.lunarwing.org")
PROJECT_DEFAULT = os.environ.get("GITLAB_PROJECT", "lunarwing/lunarwing2")
TARGET_DEFAULT = os.environ.get("REVIEW_TARGET", "aragents/kimil00p/v2.1.8.0")


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def short(sha):
    return (sha or "")[:9]


def die(msg, code=1):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(code)


# ---------------------------------------------------------------- GitLab API


def load_token():
    tok = os.environ.get("GITLAB_TOKEN", "").strip()
    if tok:
        return tok
    path = os.environ.get("GITLAB_TOKEN_FILE", "").strip()
    if not path:
        die("set GITLAB_TOKEN, or GITLAB_TOKEN_FILE naming a file that holds only the token")
    try:
        lines = [ln.strip() for ln in Path(path).expanduser().read_text().splitlines() if ln.strip()]
    except OSError as exc:
        die(f"cannot read GITLAB_TOKEN_FILE: {exc.strerror}")
    if not lines:
        die("GITLAB_TOKEN_FILE is empty")
    tok = lines[-1]
    if "=" in tok:  # tolerate `export GITLAB_TOKEN=...` style files
        tok = tok.split("=", 1)[1]
    return tok.strip().strip("'\"")


class Api:
    def __init__(self, url, project):
        self.base = url.rstrip("/") + "/api/v4/projects/" + urllib.parse.quote(project, safe="")
        self.token = load_token()

    def call(self, method, path, params=None, data=None):
        url = self.base + path
        if params:
            url += "?" + urllib.parse.urlencode(params)
        body = urllib.parse.urlencode(data).encode() if data else None
        req = urllib.request.Request(url, data=body, method=method)
        req.add_header("PRIVATE-TOKEN", self.token)
        if body is not None:
            req.add_header("Content-Type", "application/x-www-form-urlencoded")
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return json.load(resp)
        except urllib.error.HTTPError as exc:
            detail = exc.read(400).decode("utf-8", errors="replace")
            die(f"{method} {path} -> HTTP {exc.code}: {detail}")
        except urllib.error.URLError as exc:
            die(f"{method} {path} -> {exc.reason}")

    def get(self, path, **params):
        return self.call("GET", path, params or None)

    def post(self, path, **data):
        return self.call("POST", path, None, data)

    def branch_head(self, name):
        return self.get("/repository/branches/" + urllib.parse.quote(name, safe=""))["commit"]["id"]


# ---------------------------------------------------------------- state file


def load_state(path):
    try:
        return json.loads(Path(path).read_text())
    except FileNotFoundError:
        die(f"state file not found: {path}")
    except json.JSONDecodeError as exc:
        die(f"state file is not valid JSON: {exc}")


def save_state(path, state):
    path = Path(path)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(state, indent=2, ensure_ascii=False) + "\n")
    os.replace(tmp, path)


def record_note(state_path, iid, note_id, created_at, head_sha=None):
    state = load_state(state_path)
    entry = state.setdefault("mrs", {}).setdefault(str(iid), {})
    ids = entry.setdefault("posted_note_ids", [])
    if note_id not in ids:
        ids.append(note_id)
    if not entry.get("posted_note_id"):
        entry["posted_note_id"] = note_id
    entry["posted_at"] = created_at
    if head_sha:
        # The head that was reviewed. Only `post --head` and `note-id --head`
        # pass one; a CI addendum (`--any-head`) leaves the recorded head alone,
        # so a push after the review still shows as CHANGED in `status`.
        entry["head_sha"] = head_sha
    save_state(state_path, state)
    print(f"state: !{iid} posted_note_ids={ids} posted_at={created_at}")


def next_prefix(prefix):
    if len(prefix) != 2 or not prefix.isalpha() or not prefix.isupper():
        die(f"next_prefix in the state file is not two capital letters: {prefix!r}")
    first, second = prefix
    if second != "Z":
        return first + chr(ord(second) + 1)
    if first == "Z":
        die("prefix space exhausted at ZZ")
    return chr(ord(first) + 1) + "A"


# ---------------------------------------------------------------- commands


def cmd_status(args):
    api = Api(args.url, args.project)
    state = load_state(args.state)
    known = state.get("mrs", {})
    heads = {name: api.branch_head(name) for name in (args.target, "master")}
    open_mrs = api.get("/merge_requests", state="opened", per_page=100)

    rows, other = [], []
    for mr in open_mrs:
        if mr["target_branch"] != args.target or mr.get("draft") or mr.get("work_in_progress"):
            other.append(mr)
            continue
        full = api.get(f"/merge_requests/{mr['iid']}")
        prev = (known.get(str(mr["iid"])) or {})
        prev_head = prev.get("head_sha")
        if not prev_head:
            status = "NEW"
        elif prev_head == full["sha"]:
            status = "UNCHANGED"
        else:
            status = "CHANGED"
        pipe = full.get("head_pipeline") or {}
        rows.append({
            "iid": int(mr["iid"]),
            "status": status,
            "head": full["sha"],
            "known_head": prev_head,
            "prefix": prev.get("prefix"),
            "posted_run": prev.get("posted_run"),
            "notes": prev.get("posted_note_ids") or [],
            "merge_status": full.get("detailed_merge_status"),
            "pipeline": pipe.get("id"),
            "pipeline_status": pipe.get("status"),
            "author": mr["author"]["username"],
            "source": mr["source_branch"],
            "title": mr["title"],
            "updated": mr["updated_at"],
        })
    rows.sort(key=lambda r: -r["iid"])

    if args.json:
        print(json.dumps({"checked_at": now_iso(), "target": args.target, "heads": heads,
                          "last_run": state.get("last_run"), "next_prefix": state.get("next_prefix"),
                          "mrs": rows,
                          "out_of_scope": [{"iid": int(m["iid"]), "target": m["target_branch"],
                                            "source": m["source_branch"], "draft": bool(m.get("draft"))}
                                           for m in other]}, indent=2))
        return

    print(f"{now_iso()}  state: last_run={state.get('last_run')} next_prefix={state.get('next_prefix')}")
    print(f"target {args.target} {short(heads[args.target])}   master {short(heads['master'])}")
    print(f"{'IID':<6}{'STATUS':<11}{'HEAD':<11}{'KNOWN':<11}{'PFX':<5}{'RUN':<5}{'MERGE':<13}{'PIPELINE':<16}{'AUTHOR':<12}SOURCE | TITLE")
    for r in rows:
        pipe = f"{r['pipeline']} {r['pipeline_status']}" if r["pipeline"] else "-"
        print(f"{r['iid']:<6}{r['status']:<11}{short(r['head']):<11}{short(r['known_head']) or '-':<11}"
              f"{r['prefix'] or '-':<5}{r['posted_run'] or '-':<5}{r['merge_status'] or '-':<13}"
              f"{pipe:<16}{r['author']:<12}{r['source']} | {r['title'][:60]}")
    todo = [r["iid"] for r in rows if r["status"] != "UNCHANGED"]
    print(f"to review: {', '.join('!' + str(i) for i in todo) or 'none'}  (open against target: {len(rows)})")
    if other:
        desc = []
        for m in other:
            tag = " draft" if m.get("draft") else ""
            tag += " inttest" if m["source_branch"].startswith("sweetiebot/test-") else ""
            desc.append(f"!{m['iid']} -> {m['target_branch']} ({m['source_branch']}{tag})")
        print("out of scope: " + "; ".join(desc))


def cmd_jobs(args):
    api = Api(args.url, args.project)
    pipe = api.get(f"/pipelines/{args.pipeline}")
    jobs = api.get(f"/pipelines/{args.pipeline}/jobs", per_page=100)
    counts = Counter(j["status"] for j in jobs)
    print(f"pipeline {pipe['id']} {pipe['status']} sha {short(pipe['sha'])} ref {pipe['ref']} "
          f"created {pipe['created_at']} finished {pipe.get('finished_at')}")
    print("jobs: " + ", ".join(f"{k}={v}" for k, v in sorted(counts.items())) + f" (total {len(jobs)})")
    for job in jobs:
        if job["status"] != "success":
            print(f"  {job['id']}  {job['status']:<10} {job['name']}")


def cmd_post(args):
    body = Path(args.file).read_text()
    if not body.strip():
        die(f"{args.file} is empty")
    if not args.head and not args.any_head:
        die("pass --head SHA (the head you reviewed) or --any-head to skip the check")
    api = Api(args.url, args.project)
    mr = api.get(f"/merge_requests/{args.iid}")
    if mr["state"] != "opened":
        die(f"refused: !{args.iid} is {mr['state']}", 3)
    if args.head and not mr["sha"].startswith(args.head):
        die(f"refused: !{args.iid} head is {short(mr['sha'])}, you reviewed {short(args.head)}; "
            "review the new head before posting", 3)
    print(f"!{args.iid} head {short(mr['sha'])} ({mr['detailed_merge_status']}), "
          f"{len(body)} chars from {args.file}, first line: {body.splitlines()[0][:80]}")
    if args.dry_run:
        print("dry run: nothing posted")
        return
    disc = api.post(f"/merge_requests/{args.iid}/discussions", body=body)
    note = disc["notes"][0]
    print(f"posted note {note['id']} at {note['created_at']}")
    if args.record:
        reviewed = mr["sha"] if args.head else None
        record_note(args.state, args.iid, int(note["id"]), note["created_at"], reviewed)


def cmd_note_id(args):
    record_note(args.state, args.iid, args.note_id, args.at or now_iso(), args.head)


def cmd_merge_state(args):
    state = load_state(args.state)
    try:
        frag = json.loads(Path(args.fragment).read_text())
    except (OSError, json.JSONDecodeError) as exc:
        die(f"cannot read fragment: {exc}")
    run = frag.get("last_run")
    if run is None:
        die("fragment has no last_run")
    if not args.force and state.get("last_run") != run - 1:
        die(f"state last_run is {state.get('last_run')}, fragment is for run {run}; "
            "already merged, or a run is missing (use --force to merge anyway)", 3)
    entry = frag.get("runs_entry")
    if entry is not None:
        if entry.get("run") != run:
            die(f"runs_entry.run {entry.get('run')} != last_run {run}")
        if any(r.get("run") == run for r in state.get("runs", [])):
            die(f"state already has a runs entry for run {run}", 3)

    changes = []
    for iid, new in (frag.get("mrs") or {}).items():
        cur = state.setdefault("mrs", {}).get(str(iid)) or {}
        for key, val in new.items():
            if key.startswith("_"):
                continue
            if key == "posted_note_ids":
                merged = list(cur.get(key) or [])
                merged += [v for v in (val or []) if v not in merged]
                cur[key] = merged
            else:
                cur[key] = val
        state["mrs"][str(iid)] = cur
        changes.append(f"mrs[{iid}] <- {sorted(k for k in new if not k.startswith('_'))}")
    if entry is not None:
        state.setdefault("runs", []).append(entry)
        changes.append(f"runs += run {run}")
    for key in ("last_run", "last_run_at", "report", "next_prefix"):
        if key in frag:
            state[key] = frag[key]
            changes.append(f"{key} = {frag[key]}")
    if frag.get("target_head"):
        state.setdefault("target_heads", {})[f"run{run}"] = frag["target_head"]
        changes.append(f"target_heads[run{run}] = {frag['target_head']}")
    if frag.get("report") and frag["report"] not in state.setdefault("previous_reports", []):
        state["previous_reports"].append(frag["report"])

    for line in changes:
        print(("would apply: " if args.dry_run else "applied: ") + line)
    if not args.dry_run:
        save_state(args.state, state)
        print(f"state written: last_run={state['last_run']} next_prefix={state.get('next_prefix')}")


def cmd_prefix(args):
    state = load_state(args.state)
    cur = state.get("next_prefix")
    out = []
    for _ in range(args.count):
        out.append(cur)
        cur = next_prefix(cur)
    print(" ".join(out))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--state", default=str(STATE_DEFAULT), help=f"state file (default {STATE_DEFAULT})")
    parser.add_argument("--url", default=URL_DEFAULT, help="GitLab base URL ($GITLAB_URL)")
    parser.add_argument("--project", default=PROJECT_DEFAULT, help="project path ($GITLAB_PROJECT)")
    parser.add_argument("--target", default=TARGET_DEFAULT, help="review target branch ($REVIEW_TARGET)")
    sub = parser.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("status", help="open MRs against the target vs the state file")
    p.add_argument("--json", action="store_true")
    p.set_defaults(func=cmd_status)

    p = sub.add_parser("jobs", help="job summary of one pipeline")
    p.add_argument("pipeline")
    p.set_defaults(func=cmd_jobs)

    p = sub.add_parser("post", help="post a staged note file as a new MR discussion")
    p.add_argument("iid")
    p.add_argument("file")
    p.add_argument("--head", help="head sha (prefix ok) the note was written for; refused if the MR moved")
    p.add_argument("--any-head", action="store_true", help="skip the head check")
    p.add_argument("--record", action="store_true",
                   help="record the note id in the state file (with --head, also the reviewed head)")
    p.add_argument("--dry-run", action="store_true")
    p.set_defaults(func=cmd_post)

    p = sub.add_parser("note-id", help="record an already-posted note id")
    p.add_argument("iid")
    p.add_argument("note_id", type=int)
    p.add_argument("--at", help="created_at (default now)")
    p.add_argument("--head", help="head sha to store if the entry has none")
    p.set_defaults(func=cmd_note_id)

    p = sub.add_parser("merge-state", help="merge a run fragment into the state file")
    p.add_argument("fragment")
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--force", action="store_true", help="skip the last_run+1 check")
    p.set_defaults(func=cmd_merge_state)

    p = sub.add_parser("prefix", help="print the next N prefixes")
    p.add_argument("count", nargs="?", type=int, default=1)
    p.set_defaults(func=cmd_prefix)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
