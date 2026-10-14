#!/usr/bin/env python3
"""Regression checks for the worker exposure policy guard (WI#357, control 4).

The guard pins "workers must never self-expose" for the opencode lanes in
plain CI: lane config pins, entrypoint invariants, the binary wrapper, the
bridge session ruleset (including deny-after-allow ordering), and the
Dockerfile wiring. These tests run the real script against the real tree
(pass) and against fixture trees with each guard surface broken (fail).
"""

import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".gitlab" / "scripts" / "check-worker-exposure-policy.sh"

EXECUTOR_RULESET = """
function sessionPermissions() {
  return [
    { action: "shell", resource: "*", effect: "allow" },
    { action: "shell", resource: "*opencode service *", effect: "deny" },
    { action: "shell", resource: "*opencode pair *", effect: "deny" },
    { action: "shell", resource: "*opencode serve *", effect: "deny" },
  ]
}
"""

COMPILE_OUT_COPY = "COPY scripts/compile_out_remote_tunnel.py /build/compile_out_remote_tunnel.py\n"
COMPILE_OUT_RUN = "RUN python3 /build/compile_out_remote_tunnel.py /build/opencode\n"
BUILD_RUN = 'RUN OPENCODE_VERSION="${OPENCODE_VERSION#v}" bun run build -- --single\n'
READONLY_RUN = "RUN chmod -R a-w /app/scripts /app/config /app/entrypoint.sh /app/health_server.py /app/agent_comm_protocol.json\n"
FILESYSTEM_ASSERTION = "RUN python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 --image-filesystem\n"
WRAPPER_ANCHOR_ARG = "ARG LUNARWING_WRAPPER_SHA256=\n"
BINARY_SCAN_RUN = (
    "RUN install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode \\\n"
    "    && python3 /app/scripts/exposure_policy_assert.py --lane /app --profile opencode2 "
    "--binary /usr/local/lib/opencode2/opencode\n"
)

DOCKERFILE_LINES = (
    "FROM example.invalid/bun AS builder\n",
    COMPILE_OUT_COPY,
    COMPILE_OUT_RUN,
    BUILD_RUN,
    "FROM example.invalid/bun AS runtime\n",
    "COPY --from=builder /out/bin/opencode /usr/local/lib/opencode2/opencode\n",
    BINARY_SCAN_RUN,
    "COPY scripts/ ./scripts/\n",
    "COPY config/ ./config/\n",
    "COPY entrypoint.sh health_server.py agent_comm_protocol.json ./\n",
    READONLY_RUN,
    WRAPPER_ANCHOR_ARG,
    FILESYSTEM_ASSERTION,
    "USER 10003:10003\n",
)


class WorkerExposurePolicyGuardTests(unittest.TestCase):
    def setUp(self):
        # Fixture trees must live on an exec-capable filesystem: /tmp is
        # mounted noexec on some dev hosts, and bash silently skips a
        # non-executable PATH shim, which turns the fake git into the real
        # one. The repo's own tree is exec-capable everywhere we run.
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        # FC-8: TemporaryDirectory is 0700; in root CI the no-git lane's PATH
        # lives under this root while the boot suite drops to uid 10003, which
        # could not traverse a 0700 parent ("Permission denied: 'bash'",
        # ci-config jobs 106699/106728). The fixture must be world-traversable;
        # contents stay 0644/0755, so nothing becomes writable for others.
        self.root.chmod(0o755)
        script_dir = self.root / ".gitlab" / "scripts"
        script_dir.mkdir(parents=True)
        shutil.copy(SCRIPT, script_dir / SCRIPT.name)
        # The guard drives the lane's own checker; both lanes travel.
        shutil.copytree(ROOT / "opencode2.4lunarwing", self.root / "opencode2.4lunarwing")
        shutil.copytree(
            ROOT / "opencode4lunarwing",
            self.root / "opencode4lunarwing",
            ignore=shutil.ignore_patterns("kimi-cli", "node_modules", "__pycache__"),
        )
        (self.root / "opencode2.4lunarwing" / "Dockerfile").write_text(
            "".join(DOCKERFILE_LINES), encoding="utf-8"
        )
        (self.root / "opencode2.4lunarwing" / "scripts" / "opencode_task_executor.ts").write_text(
            EXECUTOR_RULESET, encoding="utf-8"
        )

    def run_guard(self, root: pathlib.Path, *, without_git: bool = False) -> subprocess.CompletedProcess:
        env = dict(os.environ)
        if without_git:
            # The ci-config image (python:3.12-slim-bookworm) has no git; the
            # guard must resolve the repo root without it (run-79 EI-1). Build
            # a PATH of just the tools the guard needs, with no git in it.
            minimal = self.root / "nogit-bin"
            minimal.mkdir(exist_ok=True)
            for tool in ("bash", "sh", "python3", "grep", "dirname", "cp", "mkdir",
                         "chmod", "rm", "ln", "date"):
                target = shutil.which(tool)
                if target is None:
                    raise AssertionError(f"required tool {tool!r} not found")
                link = minimal / tool
                if not link.exists():
                    link.symlink_to(target)
            if shutil.which("git", path=str(minimal)):
                raise AssertionError("git reachable in the no-git lane")
            env["PATH"] = str(minimal)
        return subprocess.run(
            ["bash", str(root / ".gitlab" / "scripts" / SCRIPT.name)],
            capture_output=True,
            text=True,
            env=env,
            cwd=root,
            timeout=120,
        )

    def test_real_tree_passes(self):
        result = subprocess.run(
            ["bash", str(SCRIPT)], capture_output=True, text=True, cwd=ROOT, timeout=120
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("worker exposure policy guard: ok", result.stdout)

    def test_real_tree_passes_without_git(self):
        result = self.run_guard(ROOT, without_git=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("worker exposure policy guard: ok", result.stdout)

    def test_fixture_passes(self):
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_fixture_passes_without_git(self):
        result = self.run_guard(self.root, without_git=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_share_pin_fails(self):
        config = self.root / "opencode2.4lunarwing" / "config" / "opencode2.json"
        import json

        data = json.loads(config.read_text())
        del data["share"]
        config.write_text(json.dumps(data))
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn('must pin "share": "disabled"', result.stderr)

    def test_dockerfile_without_wrapper_install_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(
            dockerfile.read_text().replace(
                "RUN install -D -m 0755 /app/scripts/opencode-exposure-guard.sh /usr/local/bin/opencode",
                "RUN true",
            ),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("no longer installs the exposure-guard wrapper", result.stderr)

    def test_dockerfile_without_compile_out_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(
            dockerfile.read_text().replace("compile_out_remote_tunnel.py", "nothing.py"),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("compile-out", result.stderr)

    def test_dockerfile_agent_owned_scripts_fails(self):
        # run-79 EI-5: the controls must not live in agent-writable files.
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(
            dockerfile.read_text().replace(
                "COPY scripts/ ./scripts/",
                "COPY --chown=opencode2:opencode2 scripts/ ./scripts/",
            ),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("root-owned", result.stderr)

    def test_dockerfile_agent_owned_config_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(
            dockerfile.read_text().replace(
                "COPY config/ ./config/",
                "COPY --chown=opencode2:opencode2 config/ ./config/",
            ),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("root-owned", result.stderr)

    def test_dockerfile_without_readonly_chmod_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(
            dockerfile.read_text().replace(
                "RUN chmod -R a-w /app/scripts /app/config", "RUN true"
            ),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("read-only", result.stderr)

    def test_dockerfile_agent_owned_boot_files_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for owner in ("opencode2:opencode2", "10003:10003"):
            for name in ("entrypoint.sh", "health_server.py", "agent_comm_protocol.json"):
                with self.subTest(owner=owner, name=name):
                    dockerfile.write_text(original + f"COPY --chown={owner} {name} ./\n")
                    result = self.run_guard(self.root)
                    self.assertEqual(result.returncode, 1)
                    self.assertIn("boot files must be root-owned", result.stderr)

    def test_dockerfile_writable_boot_file_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for name in ("entrypoint.sh", "health_server.py", "agent_comm_protocol.json"):
            with self.subTest(name=name):
                dockerfile.write_text(original.replace(f" /app/{name}", ""))
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("read-only", result.stderr)

    def test_dockerfile_missing_filesystem_assertion_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(dockerfile.read_text().replace(" --image-filesystem", ""))
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("verify installed policy-file ownership", result.stderr)

    def test_dockerfile_compile_out_run_commented_out_fails(self):
        # Run-90 FC-4 G1: a deleted/commented compile-out RUN must fail even
        # though the script stays named in COPY lines and comments.
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for replacement in (
            "# RUN python3 /build/compile_out_remote_tunnel.py /build/opencode\n",
            "COPY scripts/compile_out_remote_tunnel.py /build/compile_out_remote_tunnel.py\n",
        ):
            with self.subTest(replacement=replacement.split(" ", 1)[0]):
                mutated = original.replace(
                    "RUN python3 /build/compile_out_remote_tunnel.py /build/opencode\n",
                    replacement,
                )
                # The name survives, so the old anywhere-grep would have passed.
                self.assertIn("compile_out_remote_tunnel.py", mutated)
                dockerfile.write_text(mutated, encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("lost the OpenTunnel compile-out RUN", result.stderr)
        dockerfile.write_text(original, encoding="utf-8")

    def test_dockerfile_binary_marker_scan_dropped_fails(self):
        # Run-90 FC-4 G5: dropping only the --binary scan used to pass because
        # the final --image-filesystem RUN still names exposure_policy_assert.py.
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        marker = ("    && python3 /app/scripts/exposure_policy_assert.py "
                  "--lane /app --profile opencode2 "
                  "--binary /usr/local/lib/opencode2/opencode\n")
        self.assertIn(marker, original)
        dockerfile.write_text(
            original.replace(marker, marker.replace(
                " --binary /usr/local/lib/opencode2/opencode", "")),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("binary marker scan", result.stderr)
        dockerfile.write_text(original, encoding="utf-8")

    def test_dockerfile_repermissioning_guarded_paths_fails(self):
        # Run-90 FC-4 G3b: no RUN may hand the guarded paths back to a
        # writable state after the final --image-filesystem assertion.
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for line in (
            "RUN chown -R opencode2:opencode2 /app/scripts /app/config\n",
            "RUN chmod u+w /app/entrypoint.sh\n",
            "RUN chmod -R a-w /app/scripts && chmod u+w /app/config\n",
        ):
            with self.subTest(line=line):
                dockerfile.write_text(original + line, encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("re-permission", result.stderr)
        dockerfile.write_text(original, encoding="utf-8")

    def test_dockerfile_changes_after_filesystem_assertion_fail(self):
        # Run-96 FP-1: classify instructions, not shell command spellings.
        # All these changes would escape the already-executed image check.
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for tail in (
            "COPY --chmod=0777 entrypoint.sh /app/entrypoint.sh\n",
            "COPY --chown=opencode2 entrypoint.sh /app/entrypoint.sh\n",
            "ADD --chown=opencode2 entrypoint.sh /app/entrypoint.sh\n",
            "copy --chmod=0777 entrypoint.sh /app/entrypoint.sh\n",
            "CoPy --chmod=0777 entrypoint.sh /app/entrypoint.sh\n",
            "COPY --chmod=0777 \\\n    entrypoint.sh /app/entrypoint.sh\n",
            'COPY ["entrypoint.sh", "/app/entrypoint.sh"]\n',
            "COPY --from=writable /app/entrypoint.sh /app/entrypoint.sh\n",
            'RUN ["chmod", "a+w", "/app/entrypoint.sh"]\n',
            "RUN /bin/chmod a+w /app/entrypoint.sh\n",
            "RUN install -o 10003 entrypoint.sh /app/entrypoint.sh\n",
            "RUN ln -sf /home/opencode2/entrypoint.sh /app/entrypoint.sh\n",
            "RUN cp -p /home/opencode2/entrypoint.sh /app/entrypoint.sh\n",
            "RUN setfacl -m u:10003:rw /app/entrypoint.sh\n",
            "WORKDIR /app\nRUN chmod a+w entrypoint.sh\n",
            "RUN chown -R opencode2 /app\n",
            "RUN chmod -R a+w /app//scripts\n",
            "RUN find /app -type f -exec chmod a+w {} +\n",
            'RUN path=/app/entrypoint.sh; chmod a+w "$path"\n',
            "RUN chmod +x /app/entrypoint.sh /app/health_server.py && chown -R opencode2 /app/scripts\n",
            'VOLUME ["/app"]\n',
            "ONBUILD RUN chmod a+w /app/entrypoint.sh\n",
            "ENV MUTABLE_PATH=/app\n",
            "ARG MUTABLE_PATH=/app\n",
            'SHELL ["/bin/sh", "-c"]\n',
            "FROM example.invalid/bun AS unchecked\n",
        ):
            with self.subTest(tail=tail):
                # A dead stage with agent-writable content for COPY --from.
                planted_stage = (
                    "FROM example.invalid/bun AS writable\nWORKDIR /app\n"
                    "COPY entrypoint.sh ./\nRUN chmod a+w entrypoint.sh\n"
                )
                dockerfile.write_text(planted_stage + original + tail, encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("after --image-filesystem", result.stderr)

    def test_dockerfile_metadata_after_filesystem_assertion_passes(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(
            dockerfile.read_text()
            + '\n# Mixed-case metadata and continued instructions are allowed.\n'
            + 'eXpOsE 8443\nhealthcheck --interval=30s \\\n    CMD curl -sf http://127.0.0.1:8443/health || exit 1\n'
            + 'user 10003:10003\nentrypoint ["/app/entrypoint.sh"]\n'
            + 'cmd ["--mode", "websocket"]\nLABEL policy="WI357"\nstopsignal SIGTERM\n',
            encoding="utf-8",
        )
        result = self.run_guard(self.root, without_git=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_dockerfile_compile_out_must_precede_builder_build(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        without_patch = original.replace(COMPILE_OUT_RUN, "")
        for name, mutated in {
            "after-build": without_patch.replace(BUILD_RUN, BUILD_RUN + COMPILE_OUT_RUN),
            "unused-stage": (
                "FROM example.invalid/bun AS unused\n"
                + COMPILE_OUT_COPY + COMPILE_OUT_RUN + without_patch
            ),
            "runtime-stage": without_patch.replace(READONLY_RUN, COMPILE_OUT_RUN + READONLY_RUN),
            "missing-build": original.replace(BUILD_RUN, ""),
        }.items():
            with self.subTest(name=name):
                dockerfile.write_text(mutated, encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("compile-out", result.stderr)

    def test_dockerfile_compile_out_requires_exact_source_copy(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for name, mutated in {
            "commented-copy": original.replace(COMPILE_OUT_COPY, "# " + COMPILE_OUT_COPY),
            "wrong-source": original.replace("COPY scripts/compile_out_remote_tunnel.py", "COPY other.py"),
            "dot-is-literal": original.replace("COPY scripts/compile_out_remote_tunnel.py", "COPY scripts/compile_out_remote_tunnelXpy"),
            "copy-after-run": original.replace(COMPILE_OUT_COPY + COMPILE_OUT_RUN, COMPILE_OUT_RUN + COMPILE_OUT_COPY),
        }.items():
            with self.subTest(name=name):
                dockerfile.write_text(mutated, encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("compile-out", result.stderr)

    def test_dockerfile_binary_scan_cannot_mask_failure(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        marker = "--binary /usr/local/lib/opencode2/opencode\n"
        for suffix in (" || true", " ; true", " \\\n    || true"):
            with self.subTest(suffix=suffix):
                dockerfile.write_text(original.replace(marker, marker.rstrip() + suffix + "\n"))
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("binary marker scan", result.stderr)

    def test_dockerfile_readonly_step_must_be_exact(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        exec_prefix = "RUN chmod +x /app/entrypoint.sh /app/health_server.py && "
        for replacement in (
            READONLY_RUN.rstrip() + " && chown -R opencode2 /app/scripts\n",
            exec_prefix + "chown -R opencode2 /app/scripts\n",
            exec_prefix + READONLY_RUN.removeprefix("RUN ").rstrip() + " && chmod u+w /app/config\n",
            READONLY_RUN.replace("entrypoint.sh", "entrypointXsh"),
        ):
            with self.subTest(replacement=replacement):
                dockerfile.write_text(original.replace(READONLY_RUN, replacement), encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1)
                self.assertIn("read-only", result.stderr)

    def test_dockerfile_binary_scan_must_share_the_assert_stage(self):
        # Run-97 FQ-1: BuildKit and buildah skip unused stages, so the exact
        # wrapper-install + marker-scan line in a dead (or wrong) stage
        # satisfies the presence check while the shipped image has neither
        # the wrapper (control 2) nor the marker scan (control 5). The runtime
        # stage then restores a plain binary at the wrapper path.
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        self.assertIn(BINARY_SCAN_RUN, original)
        replaced = original.replace(
            BINARY_SCAN_RUN,
            "RUN cp /usr/local/lib/opencode2/opencode /usr/local/bin/opencode\n",
        )
        for name, mutated in {
            "dead-stage": "FROM example.invalid/bun AS deadscan\n" + BINARY_SCAN_RUN + replaced,
            "builder-stage": replaced.replace(
                "FROM example.invalid/bun AS builder\n",
                "FROM example.invalid/bun AS builder\n" + BINARY_SCAN_RUN,
            ),
        }.items():
            with self.subTest(name=name):
                dockerfile.write_text(mutated, encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("same stage", result.stderr)
        dockerfile.write_text(original, encoding="utf-8")

    def test_dockerfile_last_user_must_be_agent_uid(self):
        # Run-97 FQ-2: a USER root tail (or a missing group) would void the
        # root-owned read-only controls outside mt-admin-managed deployments.
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for name, mutated in {
            "root-user": original + "USER root\n",
            "numeric-root": original + "USER 0:0\n",
            "no-group": original.replace("USER 10003:10003", "USER 10003"),
        }.items():
            with self.subTest(name=name):
                dockerfile.write_text(mutated, encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("last USER", result.stderr)
        dockerfile.write_text(original, encoding="utf-8")

    def test_dockerfile_parser_directive_comments_fail(self):
        # Run-97 FQ-3: with escape=` Docker treats a trailing backslash as a
        # literal, so a LABEL line ending in "\" followed by RUN after the
        # final assertion passes the guard's continuation joining while Docker
        # executes the RUN. Refuse the directives instead of emulating the
        # parser grammar. Run-103 GD-1: the refusal must also cover the
        # grammar's whitespace-around-'=' spellings and a UTF-8 BOM prefix
        # (Docker strips the BOM before parsing, so both hide a directive the
        # old regex never matched).
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        tail = "LABEL hidden=yes \\\nRUN chown -R opencode2:opencode2 /app\n"
        for directive in (
            "# escape=`\n",
            "# SyNtAx=docker/dockerfile:1\n",
            "  # check=true\n",
            "# escape = `\n",
            "\ufeff# escape=`\n",
        ):
            with self.subTest(directive=directive.strip()):
                dockerfile.write_text(directive + original + tail, encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("parser directive", result.stderr)
        # Run-106 GJ-1: BuildKit trims leading Unicode whitespace (Go
        # unicode.IsSpace) before directive detection, but the guard's
        # [[:space:]] trim is ASCII-only — the directive behind such a
        # character is invisible here while the builder honours it. The
        # guard now refuses any non-empty line whose first byte is not
        # printable ASCII.
        for prefix in ("\u00a0", "\u0085", "\u202f"):
            with self.subTest(unicode_prefix=hex(ord(prefix))):
                dockerfile.write_text(
                    prefix + "# escape=`\n" + original + tail, encoding="utf-8"
                )
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("printable ASCII", result.stderr)
        dockerfile.write_text(original, encoding="utf-8")

    def test_dockerfile_escaped_trailing_backslash_fails(self):
        # Run-106 GJ-2: BuildKit does not continue a line ending in two
        # backslashes (an escaped escape token), but the guard joined any
        # backslash-final line — so `LABEL hidden=yes \\` followed by a
        # post-assert root RUN looked like one harmless LABEL here while
        # BuildKit executed the RUN (buildah/podman joins the line, so the
        # guard matched local podman builds but not the CI builder).
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for name, tail in {
            "two-backslashes": "LABEL hidden=yes \\\\\nRUN chown -R opencode2:opencode2 /app\n",
            "trailing-space": "LABEL hidden=yes \\\\ \nRUN chown -R opencode2:opencode2 /app\n",
        }.items():
            with self.subTest(name=name):
                dockerfile.write_text(
                    original.replace(FILESYSTEM_ASSERTION, FILESYSTEM_ASSERTION + tail),
                    encoding="utf-8",
                )
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("escaped backslash", result.stderr)
        dockerfile.write_text(original, encoding="utf-8")

    def test_dockerfile_whitespace_after_continuation_fails(self):
        # GW-2: BuildKit joins backslash + space/tab/CRLF, swallowing the
        # required assert RUN into the preceding instruction. The old guard
        # counted the physical RUN anyway, so this was a false pass.
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for prefix in (
            "ENV LUNARWING_STAGE=final \\\u0020\n",
            "ENV LUNARWING_STAGE=final \\\r\n",
            "RUN echo prepared \\\t\n",
        ):
            with self.subTest(prefix=prefix):
                dockerfile.write_text(original.replace(
                    FILESYSTEM_ASSERTION, prefix + FILESYSTEM_ASSERTION
                ), encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("continuation backslash must not have trailing whitespace", result.stderr)

    def test_dockerfile_heredocs_cannot_swallow_assertion(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for prefix, terminator in (
            ("RUN cat >/dev/null <<LABEL\n", "LABEL\n"),
            ("RUN cat >/dev/null <<USER\n", "USER\n"),
            ("RUN cat >/dev/null <<-'LABEL'\n", "LABEL\n"),
            ("COPY <<LABEL /ignored\n", "LABEL\n"),
            ("ADD <<LABEL /ignored\n", "LABEL\n"),
            ("RUN cat >/dev/null <\\\n<LABEL\n", "LABEL\n"),
        ):
            with self.subTest(prefix=prefix):
                dockerfile.write_text(original.replace(
                    FILESYSTEM_ASSERTION, prefix + FILESYSTEM_ASSERTION + terminator
                ), encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("heredocs are not allowed", result.stderr)

    def test_dockerfile_shell_override_fails(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for shell in (
            'SHELL ["/bin/sh", "-c"]\n',
            'sHeLl ["/usr/bin/env", "-u", "LUNARWING_REAL_OPENCODE", "/bin/sh", "-c"]\n',
        ):
            with self.subTest(shell=shell):
                dockerfile.write_text(original.replace(
                    FILESYSTEM_ASSERTION, shell + FILESYSTEM_ASSERTION
                ), encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("SHELL overrides are not allowed", result.stderr)

    def test_dockerfile_policy_variable_overrides_fail(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for name, value in (
            ("LUNARWING_WRAPPER_SHA256", "0" * 64),
            ("LUNARWING_REAL_OPENCODE", "/tmp/other-opencode"),
            ("OPENCODE_BIN", "/usr/local/lib/opencode2/opencode"),
        ):
            for prefix in ("ENV", "ARG", "RUN export"):
                with self.subTest(name=name, prefix=prefix):
                    dockerfile.write_text(original.replace(
                        FILESYSTEM_ASSERTION,
                        f"{prefix} {name}={value}\n" + FILESYSTEM_ASSERTION,
                    ), encoding="utf-8")
                    result = self.run_guard(self.root)
                    self.assertEqual(result.returncode, 1, result.stdout)
                    self.assertIn("must not override exposure-policy variables", result.stderr)

    def test_dockerfile_expanded_and_legacy_env_keys_fail(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        for env in (
            "ARG P=LUNARWING_WRAPPER_\nENV ${P}SHA256=tampered\n",
            "ARG P=LUNARWING_REAL_\nENV SAFE=value ${P}OPENCODE=/tmp/other\n",
            "ARG P=OPENCODE_\nENV ${P}BIN=/tmp/other\n",
            'ARG P=OPENCODE_\nENV "${P}BIN"=/tmp/other\n',
            'ENV OPENCODE_"BIN"=/tmp/other\n',
            "ENV STAGE final\n",
            "ENV STAGE final=other\n",
        ):
            with self.subTest(env=env):
                dockerfile.write_text(original.replace(
                    FILESYSTEM_ASSERTION, env + FILESYSTEM_ASSERTION
                ), encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("ENV requires literal, unprotected key=value assignments", result.stderr)

    def test_dockerfile_literal_env_keys_allow_expanded_and_quoted_values(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        dockerfile.write_text(dockerfile.read_text().replace(
            FILESYSTEM_ASSERTION,
            'ENV PATH=/opt/bin:$PATH NOTE="two words" OTHER="${BASE:-default}"\n'
            + FILESYSTEM_ASSERTION,
        ), encoding="utf-8")
        result = self.run_guard(self.root, without_git=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_dockerfile_wrapper_anchor_arg_must_be_in_assertion_stage(self):
        dockerfile = self.root / "opencode2.4lunarwing" / "Dockerfile"
        original = dockerfile.read_text()
        without_anchor = original.replace(WRAPPER_ANCHOR_ARG, "")
        for name, mutated in {
            "missing": without_anchor,
            "commented": original.replace(WRAPPER_ANCHOR_ARG, "# " + WRAPPER_ANCHOR_ARG),
            "global": WRAPPER_ANCHOR_ARG + without_anchor,
            "builder-only": without_anchor.replace(COMPILE_OUT_COPY, WRAPPER_ANCHOR_ARG + COMPILE_OUT_COPY),
        }.items():
            with self.subTest(name=name):
                dockerfile.write_text(mutated, encoding="utf-8")
                result = self.run_guard(self.root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("must declare ARG LUNARWING_WRAPPER_SHA256=", result.stderr)

    def test_executor_missing_deny_fails(self):
        executor = self.root / "opencode2.4lunarwing" / "scripts" / "opencode_task_executor.ts"
        executor.write_text(
            EXECUTOR_RULESET.replace(
                '    { action: "shell", resource: "*opencode pair *", effect: "deny" },\n', ""
            ),
            encoding="utf-8",
        )
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("lost the shell deny for 'opencode pair'", result.stderr)

    def test_executor_deny_before_allow_fails(self):
        executor = self.root / "opencode2.4lunarwing" / "scripts" / "opencode_task_executor.ts"
        lines = EXECUTOR_RULESET.splitlines(keepends=True)
        allow = next(line for line in lines if '"shell", resource: "*"' in line)
        denies = [line for line in lines if "opencode" in line and "deny" in line]
        reordered = "\n".join(denies + [allow]) + "\n"
        executor.write_text(reordered, encoding="utf-8")
        result = self.run_guard(self.root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("ordering regressed", result.stderr)


if __name__ == "__main__":
    unittest.main()
